//! The demo's scene: its constants, its objects, and the worlds that run them.
//!
//! The windowed binary and the headless run both build on this module:
//! everything here is world-shaped, free of window and thread plumbing, so the
//! same objects drive the windowed show and the deterministic headless capture.
//!
//! The gameplay world is a set of objects rather than one state blob. Each
//! object is a component carrying what it owns, plus a [`Tick`] that advances
//! it, so the scene grows by spawning an object and never by editing a tick:
//!
//! - [`SceneState`] builds the shape pool and hands its slots out.
//! - [`Floor`] puts the ground under the shapes, in physics and in the render
//!   world.
//! - [`Balls`] drops the shapes, maps physics bodies onto pool slots, and
//!   publishes their poses.
//! - [`SceneAudio`] installs the synthesizer's sources and starts the music.
//! - [`Controls`] is the panel. It lives here with the rest, and the render
//!   world draws it: the panel's egui context runs in this world, which lays it
//!   out, and the render world's `UiSource` draws the frames that come back.
//!
//! The panel is the one object whose drawing happens in another world, so it
//! carries a `UiHost` holding the context its interface is built in; the
//! render world asks that context for a frame through this world's command
//! queue, and draws the answer. Everything else reaches its neighbours
//! through the same channels every world uses.

use std::time::Duration;

use glam::{Quat, Vec3};
use unlit_ecs::{Entity, World};
use unlit_wgpu::resources::ResourceGraph;
use unlit3d::components::Transform;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3d::ui::{ExternalFrames, UiScreen};
use unlit3dx::physics::PhysicsWorld;
use unlit3dx::prelude::*;
use unlit3dx::ui::{PanelHost, UiHost};

use crate::render::camera_looking_at;

pub use audio::{
    AudioSetup, BGM_TRACK, SEQUENCE_TRACK, SYNTH_GAIN, SceneAudio, install_audio_job_with,
    install_synth, subscribe_audio,
};
pub use balls::{
    Balls, PENDING_TIMEOUT_TICKS, SPAWN_HEIGHT, SPAWN_SPREAD, SPAWN_TICKS, scene_frame,
};
pub use floor::{
    Floor, GROUND_CHECKER_DARK, GROUND_CHECKER_SIZE, GROUND_COLOR, GROUND_HALF, GROUND_POSITION,
    checker_ground,
};
pub use music::{ELECTRIC_PIANO_PROGRAM, MUSIC_BOX_PROGRAM, SCALE, bgm_midi, sequence_midi};
pub use shapes::{
    BALL_COLORS, BALL_RADIUS, PATTERN_SIZE, SHAPE_KINDS, ShapeKind, pattern_texture, shape_body,
    shape_kind, shape_mesh,
};
pub use shared::{
    POOL_SIZE, SceneState, pool_from_instances, request_pool_instances, request_shape_assets,
};
pub use tick::{TICK_AUDIO, TICK_BALLS, TICK_FLOOR, TICK_SCENE, Tick, tick_behaviour};
pub use ui::{Controls, control_panel_job};

mod audio;
mod balls;
mod floor;
mod music;
mod shapes;
mod shared;
mod tick;
mod ui;

/// The soundfont the demo plays, embedded at build time by `build.rs`.
///
/// When the soundfont file is missing the build writes an empty stub, so
/// the demo starts and runs silent rather than failing to compile.
pub const SOUNDFONT: &[u8] = include_bytes!(env!("SOUNDFONT_FILE"));

/// The window opens at this size, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (1024, 720);

/// The render sub-world's command queue depth. The pool's build queues
/// forty-eight instance requests at once, and the ground adds its own three
/// beside them, so the capacity keeps every request in flight.
pub const RENDER_COMMAND_CAPACITY: usize = 64;

/// Where idle pool entities wait, far below everything the camera sees.
pub const HIDDEN_POSITION: Vec3 = Vec3::new(0.0, -1000.0, 0.0);

