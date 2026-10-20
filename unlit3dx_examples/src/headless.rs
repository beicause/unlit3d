//! The headless run: the whole scene, stepped inline without a window,
//! threads or audio output.
//!
//! The windowed mode spreads the worlds across threads: a render
//! sub-world on the main thread, remote physics, audio and gameplay
//! worlds on their own threads. The headless run keeps the same worlds
//! and the same job and snapshot plumbing, but every world lives in one
//! place and the run drives them in a fixed order: gameplay ticks,
//! physics drains its jobs and steps, the synthesizer renders the tracks,
//! then the render world drains its commands and draws. With
//! every world stepped at a fixed rate, a run is reproducible down to
//! the captured frames and audio samples.

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use std::path::Path;
use std::sync::Arc;

use unlit_wgpu::readback::readback_texture;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3dx::prelude::*;

use crate::scene::{
    AudioSetup, GAME_HZ, PanelSlots, RENDER_HZ, SceneStepper, mount_scene_content,
    setup_render_world, spawn_scene,
};

/// The audio sample rate the headless run renders at, in hertz.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// How many channels the headless run mixes: the interleaved samples the
/// audio world's fill dispatch writes, and the stream the encoder reads.
pub const AUDIO_CHANNELS: usize = 2;

/// The resolution the mix is quantized to, in bits per sample. Only the
/// FLAC encoder, which a browser cannot run, needs it.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const BITS_PER_SAMPLE: usize = i16::BITS as usize;

/// The headless run's settings.
pub struct HeadlessConfig {
    /// The frame size, in pixels.
    pub size: (u32, u32),
    /// How many frames to step before the run ends.
    pub frames: u32,
    /// Which frames to capture, by index. An empty list makes the run
    /// audio-only: the render world still drains its commands, but no
    /// frame is dispatched or drawn.
    pub capture: Vec<u32>,
    /// The audio sample rate the synthesizer renders at.
    pub audio_sample_rate: u32,
    /// The soundfont the synthesizer plays, as raw bytes: the demo
    /// passes the same embedded bytes the windowed mode builds with.
    pub soundfont: Arc<[u8]>,
}

/// What one headless run produced.
pub struct HeadlessOutput {
    /// The captured frames, each RGBA, row-major, top to bottom.
    pub frames: Vec<Vec<u8>>,
    /// The mixed audio, interleaved stereo.
    pub audio: Vec<f32>,
}

