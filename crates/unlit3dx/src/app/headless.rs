//! The headless run: the scene, stepped inline without a window or threads.
//!
//! Every world lives in one place and the run drives them in a fixed order —
//! gameplay ticks, physics steps as its rate comes due, the audio world fills
//! the mix the frame asks for, then the render world drains its commands and
//! draws every frame. With every world stepped at a fixed
//! rate, a run is reproducible down to the captured frames and audio samples.

use unlit_ecs::World;
use unlit_wgpu::context::RenderDevice;
use unlit3d::frame::Frame;
use unlit3d::input::{InputFeed, InputWriter, refresh_input};

use super::App;
use super::capture::Capture;
use super::offscreen;
use super::pace::Pacing;
use super::scene::{Mode, RenderPorts, RunFrames, ScenePorts, Stepping};
use crate::audio::dispatch_audio_fill;
use crate::host::{spawn_sub_world, tick_sub_world};
use crate::world::{StepOutcome, Stepped, with_stepped_world_mut};

/// The audio sample rate a run renders at when its configuration names no
/// other, in hertz.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// How many channels a run mixes: the interleaved samples the audio world's
/// fill dispatch writes, and the stream an encoder reads.
pub const AUDIO_CHANNELS: usize = 2;

/// The frame rate a run without a display steps at, in hertz.
///
/// Nothing measures it: it is the exact step each frame asks for, which is
/// what makes a run reproducible.
pub const HEADLESS_HZ: f64 = 60.0;

/// The capacity a render sub-world's mailbox gets when a configuration names
/// no other.
pub const RENDER_COMMAND_CAPACITY: usize = 64;

/// How much of an unbounded run's mix is kept, in seconds.
///
/// A run with no length of its own plays until the process is killed, and a
/// mix kept for that whole time would grow without end. This bounds what a
/// capture writes and what a served run records; a run whose length is named
/// is bounded by that length instead.
pub const UNBOUNDED_MIX_SECONDS: f32 = 600.0;

/// A headless run's settings.
pub struct HeadlessConfig {
    /// The frame size, in pixels.
    pub size: (u32, u32),
    /// Who advances the frames, and the most it advances.
    ///
    /// A headless run advances them itself, so [`Pacing::client`] is
    /// ignored: the struct is the same one a windowed run reads, so what it
    /// names means the same thing on either path.
    pub pacing: Pacing,
    /// Which frames to keep, and where to write them.
    ///
    /// Every frame is drawn; only the named frames are read back.
    pub capture: Capture,
    /// The rate the mix is pulled at. The synthesizer takes it from the stream
    /// it renders into.
    pub audio_sample_rate: u32,
    /// The render sub-world's mailbox capacity.
    pub render_capacity: usize,
}

impl Default for HeadlessConfig {
    fn default() -> Self {
        Self {
            size: (1024, 720),
            pacing: Pacing::default(),
            capture: Capture::default(),
            audio_sample_rate: AUDIO_SAMPLE_RATE,
            render_capacity: RENDER_COMMAND_CAPACITY,
        }
    }
}

/// What one headless run produced.
pub struct HeadlessOutput {
    /// The captured frames, each RGBA, row-major, top to bottom, with the
    /// index the run gave it. Ascending, because the run reaches them in
    /// that order.
    pub frames: Vec<(u32, Vec<u8>)>,
    /// The mixed audio, interleaved stereo.
    pub audio: Vec<f32>,
    /// How many frames the run advanced.
    pub advanced: u32,
}