/// The gameplay world's tick rate, in hertz.
pub const GAME_HZ: f64 = 120.0;

/// The render pace, in hertz: the windowed mode draws as fast as the
/// compositor asks, but the headless run steps its frames at this rate.
pub const RENDER_HZ: f64 = 60.0;

/// The poses gameplay sends the render world every frame: one
/// [`Transform`] per render entity it owns, a live body at its physics
/// pose and a free slot parked at [`HIDDEN_POSITION`]. [`pose_job`] turns
/// one into the job that writes them onto the entities they name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenePoses(pub Vec<(Entity, Transform)>);

/// A job that writes `poses` onto the render entities they name.
///
/// Moving a scene's instances is the scene's own business, so it travels
/// to the render world as a job like every other change to it: the world
/// applies what it is told and holds no mailbox of its own. An entity the
/// render world has not spawned yet is not an error — it simply names
/// nothing.
pub fn pose_job(poses: ScenePoses) -> Job<()> {
    Box::new(move |world: &mut World| {
        for (entity, pose) in poses.0.iter() {
            let _ = world.with_mut::<Transform, _>(*entity, |transform| {
                transform.clone_from(pose);
            });
        }
    })
}

/// The host frame's behaviour order: the host's own input dispatch runs
/// after the setup and before the draw, whose UI source reads the same
/// events.
pub const FRAME_DISPATCH_INPUT: i32 = 5;

/// The host frame's behaviour order: the inline worlds step here, after
/// the input dispatch and before the draw, so the transforms they publish
/// are the ones the frame draws.
pub const FRAME_INLINE_STEP: i32 = 6;

/// The render world's behaviour order: the camera orbit runs here, after
/// the scene's pose jobs have landed on a frame's drain.
pub const RENDER_CAMERA: i32 = 100;

/// How fast the camera circles the scene, in radians per second.
pub const ORBIT_SPEED: f32 = 0.25;

/// The camera's orbit radius, in metres.
pub const ORBIT_RADIUS: f32 = 16.0;

/// The camera's orbit height, in metres.
pub const ORBIT_HEIGHT: f32 = 9.0;

/// The point the camera looks at.
pub const ORBIT_TARGET: Vec3 = Vec3::new(0.0, 0.6, 0.0);

/// Where the camera's orbit starts, in radians.
pub const ORBIT_START: f32 = 0.65;

/// Set up the render sub-world: the resource graph, the renderer and the
/// asset-server source. Answers the renderer resource entity.
///
/// This is the whole of what a render world needs before it can draw, and
/// none of it is this scene's: no camera, no shape, no panel. Everything
/// the scene puts in the world — meshes, materials, instances, poses,
/// camera, UI — arrives as a job, through the sub-world's drain in the
/// draw behaviour.
pub fn setup_render_world(
    host: &mut World,
    render_world: Entity,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    capabilities: DeviceCapabilities,
) -> Entity {
    let Some(mut nested) = host.get_mut::<World>(render_world) else {
        unreachable!("the sub-world is a child of the host world");
    };
    let context = spawn_context(
        &mut nested,
        device.clone(),
        queue.clone(),
        ResourceGraph::new(),
        capabilities,
    );
    let renderer = nested.spawn((Renderer::new(context),));
    let mut source = MeshSource::new(&nested, context);
    source.register_unlit_family(&nested);
    nested.spawn((Source::new(source),));
    renderer
}

