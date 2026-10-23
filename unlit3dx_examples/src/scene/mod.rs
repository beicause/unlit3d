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
//! - [`SceneState`] builds the scene's assets once; every drop asks the render
//!   world for an instance of its own.
//! - [`spawn_floor`] puts the ground under the shapes, in physics and in the
//!   render world.
//! - [`Balls`] drops the balls: each one is a [`Ball`] entity of its own, which
//!   both the render world and the physics world answer by name, and the poses
//!   the physics world reports are published from it.
//! - [`SceneAudio`] installs the synthesizer's voice and starts the music: the
//!   [`Track`]s it spawns own their clocks here, and each tick sends the audio
//!   world the messages that came due.
//! - [`mount_controls`] is the panel. It lives here with the rest, and the
//!   render world draws it: the panel's egui context runs in this world, which
//!   lays it out, and the render world's `UiSource` holds and draws every frame
//!   it is sent.
//!
//! The panel is the one object whose drawing happens in another world: it
//! carries a `UiPanel`, and the `UiPanelHost` spawned beside it holds the
//! context its interface is built in, laying a frame out each tick and sending
//! it to the render world as a job. It reads input like any other object, from
//! this world's own buffer, so the events that move it are the ones this
//! world's tick dispatched.

use std::time::Duration;

use glam::{Quat, Vec3};
use hashbrown::HashMap;
use unlit_ecs::bundle;
use unlit_ecs::{Entity, World};
use unlit_wgpu::resources::ResourceGraph;
use unlit3d::components::Transform;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3d::ui::UiTarget;
use unlit3dx::physics::PhysicsWorld;
use unlit3dx::prelude::*;
use unlit3dx::ui::UiPanelHost;

use crate::render::{camera_looking_at, mesh_source};

pub use audio::{AudioSetup, SceneAudio, SceneTracks, Track, install_synth};
pub use balls::{Ball, Balls, PENDING_TIMEOUT_TICKS, SPAWN_HEIGHT, SPAWN_SPREAD, SPAWN_TICKS};
pub use floor::{
    GROUND_CHECKER_DARK, GROUND_CHECKER_SIZE, GROUND_COLOR, GROUND_HALF, GROUND_POSITION,
    checker_ground, spawn_floor,
};
pub use music::{ELECTRIC_PIANO_PROGRAM, MUSIC_BOX_PROGRAM, SCALE, bgm_midi, sequence_midi};
pub use shapes::{
    BALL_COLORS, BALL_RADIUS, PATTERN_SIZE, SHAPE_KINDS, ShapeKind, pattern_texture, shape_body,
    shape_kind, shape_mesh,
};
pub use shared::{SceneAssets, SceneState};
pub use tick::{ScenePhase, TICK_AUDIO, TICK_BALLS, TICK_SCENE, Tick, tick_behaviour};
pub use ui::{control_panel_job, mount_controls};

mod audio;
mod balls;
mod floor;
mod music;
mod shapes;
mod shared;
mod tick;
mod ui;

/// The window opens at this size, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (1024, 720);

/// The render sub-world's command queue depth. A frame sends the scene
/// build, the ground, one creation and one show per drop, one pose job and a
/// periodic sweep, with the clears adding a batch despawn, so the capacity
/// keeps every request in flight.
pub const RENDER_COMMAND_CAPACITY: usize = 64;

/// Where a hidden instance waits, far below everything the camera sees.
pub const HIDDEN_POSITION: Vec3 = Vec3::new(0.0, -1000.0, 0.0);

/// The gameplay world's tick rate, in hertz: a little above the physics
/// rate and the render pace, so the world that drives the others is not
/// the slowest of them.
pub const GAME_HZ: f64 = 68.0;

/// The render pace, in hertz: the windowed mode draws as fast as the
/// compositor asks, but the headless run steps its frames at this rate.
pub const RENDER_HZ: f64 = 60.0;