/// Run an application's scene headless and answer the captured frames and mix.
///
/// The device is injected, so a caller picks the tier it asks for.
/// Already written captures remain available if a later frame fails.
///
/// A run bounded by nothing steps frames until the process is stopped, which
/// is why it reads as a capture the caller drives: the frames it keeps are
/// written as it reaches them, so a run cut short keeps the ones it got to.
pub async fn headless<A: App>(
    mut app: A,
    config: &HeadlessConfig,
    device: RenderDevice,
) -> Result<HeadlessOutput, String> {
    if config.audio_sample_rate == 0 {
        return Err("the audio sample rate must be positive".to_owned());
    }
    let mut host = World::new();
    let render = spawn_sub_world(&mut host, "render", config.render_capacity);
    // A run with no window draws no pointer of its own, but its client injects
    // events through `send_input`, so the host holds the writer the scene reads
    // and every sub-world subscribing to it keeps its own buffer.
    let input = InputWriter::new();
    host.root_scope_mut().spawn(InputFeed(input.clone()));
    let panel = unlit3d::relay::Snapshot::new();
    let worlds = app.spawn_scene::<Stepped>(
        &mut host,
        ScenePorts {
            render: &render,
            input: &input,
            panel: &panel,
            refresh_millihertz: Default::default(),
            mode: Mode::Headless,
        },
    );
    // No panel source: the capture holds the scene alone whatever the
    // gameplay world lays out. The camera and the objects arrive as the jobs
    // the scene queued, landing on this run's first drain of the render world.
    let setup = app.setup_render(
        &mut host,
        RenderPorts {
            world: render.entity,
            device: &device,
            panel: None,
        },
    );
    let (renderer, target) = {
        let mut nested = host
            .root_scope()
            .try_query_one::<&World>(render.entity)
            .map_err(|error| format!("the render world is unavailable: {error}"))?;
        offscreen::bind(nested.get(), setup, &device, config.size)?
    };

    let mut output = HeadlessOutput {
        frames: Vec::new(),
        audio: Vec::new(),
        advanced: 0,
    };
    let mut frame = Frame {
        delta_time: (1.0 / HEADLESS_HZ) as f32,
        index: 0,
    };
    // The stepper carries the step remainders: physics runs at its own rate
    // and the synthesizer at the sample rate, neither necessarily an integer
    // multiple of the frame rate, so both accumulate their fractional steps
    // across frames.
    let frames = RunFrames::default();
    let mut stepper = Stepping::new(worlds, A::PHYSICS_HZ, Some(config.audio_sample_rate));
    // The interleaved sample buffer, reused across frames so the audio loop
    // does not allocate one per frame.
    let mut samples_out: Vec<f32> = Vec::new();
    // An unbounded run keeps a bounded mix: see `UNBOUNDED_MIX_SECONDS`.
    let mix_limit = config.pacing.frames.map_or_else(
        || (UNBOUNDED_MIX_SECONDS * config.audio_sample_rate as f32) as usize * AUDIO_CHANNELS,
        |_| usize::MAX,
    );
    let mut mix_capped = false;

    loop {
        let index = frames.advanced();
        if config.pacing.is_spent(index) {
            break;
        }
        frame.index = index;

        // The host's own buffer is folded every frame so the events a client
        // injected are consumed rather than piling up: the scene's worlds
        // subscribed to the same writer, and each folds its own copy on its
        // own tick. The host has no behaviour reading it, so the result is
        // dropped.
        refresh_input(&host);

        // The worlds step once for this frame: gameplay first, so the notes
        // its sequences play reach the audio world before it steps, then
        // physics as its rate comes due and audio for the jobs booked.
        match stepper.step(&host, 1.0 / HEADLESS_HZ) {
            StepOutcome::Ran => {}
            StepOutcome::Finished | StepOutcome::Stopped => break,
            StepOutcome::Failed => return Err(format!("the scene failed before frame {index}")),
        }

        // Audio: render whatever samples the frame's duration asks for.
        let fill = stepper.audio_frames(1.0 / HEADLESS_HZ);
        if fill > 0 {
            samples_out.clear();
            samples_out.resize(fill * AUDIO_CHANNELS, 0.0);
            with_stepped_world_mut(&host, stepper.worlds().audio.entity, |audio| {
                dispatch_audio_fill(
                    audio,
                    &mut samples_out,
                    config.audio_sample_rate,
                    AUDIO_CHANNELS as u16,
                );
            })
            .map_err(|error| format!("the audio world could not fill the mix: {error}"))?;
            let room = mix_limit.saturating_sub(output.audio.len());
            if room < samples_out.len() && !mix_capped {
                mix_capped = true;
                log::warn!(
                    "the run has no length, so its mix stops at {UNBOUNDED_MIX_SECONDS} seconds; name --max-frames to record a longer one"
                );
            }
            let take = room.min(samples_out.len());
            output.audio.extend_from_slice(&samples_out[..take]);
        }

        // The render world advances and renders every frame. Readback remains
        // conditional, so an uncaptured frame still updates temporal targets.
        let texture = {
            let mut nested = host
                .root_scope()
                .try_query_one::<&mut World>(render.entity)
                .map_err(|error| format!("the render world is unavailable: {error}"))?;
            let world = nested.get();
            match tick_sub_world(world, &mut frame) {
                StepOutcome::Ran => {}
                StepOutcome::Finished | StepOutcome::Stopped => break,
                StepOutcome::Failed => {
                    return Err(format!("the render world failed before frame {index}"));
                }
            }
            offscreen::render(world, renderer)?;
            config
                .capture
                .wants(index)
                .then(|| offscreen::texture(world, renderer, &target.texture))
                .transpose()?
        };
        frames.advance();
        if let Some(texture) = texture {
            let (_, _, pixels) = offscreen::read(&device.device, &device.queue, &texture).await?;
            config.capture.write_frame(index, &pixels, config.size)?;
            output.frames.push((index, pixels));
        }
    }

    output.advanced = frames.advanced();
    Ok(output)
}