/// A job that gives the render world this scene's camera: one entity
/// holding a camera aimed at the scene, and the frame behaviour that walks
/// it around its orbit.
///
/// Where a scene looks from is the scene's to say, so it reaches the
/// render world the same way its meshes and poses do. The behaviour keeps
/// the projection matched to the frame's own aspect, and advances the
/// orbit by each frame's delta, so the speed does not follow the redraw
/// rate.
pub fn camera_job() -> Job<()> {
    let mut azimuth = ORBIT_START;
    Box::new(move |world: &mut World| {
        let camera_entity = world.spawn((camera_looking_at(
            WINDOW_SIZE,
            orbit_eye(azimuth),
            ORBIT_TARGET,
            60f32.to_radians(),
            0.1,
        ),));
        world.spawn((OnFrame::new(
            FrameBehaviourOrder(RENDER_CAMERA),
            move |world, _entity, frame| {
                azimuth += ORBIT_SPEED * frame.delta_time;
                let camera = camera_looking_at(
                    frame.size,
                    orbit_eye(azimuth),
                    ORBIT_TARGET,
                    60f32.to_radians(),
                    0.1,
                );
                let _ = world.with_mut::<Camera, _>(camera_entity, |current| {
                    *current = camera;
                });
            },
        ),));
    })
}

/// Queue this scene's own content on the render world: its camera, and —
/// where there is a panel to draw — the UI source that draws it.
///
/// Both are jobs, so a world that was set up to draw can be given a scene
/// without its setup ever hearing of one. The panel itself lives in the
/// gameplay world, so what arrives here is that world's command queue: the
/// source asks it for a frame, and draws what comes back.
pub fn mount_scene_content(render: &WorldHandle, panels: Option<PanelSlots>) {
    if let Err(error) = render.send(camera_job()) {
        log::error!("the render world refused its camera ({error:?})");
    }
    if let Some(panels) = panels
        && let Err(error) = render.send(control_panel_job(panels))
    {
        log::error!("the render world refused its control panel ({error:?})");
    }
}

/// The point on the orbit the camera sits at for one azimuth.
fn orbit_eye(azimuth: f32) -> Vec3 {
    Vec3::new(
        ORBIT_TARGET.x + azimuth.cos() * ORBIT_RADIUS,
        ORBIT_HEIGHT,
        ORBIT_TARGET.z + azimuth.sin() * ORBIT_RADIUS,
    )
}

/// The physics state the demo subscribes to: where every body is.
///
/// The library defines no snapshot payload of its own, so this one belongs to
/// the demo: the collector the shapes subscribe with fills it from the rapier
/// world. It holds the newest poses, not a history — a body that moved twice
/// between two reads appears once, where it is now.
#[derive(Default)]
pub struct GamePhysics {
    /// Every moving body's transform.
    pub transforms: Vec<(RigidBodyHandle, Vec3, Quat)>,
}

/// The channels between the scene's worlds, wired the same way in both
/// run modes: transforms flow to the render world, body commands to
/// physics, transport jobs to audio, and the objects of the gameplay world
/// tie them to the render server. Each subscription belongs to the world it
/// reads, so a run that never asks for a snapshot pays nothing for it.
///
/// Each mode builds the struct from the worlds it has — remote worlds on
/// threads for the windowed show, inline worlds with unbounded queues for
/// the headless capture — and both then spawn the same objects from it, so
/// the wiring is written once.
#[derive(Clone)]
pub struct SceneWiring {
    /// Commands into the render world: the objects build their scene and
    /// move their instances through them.
    pub render_sender: JobSender,
    /// Commands into the physics world.
    pub physics_sender: JobSender,
    /// The physics world's latest pose snapshot.
    pub physics_reader: SnapshotReader<GamePhysics>,
    /// Commands into the audio world.
    pub audio_sender: JobSender,
    /// The audio world's readiness mailbox.
    pub audio_ready: SnapshotReader<()>,
}

/// Subscribe the shapes to the physics world's body poses.
///
/// The collector walks the rapier world on the physics thread and copies the
/// moving bodies' transforms; it answers nothing while they are unchanged, so
/// a world whose bodies are all asleep publishes no snapshot at all. The
/// mailbox carries state, not history: a body that moved twice between two
/// reads is seen once, where it is now.
fn subscribe_physics(sender: &JobSender) -> SnapshotReader<GamePhysics> {
    let mut previous: Vec<(RigidBodyHandle, Vec3, Quat)> = Vec::new();
    sender
        .subscribe(move |world: &World| {
            let mut transforms = Vec::new();
            if let Some(physics) = world.query::<&PhysicsWorld>().next() {
                for (handle, body) in physics.1.bodies.iter() {
                    if body.is_dynamic() || body.is_kinematic() {
                        transforms.push((handle, body.translation(), *body.rotation()));
                    }
                }
            }
            if transforms == previous {
                return None;
            }
            // The collector keeps what it published to compare the next step
            // against, so the world allocates only when the poses changed.
            previous = transforms.clone();
            Some(GamePhysics { transforms })
        })
        .expect("the physics world accepts a subscription")
}

