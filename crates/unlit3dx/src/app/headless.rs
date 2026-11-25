//! The headless run: the scene, stepped inline without a window or threads.
//!
//! Every world lives in one place and the run drives them in a fixed order —
//! gameplay ticks, physics steps as its rate comes due, the audio world fills
//! the mix the frame asks for, then the render world drains its commands and
//! draws the frames the capture names. With every world stepped at a fixed
//! rate, a run is reproducible down to the captured frames and audio samples.

use unlit_ecs::World;
use unlit_wgpu::context::RenderDevice;
use unlit3d::input::{InputFeed, InputWriter};

use super::App;
use super::capture::Capture;
use super::error::RunError;
use super::offscreen;
use super::pace::Pacing;
use super::scene::{Mode, RenderPorts, RunFrames, ScenePorts};
use super::stepped::SteppedRun;
use crate::host::spawn_sub_world;
use crate::world::{StepOutcome, Stepped};

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

/// The duration of one frame a run without a display steps at, in seconds:
/// the single source of the fixed step every such run hands its frames.
pub const FIXED_FRAME_SECONDS: f64 = 1.0 / HEADLESS_HZ;

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
    /// The run draws a frame only when the capture names it, so a capture that
    /// names nothing leaves the run audio-only: the render world still drains
    /// its commands and dispatches its frames, but none is drawn.
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
) -> Result<HeadlessOutput, RunError> {
    if config.audio_sample_rate == 0 {
        return Err(RunError::AudioRate);
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
            .map_err(RunError::RenderWorld)?;
        offscreen::bind(nested.get(), setup, &device, config.size)?
    };

    let mut output = HeadlessOutput {
        frames: Vec::new(),
        audio: Vec::new(),
        advanced: 0,
    };
    // The run keeps the physics remainder and the mix's across frames; see
    // `SteppedRun`.
    let mut run = SteppedRun::new(
        worlds,
        A::PHYSICS_HZ,
        RunFrames::default(),
        render.entity,
        renderer,
        config.audio_sample_rate,
    );
    // Every run keeps a bounded mix: as long as the run itself when it names a
    // length, at most `UNBOUNDED_MIX_SECONDS` when it plays until killed. The
    // bound is whole interleaved frames, so a cut never lands mid-frame.
    let mix_seconds = config
        .pacing
        .frames
        .map_or(UNBOUNDED_MIX_SECONDS, |frames| {
            frames as f32 * FIXED_FRAME_SECONDS as f32
        });
    let mix_limit = (mix_seconds * config.audio_sample_rate as f32) as usize * AUDIO_CHANNELS;
    let mut mix_capped = false;

    loop {
        let index = run.frames.advanced();
        if config.pacing.is_spent(index) {
            break;
        }
        // The capture is what decides whether this frame is drawn: a drawn
        // frame nobody reads is a wasted draw, and a frame the capture names
        // and the run does not draw is a picture missing from it.
        let draw = config.capture.wants(index);
        match run.advance(&host, index, draw)? {
            StepOutcome::Ran => {}
            StepOutcome::Finished | StepOutcome::Stopped => break,
            StepOutcome::Failed(error) => return Err(RunError::Scene(error)),
        }

        let samples = run.mix.samples();
        if !samples.is_empty() {
            // The samples are interleaved, so the cut is a whole number of
            // channel frames: a run that stops at its bound hands back a mix
            // whose frames are all complete.
            let room = mix_limit.saturating_sub(output.audio.len()) / AUDIO_CHANNELS;
            let wanted = samples.len() / AUDIO_CHANNELS;
            if room < wanted && !mix_capped {
                mix_capped = true;
                log::warn!(
                    "the mix stops at {mix_seconds} seconds, which is this run's bound; a longer run needs a longer --max-frames"
                );
            }
            let take = room.min(wanted) * AUDIO_CHANNELS;
            output.audio.extend_from_slice(&samples[..take]);
        }

        if draw {
            let texture = {
                let mut nested = host
                    .root_scope()
                    .try_query_one::<&World>(run.render)
                    .map_err(RunError::RenderWorld)?;
                offscreen::texture(nested.get(), run.renderer, &target.texture)?
            };
            let (_, _, pixels) = offscreen::read(&device.device, &device.queue, &texture).await?;
            config.capture.write_frame(index, &pixels, config.size)?;
            output.frames.push((index, pixels));
        }
    }

    output.advanced = run.frames.advanced();
    Ok(output)
}