/// Run the scene headless and answer the captured frames and audio.
///
/// A headless run has no way to recover a world that failed to come up,
/// so a broken setup panics instead of returning a hole in the capture.
pub fn run(
    config: &HeadlessConfig,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    capabilities: DeviceCapabilities,
) -> HeadlessOutput {
    let mut host = World::new();

    // The render sub-world, drained inline every frame, exactly as the
    // windowed mode's draw behaviour drains it.
    let render = spawn_sub_world(&mut host, "render", 64);

    // The scene's worlds, stepped by this run: the same worlds, queues and
    // mailboxes the windowed mode builds on threads, so both modes share
    // one scene graph. The synthesizer is the same factory the audio thread
    // builds, so the headless run's samples match the windowed one's by
    // construction; a stepped world has no device, so its sources are
    // installed up front at the rate the run mixes at.
    // An empty soundfont — the build embeds a stub when the asset is absent —
    // is not a synthesizer to build: the world stays silent instead of
    // failing to load it on the first tick.
    let audio_setup = if config.soundfont.is_empty() {
        AudioSetup::Silent
    } else {
        AudioSetup::Pulled {
            soundfont: config.soundfont.clone(),
            sample_rate: config.audio_sample_rate,
        }
    };
    // Nothing feeds input to a run with no window, but the gameplay world
    // still takes a buffer: a panel lays its frames out from it, and a run
    // that never pushes an event simply never sees one.
    let input = InputWriter::new();
    let worlds = spawn_scene::<Stepped>(
        &mut host,
        render.clone(),
        audio_setup,
        PanelSlots::default(),
        &input,
    );
    let renderer = setup_render_world(&mut host, render.entity(), device, queue, capabilities);
    // No control panel headless: nothing is there to click. The camera is
    // still the scene's, so it arrives as a job like the meshes do.
    mount_scene_content(&render, None);
    let mut offscreen = None;
    {
        let nested = host
            .root_scope()
            .get_mut::<World>(render.entity())
            .expect("the render sub-world exists");
        let _ = nested.root_scope().with_mut::<Renderer, _>(renderer, |r| {
            offscreen = bind_offscreen(&nested, r, device, config.size);
        });
    }

    let mut output = HeadlessOutput {
        frames: Vec::new(),
        audio: Vec::new(),
    };
    let mut frame = Frame {
        delta_time: (1.0 / RENDER_HZ) as f32,
        index: 0,
        size: config.size,
    };
    // The stepper carries the step remainders: physics runs at 64 Hz and
    // the synthesizer at its own sample rate, neither an integer multiple
    // of the render rate, so both accumulate their fractional steps across
    // frames. The frame's duration is the exact render step, not a
    // measured one, so the same frames give the same steps every run.
    let mut stepper = SceneStepper::new(GAME_HZ, PHYSICS_HZ, Some(config.audio_sample_rate));
    // The interleaved sample buffer, reused across frames so the audio
    // loop does not allocate one per frame.
    let mut samples_out: Vec<f32> = Vec::new();

    for index in 0..config.frames {
        frame.index = index;

        // The worlds step once for this frame: gameplay first, so its
        // requests reach the others before they step, then physics as its
        // rate comes due and audio for the transport jobs booked.
        stepper.step(&host, &worlds, 1.0 / RENDER_HZ);

        // Audio: render whatever samples the frame's duration asks for.
        let frames = stepper.audio_frames(1.0 / RENDER_HZ);
        if frames > 0 {
            samples_out.clear();
            samples_out.resize(frames * AUDIO_CHANNELS, 0.0);
            let filled = with_stepped_world(&host, worlds.audio.entity(), |audio| {
                dispatch_audio_fill(
                    audio,
                    &mut samples_out,
                    config.audio_sample_rate,
                    AUDIO_CHANNELS as u16,
                );
            });
            assert!(filled.is_some(), "the audio world is a stepped world");
            output.audio.extend_from_slice(&samples_out);
        }

        // Render: drain the sub-world's commands — the scene build rides
        // on that drain — then dispatch and draw. An audio-only run
        // captures no frames and skips the draw entirely: nothing reads
        // the picture, and the camera behaviour it would drive belongs
        // to the picture alone.
        let mut nested = host
            .root_scope()
            .get_mut::<World>(render.entity())
            .expect("the render sub-world exists");
        drain_sub_world(&mut nested);
        if !config.capture.is_empty() {
            dispatch_frame(&mut nested, &mut frame);
            let _ = nested.root_scope().with_mut::<Renderer, _>(renderer, |r| {
                r.render(&nested);
            });
            if config.capture.contains(&index) {
                let color = offscreen.as_ref().expect("the offscreen target exists");
                output.frames.push(readback_texture(device, queue, color));
            }
        }
    }

    output
}

/// Write interleaved stereo samples as a lossless FLAC stream.
///
/// The samples clamp into the 16-bit range first, exactly as a PCM16 WAV
/// would, so the stream decodes back to the same samples it was written
/// from. FLAC is the smaller deliverable: the same ten-second mix takes a
/// third of the WAV's bytes, which is why the stored baselines are streams
/// rather than a raw header and samples.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub fn write_flac(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    std::fs::write(path, encode_flac(samples, sample_rate)?)
}

/// Quantize interleaved samples to 16 bits and encode them as FLAC.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn encode_flac(samples: &[f32], sample_rate: u32) -> std::io::Result<Vec<u8>> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let quantized: Vec<i32> = samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16 as i32)
        .collect();
    let config = flacenc::config::Encoder::default();
    let block_size = config.block_size;
    let verified = config
        .into_verified()
        .map_err(|(_, error)| std::io::Error::other(error.to_string()))?;
    let source = flacenc::source::MemSource::from_samples(
        &quantized,
        AUDIO_CHANNELS,
        BITS_PER_SAMPLE,
        sample_rate as usize,
    );
    let mut stream = flacenc::encode_with_fixed_block_size(&verified, source, block_size)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    // The trailing frame holds whatever samples the mix's length leaves over,
    // and the encoder lets that drag the stream's declared minimum block size
    // down. The reference encoder declares one block size for the whole
    // stream, and a decoder that trusts the declaration to seek cannot tell
    // the two apart, so say the same thing here.
    stream
        .stream_info_mut()
        .set_block_sizes(block_size, block_size)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(sink.into_inner())
}