/// The scene's worlds, however they are driven.
///
/// Every world is a host entity carrying the runner's proxy, so the host's
/// data is the same whether the worlds run on threads or the host steps
/// them; only who calls their ticks differs.
#[derive(Clone)]
pub struct SceneWorlds {
    /// The channels between the worlds.
    pub wiring: SceneWiring,
    /// The rapier world.
    pub physics: WorldHandle,
    /// The synthesizer's world.
    pub audio: WorldHandle,
    /// The world the scene's objects live in; its sender is the control
    /// panel's handle on them.
    pub gameplay: WorldHandle,
    /// Whether the audio world carries sources to transport.
    audio_sources: bool,
}

impl SceneWorlds {
    /// The sender for the audio world's transport jobs, when the scene
    /// gave it a synthesizer: the control panel's handle on the tracks.
    #[must_use]
    pub fn audio_tracks(&self) -> Option<JobSender> {
        self.audio_sources.then(|| self.wiring.audio_sender.clone())
    }
}

/// The slot a panel publishes its frames into.
const PANEL_SLOT: usize = 0;

/// The one-way handover from the world holding the panels to the one drawing
/// them.
///
/// The gameplay world writes its frames into `frames`; the render world reads
/// them there. Neither asks the other for anything, so a settled panel costs
/// nothing on the wire.
#[derive(Clone, Default)]
pub struct PanelSlots {
    /// The slots frames are published into.
    pub frames: ExternalFrames,
    /// The target the drawing side last drew at, which a publisher has to
    /// match for its clip rectangles to be packed in the right pixels.
    pub screen: UiScreen,
}

