//! The headless run: the scene, stepped inline without a window or threads.
//!
//! Every world lives in one place and the run drives them in a fixed order —
//! gameplay ticks, physics steps as its rate comes due, the audio world fills
//! the mix the frame asks for, then the render world drains its commands and
//! draws the frames the capture names. With every world stepped at a fixed
//! rate, a run is reproducible down to the captured frames and audio samples.

use unlit_ecs::World;
use unlit_wgpu::context::RenderDevice;
use unlit_wgpu::readback::readback_texture;
use unlit3d::frame::Frame;
use unlit3d::offscreen::{PersistentTarget, TargetConfig};
use unlit3d::renderer::Renderer;

use super::App;
use super::capture::Capture;
use super::pace::Pacing;
use super::scene::{Mode, RenderPorts, ScenePorts, Stepping};
use crate::audio::dispatch_audio_fill;
use crate::host::{spawn_sub_world, tick_sub_world};
use crate::world::{Stepped, with_stepped_world_mut};

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
    /// The run draws a frame only when the capture names it. A capture that
    /// names nothing leaves the run audio-only: the render world still
    /// drains its commands and dispatches its frames, but none is drawn.
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
/// The device is injected, so a caller picks the tier it asks for. A headless
/// run has no way to recover a world that failed to come up, so a broken setup
/// panics instead of returning a hole in the capture.
///
/// A run bounded by nothing steps frames until the process is stopped, which
/// is why it reads as a capture the caller drives: the frames it keeps are
/// written as it reaches them, so a run cut short keeps the ones it got to.
pub async fn headless<A: App>(
    mut app: A,
    config: &HeadlessConfig,
    device: RenderDevice,
) -> HeadlessOutput {
    let mut host = World::new();
    let render = spawn_sub_world(&mut host, "render", config.render_capacity);
    // Nothing feeds input to a run with no window, but the scene still takes
    // a buffer: a panel lays its frames out from it, and a run that never
    // pushes an event simply never sees one.
    let input = unlit3d::input::InputWriter::new();
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
    let offscreen = {
        let mut nested = host.root_scope().query_one::<&mut World>(render.entity);
        let world = nested.get();
        let context = world
            .root_scope()
            .query_one::<&Renderer>(setup.renderer.entity)
            .get()
            .context();
        PersistentTarget::bind(
            world,
            context,
            setup.pass.entity,
            &device.device,
            config.size,
            TargetConfig::default(),
        )
        .expect("the offscreen target binds")
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

    let mut index = 0u32;
    while !config.pacing.is_spent(index) {
        frame.index = index;

        // The worlds step once for this frame: gameplay first, so the notes
        // its sequences play reach the audio world before it steps, then
        // physics as its rate comes due and audio for the jobs booked.
        stepper.step(&host, 1.0 / HEADLESS_HZ);

        // Audio: render whatever samples the frame's duration asks for.
        let frames = stepper.audio_frames(1.0 / HEADLESS_HZ);
        if frames > 0 {
            samples_out.clear();
            samples_out.resize(frames * AUDIO_CHANNELS, 0.0);
            let filled = with_stepped_world_mut(&host, stepper.worlds().audio.entity, |audio| {
                dispatch_audio_fill(
                    audio,
                    &mut samples_out,
                    config.audio_sample_rate,
                    AUDIO_CHANNELS as u16,
                );
            });
            assert!(filled.is_ok(), "the audio world is a stepped world");
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

        // Render: drain the sub-world's commands — the scene build rides on
        // that drain — then dispatch the frame. The dispatch happens every
        // frame, an audio-only run included: it is what advances the frame's
        // behaviours, so the path a capture freezes is the path the run walked.
        // Only the frames a capture names are drawn.
        //
        // Drawing every captured frame is enough because a draw does not
        // advance the scene: it reads what the steps left behind, and the state
        // it writes is only what a frame publishes for its readers — the target
        // a panel lays itself out against and the URLs its widgets opened —
        // which the next frame overwrites anyway. A scene that accumulates
        // across draws — a ping-pong target, a temporal filter — would have to
        // draw every frame.
        let mut nested = host.root_scope().query_one::<&mut World>(render.entity);
        let world = nested.get();
        tick_sub_world(world, &mut frame);
        if config.capture.wants(index) {
            let context = {
                let mut renderer_row = world
                    .root_scope()
                    .query_one::<&mut Renderer>(setup.renderer.entity);
                renderer_row.get().render(&*world);
                renderer_row.get().context()
            };
            let texture = {
                let mut graph = context
                    .try_graph(&world.root_scope())
                    .expect("the frame's resource graph exists");
                graph
                    .get()
                    .get(&offscreen.texture)
                    .cloned()
                    .expect("the target texture is a resource node")
            };
            let pixels = readback_texture(&device.device, &device.queue, &texture).await;
            if let Err(message) = config.capture.write_frame(index, &pixels, config.size) {
                log::error!("{message}");
            }
            output.frames.push((index, pixels));
        }
        drop(nested);
        index = index.saturating_add(1);
        output.advanced = index;
    }

    output
}