/// The poses gameplay sends the render world every frame: one [`Transform`]
/// per live body, at its physics pose. An instance that is hidden or waiting
/// for a body is not named: the show job already put it where it belongs.
/// [`pose_job`] turns one into the job that writes them onto the entities
/// they name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenePoses(pub Vec<(Entity, Transform)>);

/// A job that writes `poses` onto the render entities they name.
///
/// Moving a scene's instances is the scene's own business, so it travels
/// to the render world as a job like every other change to it: the world
/// applies what it is told and holds no mailbox of its own. An entity the
/// render world has not spawned yet is not an error — it simply names
/// nothing.
pub fn pose_job(poses: ScenePoses) -> Job {
    Box::new(move |world: &mut World| {
        // A render instance lives under the source that draws it, so the
        // scope that owns it is the one that reaches it.
        let Some((source, _)) = mesh_source(world) else {
            return;
        };
        let Some(scope) = world.scope(source) else {
            return;
        };
        for (entity, pose) in poses.0.iter() {
            let _ = scope
                .query_one::<&mut Transform>(*entity)
                .map(|mut transform| {
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

/// Set up the render sub-world: the resource graph, the renderer, the pass
/// its sources draw into, and the asset-server source. Answers the renderer
/// resource entity and the pass entity.
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
) -> (Entity, Entity) {
    let Some(mut nested) = host.root_scope().query_one::<&mut World>(render_world) else {
        unreachable!("the sub-world is a child of the host world");
    };
    let context = spawn_context(
        &mut nested,
        device.clone(),
        queue.clone(),
        ResourceGraph::new(),
        capabilities,
    );
    let renderer = nested
        .root_scope_mut()
        .spawn(bundle![Renderer::new(context)]);
    // The pass draws every source below it, so the scene's sources belong to
    // it rather than to the world's root.
    let pass = nested.root_scope_mut().spawn(bundle![
        FramePass::new(SCENE_PASS_ORDER),
        RenderLoadOps::default(),
    ]);
    let mut source = MeshSource::new(&nested.root_scope(), context);
    source.register_unlit_family(&nested.root_scope());
    nested
        .scope_mut(pass)
        .expect("the scene's pass is alive")
        .spawn(bundle![Source::new(source)]);
    (renderer, pass)
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
pub fn camera_job() -> Job {
    let mut azimuth = ORBIT_START;
    Box::new(move |world: &mut World| {
        // The camera belongs to the mesh source: a source frames what its own
        // subtree holds, so a camera beside it would never be found.
        let Some((source, _)) = mesh_source(world) else {
            panic!("the render world carries no MeshSource");
        };
        let camera_entity = world
            .scope_mut(source)
            .expect("the mesh source entity exists")
            .spawn(bundle![camera_looking_at(
                WINDOW_SIZE,
                orbit_eye(azimuth),
                ORBIT_TARGET,
                60f32.to_radians(),
                0.1,
            ),]);
        world.root_scope_mut().spawn(bundle![OnFrame::new(
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
                let _ = world
                    .root_scope()
                    .query_one::<&mut Camera>(camera_entity)
                    .map(|mut current| {
                        *current = camera;
                    });
            },
        ),]);
    })
}

/// Queue this scene's own content on the render world: its camera, and —
/// where there is a panel to draw — the UI source that draws it.
///
/// `pass` is the pass returned by [`setup_render_world`], where the panel's
/// source belongs.
///
/// Both are jobs, so a world that was set up to draw can be given a scene
/// without its setup ever hearing of one. The panel itself lives in the
/// gameplay world and nothing here reaches it: the source draws whatever the
/// slots hold, and the world holding the panel fills them.
pub fn mount_scene_content(render: &WorldHandle, pass: Entity, panels: Option<PanelSlots>) {
    if let Err(error) = render.sender.send(camera_job()) {
        log::error!("the render world refused its camera ({error:?})");
    }
    if let Some(panels) = panels
        && let Err(error) = render.sender.send(control_panel_job(panels, pass))
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
/// the demo: the collector the balls subscribe with fills it from the rapier
/// world. It holds the newest poses, not a history — a body that moved twice
/// between two reads appears once, where it is now. A handle is the key
/// because it is what a ball names once its body is spawned.
#[derive(Default)]
pub struct GamePhysics {
    /// Every moving body's pose, by the handle it was spawned with.
    pub transforms: HashMap<RigidBodyHandle, (Vec3, Quat)>,
}

/// The other worlds, as the scene's objects reach them.
///
/// A tick hands one to every object, so an object names the world it wants
/// instead of keeping a channel of its own. The ports are plain senders:
/// what an object says to another world is a job, and what it learns back is
/// a subscription — [`poses`](Self::poses) is the physics world's, collected
/// between its steps. Each subscription belongs to the world it reads, so a
/// run that never asks for a snapshot pays nothing for it.
///
/// A port also carries what its world is doing: [`render_state`](Self::render_state),
/// [`physics_state`](Self::physics_state) and [`audio_state`](Self::audio_state)
/// are how an object that asked a question learns that the world it asked is
/// gone, instead of waiting for an answer that will never come.
///
/// Both run modes build one from the worlds they have — threaded worlds for
/// the windowed show, stepped worlds for the headless capture — and both then
/// spawn the same objects with it, so the wiring is written once.
#[derive(Clone)]
pub struct Scene {
    /// Commands into the render world: the objects build their scene and
    /// move their instances through them.
    pub render: JobSender,
    /// Commands into the physics world.
    pub physics: JobSender,
    /// Commands into the audio world.
    pub audio: JobSender,
    /// The physics world's latest pose snapshot.
    pub poses: SnapshotReader<GamePhysics>,
    /// Whether the render world is still running.
    pub render_state: SnapshotReader<WorldState>,
    /// Whether the physics world is still running.
    pub physics_state: SnapshotReader<WorldState>,
    /// Whether the audio world is still running.
    pub audio_state: SnapshotReader<WorldState>,
}

/// Watch the physics world's body poses.
///
/// The collector walks the rapier world on the physics thread and copies the
/// moving bodies' transforms; it answers nothing while they are unchanged, so
/// a world whose bodies are all asleep publishes no snapshot at all. The
/// snapshot carries state, not history: a body that moved twice between two
/// reads is seen once, where it is now.
fn watch_poses(sender: &JobSender) -> SnapshotReader<GamePhysics> {
    let mut previous: HashMap<RigidBodyHandle, (Vec3, Quat)> = HashMap::new();
    sender
        .watch(move |world: &World| {
            let mut transforms = HashMap::new();
            if let Some(physics) = world.root_scope().query::<(Entity, &PhysicsWorld)>().next() {
                for (handle, body) in physics.1.bodies.iter() {
                    if body.is_dynamic() || body.is_kinematic() {
                        transforms.insert(handle, (body.translation(), *body.rotation()));
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

/// Whether the world behind a port has ended.
///
/// A question asked of a world that panicked is never answered, and neither is
/// one asked of a world that stopped: the job is gone with the turn it ran in.
/// An object that asked waits on this instead of waiting forever.
pub(super) fn world_ended(state: &SnapshotReader<WorldState>) -> bool {
    matches!(
        state.latest().as_deref(),
        Some(WorldState::Stopped | WorldState::Panicked(_))
    )
}

/// The scene's worlds, however they are driven.
///
/// Every world is a host entity carrying the runner's proxy, so the host's
/// data is the same whether the worlds run on threads or the host steps
/// them; only who calls their ticks differs. What the objects reach them
/// through is a [`Scene`], built beside them and handed to every tick.
#[derive(Clone)]
pub struct SceneHandles {
    /// The rapier world.
    pub physics: WorldHandle,
    /// The synthesizer's world.
    pub audio: WorldHandle,
    /// The world the scene's objects live in; its sender is the control
    /// panel's handle on them.
    pub gameplay: WorldHandle,
}

/// The slot a panel publishes its frames into.
const PANEL_SLOT: usize = 0;

/// What the two worlds holding the panel share.
///
/// A panel's frames travel the other way, as jobs to the source drawing them:
/// the drawing side owns the slots, and no frame crosses a thread except in
/// the job that carries it. What comes back this way is the target the
/// drawing side measured, which a publisher has to match for its clip
/// rectangles to be packed in the right pixels.
#[derive(Clone, Default)]
pub struct PanelSlots {
    /// The target the drawing side measures every frame it draws.
    pub target: Snapshot<UiTarget>,
}

/// Build the scene's worlds with the runner `R`: on their own threads, or
/// stepped by the host.
///
/// The worlds are spawned in the order they depend on each other — physics
/// first, so the wiring can subscribe to its state, then audio, then the
/// gameplay world with its objects.
///
/// `input` is the writer the host feeds. The gameplay world is a sibling of
/// the host rather than a world nested inside it, so nothing distributes the
/// writer to it: it subscribes here, once, before its first tick. A run with
/// no input of its own — the headless capture — passes a writer nothing is
/// fed, and the panel still lays its frames out.
pub fn spawn_scene<R: WorldRunner>(
    host: &mut World,
    render: WorldHandle,
    audio_setup: AudioSetup,
    panels: PanelSlots,
    input: &InputWriter,
) -> SceneHandles {
    // The physics world: fixed step, default gravity. The ground is queued
    // by the floor object before the first ball drops.
    let physics = spawn_physics_world_with::<R>(host, PhysicsConfig::default());
    // The demo asks for the poses it needs; the library collects nothing on
    // its own. The subscription lives in the physics world, so it runs
    // between that world's steps whatever drives it.
    let poses = watch_poses(&physics.sender);

    // The audio world: one synthesizer voice, mixed into the output stream.
    // The voice builds its synthesizer on the first fill it renders, so it
    // needs no rate here: whatever opens the stream says what that rate is.
    // Only the setup that opens a device asks a stepped world for one; a
    // world the host pulls the fills of opens nothing.
    let config = AudioConfig {
        stepped_device: matches!(audio_setup, AudioSetup::Device),
        ..AudioConfig::default()
    };
    let audio = spawn_audio_world_with::<R>(host, config);
    // The voice is installed from the gameplay world, as a job whose answer
    // is the entity the scenes' sequences address. The soundfont is kept for
    // that job rather than loaded here, so the decode runs where the job
    // runs instead of stalling the host's setup.
    let soundfont = match audio_setup {
        AudioSetup::Silent => None,
        AudioSetup::Pulled { soundfont } => Some(soundfont),
        AudioSetup::Device => Some(unlit3dx_asset_bytes::soundfont().into()),
    };

    let scene = Scene {
        render: render.sender.clone(),
        render_state: render.state.clone(),
        physics: physics.sender.clone(),
        physics_state: physics.state.clone(),
        audio: audio.sender.clone(),
        audio_state: audio.state.clone(),
        poses,
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
            let scene = scene.clone();
            let soundfont = soundfont.clone();
            let panels = panels.clone();
            let input = input.clone();
            move |world: &mut World| {
                // This world is a sibling of the host, not a world nested in
                // it, so the host's fan-out never reaches it: it takes its own
                // buffer, once, before the first tick reads one.
                world.root_scope_mut().spawn(bundle![input.subscribe()]);
                world.root_scope_mut().spawn(bundle![OnFrame::new(
                    ScenePhase::Input.at(0),
                    |world, _entity, _frame| {
                        dispatch_input(world);
                    },
                ),]);
                spawn_floor(&scene);
                let state = SceneState::new(&scene);
                world.root_scope_mut().spawn(bundle![
                    state,
                    tick_behaviour::<SceneState>(scene.clone(), ScenePhase::Objects.at(TICK_SCENE)),
                ]);
                let balls = Balls::new();
                world.root_scope_mut().spawn(bundle![
                    balls,
                    tick_behaviour::<Balls>(scene.clone(), ScenePhase::Objects.at(TICK_BALLS)),
                ]);
                if let Some(soundfont) = soundfont {
                    let audio = SceneAudio::new(soundfont);
                    world.root_scope_mut().spawn(bundle![
                        audio,
                        tick_behaviour::<SceneAudio>(
                            scene.clone(),
                            ScenePhase::Objects.at(TICK_AUDIO)
                        ),
                    ]);
                }
                // The panel is the fifth object, and lives here with the rest.
                // The host that lays it out is an entity of its own, so the
                // world it belongs to is the only thing tying the two
                // together. Publishing sits on its own place in this world's
                // frame, after everything that could change what the panel
                // shows has ticked, and the render world draws whatever is
                // there.
                world.root_scope_mut().spawn(bundle![UiPanelHost::new(
                    render.sender.clone(),
                    panels.target.reader(),
                    PANEL_SLOT,
                ),]);
                mount_controls(world);
                world.root_scope_mut().spawn(bundle![OnFrame::new(
                    ScenePhase::Publish.at(0),
                    |world, _entity, _frame| publish_panels(world),
                ),]);
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
                if R::SELF_DRIVEN {
                    clock.wait(control);
                }
                true
            }
        },
    );

    SceneHandles {
        physics,
        audio,
        gameplay,
    }
}

/// Lay this tick's panel out and publish it, if it could differ from the one
/// the drawing side holds.
///
/// The events the panel reads are this world's own, from the buffer its tick
/// filled, so what the user clicked on is what this tick dispatched.
fn publish_panels(world: &World) {
    if let Some((_, mut host)) = world
        .root_scope()
        .query::<(Entity, &mut UiPanelHost)>()
        .next()
    {
        host.publish(world);
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
    /// steps as its rate comes due, and the audio world drains the note jobs
    /// those ticks sent.
    ///
    /// The render world is not stepped here: it is a sub-world the draw
    /// drains, not one with a rate of its own.
    pub fn step(&mut self, host: &World, worlds: &SceneHandles, delta_seconds: f64) {
        // Gameplay first: its requests reach the other worlds before they
        // step.
        self.gameplay_debt += delta_seconds;
        while self.gameplay_debt >= self.gameplay_period {
            step_stepped_world(host, worlds.gameplay.entity);
            self.gameplay_debt -= self.gameplay_period;
        }
        self.physics_debt += delta_seconds;
        while self.physics_debt >= self.physics_period {
            step_stepped_world(host, worlds.physics.entity);
            self.physics_debt -= self.physics_period;
        }
        step_stepped_world(host, worlds.audio.entity);
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
/// [`drop_ball_in`] instead.
pub fn drop_ball(sender: &JobSender) {
    sender
        .send(Box::new(|world: &mut World| {
            drop_ball_in(&world.root_scope());
        }))
        .ok();
}

/// Ask the shapes to drop the next one, from inside the gameplay world.
pub fn drop_ball_in(world: &Scope<'_>) {
    if let Some((_, mut balls)) = world.query::<(Entity, &mut Balls)>().next() {
        balls.queue_drop();
    }
}

/// Ask the shapes to go away, from inside the gameplay world.
///
/// The music keeps playing; the panel drives it.
pub fn clear_balls_in(world: &Scope<'_>) {
    if let Some((_, mut balls)) = world.query::<(Entity, &mut Balls)>().next() {
        balls.queue_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use unlit_wgpu::ui::ScreenDescriptor;
    use unlit3d::ui::{UiPanel, egui};

    /// The target a drawing side publishes once it has measured its pass.
    ///
    /// These tests drive only the gameplay world, so nothing draws and
    /// nothing would otherwise report a size to lay a frame out at.
    fn measured_target() -> UiTarget {
        UiTarget {
            screen: ScreenDescriptor {
                size_in_pixels: [WINDOW_SIZE.0, WINDOW_SIZE.1],
                pixels_per_point: 1.0,
            },
            max_texture_side: 2048,
        }
    }

    /// The gameplay world is a sibling of the host, so nothing distributes
    /// the input writer into it: the world subscribes itself when it is set
    /// up. This checks that an event fed to the writer reaches that world and
    /// brings its panel back to lay a frame out.
    #[test]
    fn input_reaches_a_panel_published_from_a_stepped_world() {
        let mut host = World::new();
        let render = spawn_sub_world(&mut host, "render", 8);
        let input = InputWriter::new();
        let panels = PanelSlots::default();
        // The target is published before the panel exists to subscribe to it:
        // a snapshot keeps its newest value, so the panel finds it on its
        // first tick instead of waiting for another draw.
        panels.target.publish(measured_target());
        let worlds = spawn_scene::<Stepped>(&mut host, render, AudioSetup::Silent, panels, &input);
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        worlds
            .gameplay
            .sender
            .send(Box::new(move |world: &mut World| {
                world.root_scope_mut().spawn(bundle![UiPanel::new(
                    move |_, _, _ui: &mut egui::Ui| {
                        counted.fetch_add(1, Ordering::Relaxed);
                    },
                ),]);
            }))
            .expect("the gameplay world accepts a job");

        // The first ticks settle the panel: egui answers its own first pass
        // with a request for another, and once the picture is settled with no
        // input arriving, no frame is laid out at all.
        let mut settled = 0;
        for _ in 0..8 {
            step_stepped_world(&host, worlds.gameplay.entity);
            let now = runs.load(Ordering::Relaxed);
            if now == settled {
                break;
            }
            settled = now;
        }
        assert!(settled > 0, "the panel was laid out");

        // A press fed to the writer is what a window would push.
        input.push(InputEvent::Key(KeyEvent {
            key: Key::Space,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }));
        step_stepped_world(&host, worlds.gameplay.entity);

        assert!(
            runs.load(Ordering::Relaxed) > settled,
            "the event brought the panel back to lay a frame out"
        );
    }

    /// The events a panel sees are the ones this world's own dispatch
    /// delivered on that tick, which is what makes the two agree.
    #[test]
    fn a_panel_reads_the_events_its_own_tick_dispatched() {
        let mut host = World::new();
        let render = spawn_sub_world(&mut host, "render", 8);
        let input = InputWriter::new();
        let pressed = Arc::new(AtomicUsize::new(0));
        let panels = PanelSlots::default();
        // Published before the panel exists to subscribe to it, which a
        // snapshot keeps for it.
        panels.target.publish(measured_target());
        // The wiring is the scene's, but the panel is this test's: it counts
        // the space presses it saw, the way the real one reads a click.
        let worlds = spawn_scene::<Stepped>(&mut host, render, AudioSetup::Silent, panels, &input);
        let counted = Arc::clone(&pressed);
        worlds
            .gameplay
            .sender
            .send(Box::new(move |world: &mut World| {
                world.root_scope_mut().spawn(bundle![UiPanel::new(
                    move |world, _, _ui: &mut egui::Ui| {
                        if let Some((_, events)) = world.query::<(Entity, &InputEvents)>().next() {
                            events.with_batch(|batch| {
                                counted.fetch_add(batch.len(), Ordering::Relaxed);
                            });
                        }
                    },
                ),]);
            }))
            .expect("the gameplay world accepts a job");

        input.push(InputEvent::Key(KeyEvent {
            key: Key::Space,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }));
        step_stepped_world(&host, worlds.gameplay.entity);

        assert_eq!(
            pressed.load(Ordering::Relaxed),
            1,
            "the panel saw the event its tick dispatched"
        );
    }
}