/// Build the scene's worlds with the runner `R`: on their own threads, or
/// stepped by the host.
///
/// The worlds are spawned in the order they depend on each other — physics
/// and audio first, so the wiring can subscribe to their state, then the
/// gameplay world with its objects.
pub fn spawn_scene<R: WorldRunner>(
    host: &mut World,
    render: WorldHandle,
    audio_setup: AudioSetup,
    panels: PanelSlots,
) -> SceneWorlds {
    // The physics world: fixed step, default gravity. The ground is queued
    // by the floor object before the first ball drops.
    let physics = spawn_physics_world_with::<R>(host, PhysicsConfig::default());
    let physics_sender = physics.sender();
    // The demo asks for the poses it needs; the library collects nothing on
    // its own. The subscription lives in the physics world, so it runs
    // between that world's steps whatever drives it.
    let physics_reader = subscribe_physics(&physics_sender);

    // The audio world: a synthesizer voice plus one MIDI player per track,
    // all fed by the soundfont, mixed into the output stream. Only the
    // setup that opens a device asks a stepped world for one; a world the
    // host pulls the fills of opens nothing.
    let config = AudioConfig {
        stepped_device: matches!(audio_setup, AudioSetup::Device),
        ..AudioConfig::default()
    };
    let audio = spawn_audio_world_with::<R>(host, config);
    let audio_sender = audio.sender();
    let audio_ready = subscribe_audio(&audio_sender);
    let (audio_format, audio_sources) = match audio_setup {
        AudioSetup::Silent => (None, false),
        AudioSetup::Pulled {
            soundfont,
            sample_rate,
        } => {
            // The sources travel as a job, exactly as the windowed mode
            // queues them once its device reports a rate; a world that
            // opens no device has no rate to wait for, so one step
            // installs them before the first gameplay tick reads the
            // readiness mailbox.
            let install = install_audio_job_with(soundfont, sample_rate);
            if let Err(error) = audio_sender.send(install) {
                log::error!("the audio world refused its sources ({error:?})");
            }
            step_stepped_world(host, audio.entity());
            (None, true)
        }
        AudioSetup::Device => {
            // The format is a component the audio world spawns on the tick
            // its stream opens, so the subscription is made before that
            // tick and answers the rate the device really uses.
            let format = audio_sender
                .subscribe(|world: &World| {
                    world
                        .query::<&AudioFormat>()
                        .next()
                        .map(|(_, format)| *format)
                })
                .expect("the audio world accepts a subscription");
            (Some(format), true)
        }
    };

    let wiring = SceneWiring {
        render_sender: render.sender(),
        physics_sender,
        physics_reader,
        audio_sender,
        audio_ready,
    };

    // The gameplay world: one entity per object, each ticking itself as the
    // world's frame. A world the host steps is ticked once per call, so it
    // does not pace itself; a thread of its own runs at the gameplay rate
    // and sleeps between ticks.
    let gameplay = R::spawn(
        host,
        "gameplay",
        64,
        {
            let render = render.clone();
            let wiring = wiring.clone();
            let audio_format = audio_format.clone();
            let panel_audio = audio_sources.then(|| wiring.audio_sender.clone());
            let panels = panels.clone();
            move |world: &mut World| {
                let state = SceneState::new(world, render.clone());
                world.spawn((state, tick_behaviour::<SceneState>(TICK_SCENE)));
                let floor = Floor::new(&wiring, render.clone());
                world.spawn((floor, tick_behaviour::<Floor>(TICK_FLOOR)));
                let balls = Balls::new(&wiring);
                world.spawn((balls, tick_behaviour::<Balls>(TICK_BALLS)));
                if audio_sources {
                    let audio = SceneAudio::new(&wiring, audio_format);
                    world.spawn((audio, tick_behaviour::<SceneAudio>(TICK_AUDIO)));
                }
                // The panel is the fifth object, and lives here with the rest.
                // It lays its frames out on this world's own tick and
                // publishes them; the render world draws whatever is there.
                let host = PanelHost::new(
                    UiHost::new(),
                    panels.frames.clone(),
                    panels.screen.clone(),
                    PANEL_SLOT,
                );
                Controls::new(panel_audio).mount(world, host);
            }
        },
        {
            let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / GAME_HZ));
            let mut index = 0;
            move |world: &mut World, control: &ThreadControl, ()| {
                if control.stop_requested() {
                    return false;
                }
                // A tick is a frame whose duration is the gameplay period:
                // the objects read it from the frame like any other
                // behaviour, and nothing paces itself off a clock.
                let mut frame = Frame {
                    delta_time: (1.0 / GAME_HZ) as f32,
                    index,
                    size: WINDOW_SIZE,
                };
                index = index.wrapping_add(1);
                dispatch_frame(world, &mut frame);
                publish_panels(world);
                if R::SELF_DRIVEN {
                    clock.wait(control);
                }
                true
            }
        },
    );

    SceneWorlds {
        wiring,
        physics,
        audio,
        gameplay,
        audio_sources,
    }
}

/// Lay this tick's panel out and publish it, if it could differ from the one
/// the drawing side holds.
///
/// The events are this world's own, taken from the buffer its tick filled:
/// they are what decides whether laying a frame out is worth it at all.
fn publish_panels(world: &World) {
    // The events are read into a scratch list before the host is borrowed:
    // publishing takes the world by reference, and both borrows cannot be
    // held at once.
    let events = world
        .query::<&InputEvents>()
        .next()
        .map(|(_, events)| events.with_batch(|batch| batch.to_vec()))
        .unwrap_or_default();
    if let Some((_, mut host)) = world.query::<&mut PanelHost>().next() {
        host.publish(world, &events);
    }
}

