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

use std::sync::Arc;

use unlit_wgpu::readback::readback_texture;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3dx::prelude::*;

use crate::scene::{
    AudioSetup, HEADLESS_HZ, PanelSlots, SceneStepper, mount_scene_content, setup_render_world,
    spawn_scene,
};

/// The audio sample rate the headless run renders at, in hertz.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// How many channels the headless run mixes: the interleaved samples the
/// audio world's fill dispatch writes, and the stream the encoder reads.
pub const AUDIO_CHANNELS: usize = 2;

/// The headless run's settings.
pub struct HeadlessConfig {
    /// The frame size, in pixels.
    pub size: (u32, u32),
    /// How many frames to step before the run ends.
    pub frames: u32,
    /// Which frames to capture, by index, ascending.
    /// An empty list makes the run audio-only: the render world still drains
    /// its commands and dispatches its frames, but none is drawn.
    ///
    /// The frames come back in the order they were stepped. Every index must
    /// name a frame the run steps and each must follow the one before, or
    /// [`run`] panics: an index past the last frame matches nothing, and a
    /// list that went backwards would hand its frames back in an order the
    /// caller did not name them in.
    pub capture: Vec<u32>,
    /// The rate the mix is pulled at, and the rate the recorded audio is
    /// written at. The synthesizer takes it from the stream it renders into.
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
    // Every capture entry has to name a frame the run steps, and the entries
    // have to ascend: a frame is drawn where the steps left it, so the frames
    // come back in the order they were stepped. An index past the last frame
    // matches nothing, and a list that went backwards would answer in its own
    // order rather than the caller's.
    for (position, index) in config.capture.iter().enumerate() {
        assert!(
            *index < config.frames,
            "capture entry {position} names frame {index}, but the run steps {} frames",
            config.frames
        );
        if position > 0 {
            assert!(
                config.capture[position - 1] < *index,
                "capture entry {position} names frame {index} after frame {}, so the capture does not ascend",
                config.capture[position - 1]
            );
        }
    }

    let mut host = World::new();

    // The render sub-world, drained inline every frame, exactly as the
    // windowed mode's draw behaviour drains it.
    let render = spawn_sub_world(&mut host, "render", 64);

    // The scene's worlds, stepped by this run: the same worlds, queues and
    // mailboxes the windowed mode builds on threads, so both modes share
    // one scene graph. The synthesizer is the same factory the audio thread
    // builds, so the headless run's samples match the windowed one's by
    // construction; a stepped world has no device, so its voice builds on the
    // first fill this run pulls, at the rate those fills name.
    // An empty soundfont — the build embeds a stub when the asset is absent —
    // is not a synthesizer to build: the world stays silent instead of
    // failing to load it on the first tick.
    let audio_setup = if config.soundfont.is_empty() {
        AudioSetup::Silent
    } else {
        AudioSetup::Pulled {
            soundfont: config.soundfont.clone(),
            autoplay_background: true,
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
        true,
    );
    let (renderer, pass) =
        setup_render_world(&mut host, render.entity, device, queue, capabilities);
    // No control panel headless: nothing is there to click. The camera is
    // still the scene's, so it arrives as a job like the meshes do.
    mount_scene_content(&render, pass, None);
    let offscreen = {
        let nested = host
            .root_scope()
            .query_one::<&mut World>(render.entity)
            .expect("the render sub-world exists");
        let context = nested
            .root_scope()
            .query_one::<&Renderer>(renderer.0)
            .expect("the renderer is a resource entity")
            .context();
        bind_offscreen(&nested, context, pass.0, device, config.size)
    };

    let mut output = HeadlessOutput {
        frames: Vec::new(),
        audio: Vec::new(),
    };
    let mut frame = Frame {
        delta_time: (1.0 / HEADLESS_HZ) as f32,
        index: 0,
        size: config.size,
    };
    // The stepper carries the step remainders: physics runs at PHYSICS_HZ
    // and the synthesizer at its own sample rate, neither necessarily an
    // integer multiple of the frame rate, so both accumulate their
    // fractional steps across frames. The frame's duration is the exact
    // step the run asks for, not a measured one, so the same frames give the
    // same steps every run.
    let mut stepper = SceneStepper::new(PHYSICS_HZ, Some(config.audio_sample_rate));
    // The interleaved sample buffer, reused across frames so the audio
    // loop does not allocate one per frame.
    let mut samples_out: Vec<f32> = Vec::new();

    for index in 0..config.frames {
        frame.index = index;

        // The worlds step once for this frame: gameplay first, so the notes
        // its sequences play reach the audio world before it steps, then
        // physics as its rate comes due and audio for the jobs booked.
        stepper.step(&host, &worlds, 1.0 / HEADLESS_HZ);

        // Audio: render whatever samples the frame's duration asks for.
        let frames = stepper.audio_frames(1.0 / HEADLESS_HZ);
        if frames > 0 {
            samples_out.clear();
            samples_out.resize(frames * AUDIO_CHANNELS, 0.0);
            let filled = with_stepped_world(&host, worlds.audio.entity, |audio| {
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
        // on that drain — then dispatch the frame. The dispatch happens every
        // frame, an audio-only run included: it is what advances the frame's
        // behaviours, so the path a capture freezes is the path the run walked.
        // Only the frames a capture names are drawn.
        //
        // Drawing every captured frame is enough because a draw does not
        // advance the scene: it reads what the steps left behind, and the state
        // it writes is only what a frame publishes for its readers — the
        // target a panel lays itself out against and the URLs its widgets
        // opened — which the next frame overwrites anyway. So the frame drawn
        // for a capture is the frame that would have been drawn on time. A
        // scene that accumulates across draws — a ping-pong target, a temporal
        // filter — would have to draw every frame.
        let mut nested = host
            .root_scope()
            .query_one::<&mut World>(render.entity)
            .expect("the render sub-world exists");
        drain_sub_world(&mut nested);
        dispatch_frame(&mut nested, &mut frame);
        if config.capture.contains(&index) {
            let _ = nested
                .root_scope()
                .query_one::<&mut Renderer>(renderer.0)
                .map(|mut r| {
                    r.render(&nested);
                });
            let target = offscreen.as_ref().expect("the offscreen target exists");
            output
                .frames
                .push(readback_texture(device, queue, &target.texture));
        }
    }

    output
}
