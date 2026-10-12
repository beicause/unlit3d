//! The headless run: the whole scene, stepped inline without a window,
//! threads or audio output.
//!
//! The windowed mode spreads the worlds across threads: a render
//! sub-world on the main thread, remote physics, audio and gameplay
//! worlds on their own threads. The headless run keeps the same worlds
//! and the same job and snapshot plumbing, but every world lives in one
//! place and the run drives them in a fixed order: gameplay ticks,
//! physics drains its jobs and steps, the synthesizer renders the notes'
//! samples, then the render world drains its commands and draws. With
//! every world stepped at a fixed rate, a run is reproducible down to
//! the captured frames and audio samples.

use std::path::{Path, PathBuf};

use crossbeam_channel::{Sender, unbounded};
use unlit_wgpu::readback::readback_texture;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3dx::prelude::*;

use crate::scene::{
    GAMEPLAY_TICKS_PER_FRAME, GROUND_HALF, GROUND_POSITION, Gameplay, RENDER_HZ, SYNTH_GAIN,
    bind_offscreen, setup_render_world,
};

/// The audio sample rate the headless run renders at, in hertz.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

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
    /// The soundfont the synthesizer plays.
    pub soundfont: PathBuf,
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
    let render = spawn_sub_world(&mut host, 64);
    let (frame_writer, frame_reader) = snapshot_pair::<TransformFrame>();
    let renderer = setup_render_world(&mut host, render, device, queue, capabilities, frame_reader);
    let mut offscreen = None;
    {
        let nested = host
            .get_mut::<World>(render)
            .expect("the render sub-world exists");
        let _ = nested.with_mut::<Renderer, _>(renderer, |r| {
            offscreen = Some(bind_offscreen(&nested, r, device, config.size));
        });
    }

    // The physics world: no thread, just the rapier world and the job
    // queue gameplay drops balls into, with the ground in place before
    // gameplay's first tick.
    let mut physics_world = World::new();
    physics_world.spawn((unlit3dx::physics::PhysicsWorld::new(),));
    let (physics_sender, physics_receiver): (Sender<Job<()>>, _) = unbounded();
    let (physics_writer, physics_reader) = snapshot_pair::<PhysicsSnapshot>();
    physics_sender
        .send(Box::new(|world: &mut World| {
            spawn_fixed_cuboid(world, GROUND_POSITION, GROUND_HALF, 0.9);
        }))
        .expect("the physics job queue takes the ground job");

    // The audio world: no thread and no output stream, just the
    // synthesizer and the job queue the gameplay books notes into.
    let mut audio_world = World::new();
    let (audio_sender, audio_receiver) = unbounded();
    SynthVoice::spawn(
        &mut audio_world,
        &config.soundfont,
        config.audio_sample_rate,
    )
    .expect("the headless run's soundfont loads");

    // The gameplay world, ticked inline at the gameplay rate.
    let mut gameplay_world = World::new();
    gameplay_world.spawn((Gameplay::new(
        frame_writer,
        physics_reader,
        physics_sender,
        audio_sender.clone(),
        host.get::<SubWorld>(render)
            .expect("the render sub-world carries its proxy")
            .clone(),
    ),));

    let mut output = HeadlessOutput {
        frames: Vec::new(),
        audio: Vec::new(),
    };
    let mut frame = Frame {
        delta_time: (1.0 / RENDER_HZ) as f32,
        index: 0,
        size: config.size,
    };
    // Step remainders: physics runs at 64 Hz and the synthesizer at its
    // own sample rate, neither an integer multiple of the render rate, so
    // both accumulate their fractional steps across frames.
    let mut physics_debt = 0.0f64;
    let mut audio_debt = 0.0f64;
    let frame_seconds = 1.0 / RENDER_HZ;
    let physics_period = 1.0 / PHYSICS_HZ;
    let sample_period = 1.0 / config.audio_sample_rate as f64;
    // The sample buffers, reused across frames so the audio loop does not
    // allocate one per frame.
    let (mut left, mut right): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());

    for index in 0..config.frames {
        frame.index = index;

        // Gameplay ticks first: its requests reach the other worlds
        // before they step.
        for _ in 0..GAMEPLAY_TICKS_PER_FRAME {
            if let Some((_, mut game)) = gameplay_world.query::<&mut Gameplay>().next() {
                game.tick();
            }
        }

        // Physics: drain the jobs gameplay queued, then step and publish.
        for job in physics_receiver.try_iter() {
            job(&mut physics_world);
        }
        physics_debt += frame_seconds;
        while physics_debt >= physics_period {
            if let Some(snapshot) = step_once(&physics_world) {
                physics_writer.publish(snapshot);
            }
            physics_debt -= physics_period;
        }

        // Audio: drain the notes gameplay booked, then render whatever
        // samples the frame's duration asks for.
        for job in audio_receiver.try_iter() {
            job(&mut audio_world);
        }
        audio_debt += frame_seconds;
        let samples = (audio_debt / sample_period) as usize;
        if samples > 0 {
            audio_debt -= samples as f64 * sample_period;
            left.clear();
            left.resize(samples, 0.0);
            right.clear();
            right.resize(samples, 0.0);
            for (_, mut voice) in audio_world.query::<&mut SynthVoice>() {
                voice.synthesizer.render(&mut left, &mut right);
            }
            // The same gain the audio thread's mixer applies, so the
            // headless mix matches the windowed one. The channels
            // interleave here, sample frame by sample frame: the WAV
            // writer downstream reads one left and one right sample per
            // frame, not two half-file channel planes.
            output.audio.extend(
                left.iter()
                    .zip(&right)
                    .flat_map(|(left, right)| [left, right])
                    .map(|sample| (*sample * SYNTH_GAIN).clamp(-1.0, 1.0)),
            );
        }

        // Render: drain the sub-world's commands — the scene build rides
        // on that drain — then dispatch and draw. An audio-only run
        // captures no frames and skips the draw entirely: nothing reads
        // the picture, and the camera behaviour it would drive belongs
        // to the picture alone.
        let mut nested = host
            .get_mut::<World>(render)
            .expect("the render sub-world exists");
        drain_sub_world(&mut nested);
        if !config.capture.is_empty() {
            dispatch_frame(&mut nested, &mut frame);
            let _ = nested.with_mut::<Renderer, _>(renderer, |r| {
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

/// Write interleaved stereo samples as a PCM16 WAV file.
///
/// The samples clamp into the 16-bit range; the header is the smallest
/// canonical one a decoder accepts.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    let sample_bytes = size_of::<i16>();
    let data_len = samples.len() * sample_bytes;
    let mut file = Vec::with_capacity(44 + data_len);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    file.extend_from_slice(b"WAVE");
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&2u16.to_le_bytes());
    file.extend_from_slice(&sample_rate.to_le_bytes());
    file.extend_from_slice(&(sample_rate * 2 * sample_bytes as u32).to_le_bytes());
    file.extend_from_slice(&(2 * sample_bytes as u16).to_le_bytes());
    file.extend_from_slice(&16u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        let quantized = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        file.extend_from_slice(&quantized.to_le_bytes());
    }
    std::fs::write(path, file)
}