/// The pacing a host uses to step a scene's worlds frame by frame.
///
/// The frame's own duration is what drives the worlds, and none of their
/// rates is an integer multiple of the display's, so each frame carries
/// the fraction of a step it did not spend into the next one. Counting
/// seconds rather than frames is what keeps the simulation at its rates on
/// a display that redraws at anything but 60 Hz; a fixed-rate run passes an
/// exact step instead of a measured one, which is what makes it
/// reproducible.
pub struct SceneStepper {
    /// The gameplay time the last frame did not spend.
    gameplay_debt: f64,
    /// The physics time the last frame did not spend.
    physics_debt: f64,
    /// The audio time the last frame did not spend.
    audio_debt: f64,
    /// How long one gameplay tick lasts.
    gameplay_period: f64,
    /// How long one physics step lasts.
    physics_period: f64,
    /// How long one audio frame lasts, or none when the host pulls no
    /// audio.
    sample_period: Option<f64>,
}

impl SceneStepper {
    /// A stepper for a scene simulated at `gameplay_hz` and `physics_hz`,
    /// and — when `sample_rate` is given — mixed at that rate.
    #[must_use]
    pub fn new(gameplay_hz: f64, physics_hz: f64, sample_rate: Option<u32>) -> Self {
        Self {
            gameplay_debt: 0.0,
            physics_debt: 0.0,
            audio_debt: 0.0,
            gameplay_period: 1.0 / gameplay_hz,
            physics_period: 1.0 / physics_hz,
            sample_period: sample_rate.map(|rate| 1.0 / f64::from(rate)),
        }
    }

    /// Step the scene once for `delta_seconds`: gameplay ticks, physics
    /// steps as its rate comes due, and the audio world drains the
    /// transport jobs those ticks booked.
    ///
    /// The render world is not stepped here: it is a sub-world the draw
    /// drains, not one with a rate of its own.
    pub fn step(&mut self, host: &World, worlds: &SceneWorlds, delta_seconds: f64) {
        // Gameplay first: its requests reach the other worlds before they
        // step.
        self.gameplay_debt += delta_seconds;
        while self.gameplay_debt >= self.gameplay_period {
            step_stepped_world(host, worlds.gameplay.entity());
            self.gameplay_debt -= self.gameplay_period;
        }
        self.physics_debt += delta_seconds;
        while self.physics_debt >= self.physics_period {
            step_stepped_world(host, worlds.physics.entity());
            self.physics_debt -= self.physics_period;
        }
        step_stepped_world(host, worlds.audio.entity());
    }

    /// The audio frames this frame's duration asks for, carrying the
    /// fraction it did not spend into the next frame.
    ///
    /// Zero when the stepper was built without a sample rate.
    pub fn audio_frames(&mut self, delta_seconds: f64) -> usize {
        let Some(period) = self.sample_period else {
            return 0;
        };
        self.audio_debt += delta_seconds;
        let frames = (self.audio_debt / period) as usize;
        self.audio_debt -= frames as f64 * period;
        frames
    }
}

/// Ask the shapes to drop the next one, from another world.
///
/// The host's keyboard behaviour lives outside the gameplay world, so it
/// reaches the shapes through a job; the panel, which lives inside it, calls
/// [`request_drop_in`] instead.
pub fn request_drop(sender: &JobSender) {
    sender
        .send(Box::new(|world: &mut World| request_drop_in(world)))
        .ok();
}

/// Ask the shapes to drop the next one, from inside the gameplay world.
pub fn request_drop_in(world: &World) {
    if let Some((_, mut balls)) = world.query::<&mut Balls>().next() {
        balls.request_drop();
    }
}

/// Ask the shapes to go back in the pool, from inside the gameplay world.
///
/// The music keeps playing; the panel drives it.
pub fn request_clear_in(world: &World) {
    if let Some((_, mut balls)) = world.query::<&mut Balls>().next() {
        balls.request_clear();
    }
}
