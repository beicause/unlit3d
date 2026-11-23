//! The demo's scene: its constants, its objects, and the worlds that run them.
//!
//! Every run builds on this module through the application's
//! [`crate::demo::Demo`]: everything here is world-shaped, free of window and
//! thread plumbing, so the same objects drive the windowed show, the
//! deterministic headless capture and the interactive session.
//!
//! The gameplay world is a set of objects rather than one state blob. The
//! `tick` module says how one is put together; the objects themselves are:
//!
//! - [`SceneState`] builds the scene's assets once; every drop asks the render
//!   world for an instance of its own.
//! - [`spawn_floor`] puts the ground under the balls, in physics and in the
//!   render world.
//! - [`Balls`] drops the balls: each one is a [`Ball`] entity of its own, which
//!   both the render world and the physics world answer by name, and the poses
//!   the physics world reports are published from it.
//! - [`SceneAudio`] installs the synthesizer's voice and starts the music: the
//!   [`Track`]s it spawns own their clocks here, and each tick sends the audio
//!   world the messages that came due.
//! - [`mount_controls`] is the panel. It lives here with the rest, and the
//!   render world draws it: the panel's egui context runs in this world, which
//!   lays it out, and the render world's `EguiSource` holds and draws every frame
//!   it is sent.
//!
//! The panel is the one object whose drawing happens in another world: it
//! carries a `EguiPanel`, and the `EguiPanelHost` spawned beside it holds the
//! context its interface is built in, laying a frame out each tick and sending
//! it to the render world as a job. It reads input like any other object, from
//! this world's own buffer, so the events that move it are the ones this
//! world's tick dispatched.
//!
//! Everything the scene owns in the render world arrives there as a job this
//! world queues: its assets, instances, poses and its camera. What it does not
//! own is the pipeline — the pass, and the source the panel draws through,
//! which belongs to that pass. The host builds those in the GPU callback,
//! because a pass exists only once the device has answered and a job would
//! have to wait behind the drain that draws.
//!
//! The setup spawns the objects in whatever order reads best, and only the two
//! that read what another object writes state an order: the balls tick after
//! the default band the assets are built in, and the panel's publish after both.
//! Everything else is in the default band, where the dispatch promises nothing
//! about which of them runs first.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use glam::{Quat, Vec3};
use hashbrown::HashMap;
use unlit_ecs::bundle;
use unlit_ecs::{Entity, RemoteEntity, World};
use unlit_wgpu::resources::ResourceGraph;
use unlit3d::components::Transform;
use unlit3d::egui::EguiTarget;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3dx::egui::EguiPanelHost;
use unlit3dx::physics::PhysicsWorld;
use unlit3dx::prelude::*;

use unlit3dx::app::SceneWorlds;

use crate::render::{camera_looking_at, mesh_source};

use self::audio::Music;
use self::balls::BallDrops;
use self::shared::SceneBuild;

pub use audio::{AudioSetup, SceneAudio, SceneTracks, Track, install_synth};
pub use balls::{Ball, BallPhase, Balls};
pub use egui::mount_controls;
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

mod audio;
mod balls;
mod egui;
mod floor;
mod music;
mod shapes;
mod shared;
mod tick;

/// The window opens at this size, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (1024, 720);

/// Where a hidden instance waits, far below everything the camera sees.
pub const HIDDEN_POSITION: Vec3 = Vec3::new(0.0, -1000.0, 0.0);

/// The gameplay world's pacing rate, in hertz, until the window reports the
/// refresh rate of the display it is on.
///
/// The rate is a ceiling on how often the world wakes, not a simulation rate:
/// a tick's duration is the one it measured, so a display that redraws
/// slower or faster changes how long the thread sleeps, never how the scene
/// behaves.
pub const GAMEPLAY_HZ: f64 = 60.0;

/// The poses gameplay sends the render world every frame: one [`Transform`]
/// per live body, at its physics pose. An instance that is hidden or waiting
/// for a body is not named: the show job already put it where it belongs.
/// [`pose_job`] turns one into the job that writes them onto the entities
/// they name.
///
/// The entities are the render world's own. Every world numbers its entities
/// from zero, so an id from the world that sends the job names something
/// unrelated in the world that runs it, which is why they are
/// [`RemoteEntity`]s rather than bare entities.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenePoses(pub Vec<(RemoteEntity, Transform)>);

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
        let Ok(scope) = world.try_scope(source.entity) else {
            return;
        };
        for (entity, pose) in poses.0.iter() {
            let _ = scope
                .try_query_one::<&mut Transform>(entity.entity)
                .map(|mut transform| {
                    transform.get().clone_from(pose);
                });
        }
    })
}

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

/// The order the balls tick at: after the default band, where the scene's
/// assets are built, since a tick reads the assets the build wrote.
pub(super) const BALLS_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(100);

/// The order the panel publishes at: after every object, the balls included,
/// since the panel shows what they wrote this frame.
pub(super) const PUBLISH_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(200);

/// Set up the render sub-world: the resource graph, the renderer, the pass
/// its sources draw into, the asset-server source, and — when `panel` names a
/// slot — the source that draws the control panel. Answers the renderer
/// resource entity and the pass entity.
///
/// This is the whole of what a render world needs before it can draw, and
/// none of it is this scene's: no camera, no shape, no panel. Everything
/// the scene puts in the world — meshes, materials, instances, poses and
/// the camera — arrives as a job, through the sub-world's drain in the draw
/// behaviour.
///
/// The panel's source is here rather than queued from the scene because it
/// belongs to the pass, and a pass records the sources spawned below it: this
/// function is what makes the pass, so it is the only place the source can be
/// put under it. Passing `None` leaves the pass with one source, which is what
/// a run that captures frames rather than drawing them wants — the gameplay
/// world still lays its panel out, but no source carries the frame to a pass,
/// so the captured image holds the scene alone.
pub fn setup_render_world(
    host: &mut World,
    render_world: Entity,
    device: RenderDevice,
    panel: Option<Snapshot<EguiTarget>>,
) -> (RemoteEntity, RemoteEntity) {
    let Ok(mut nested) = host.root_scope().try_query_one::<&mut World>(render_world) else {
        panic!(
            "render_world {:?} is not a sub-world of the host world",
            render_world
        );
    };
    let world = nested.get();
    let context = spawn_context(world, device, ResourceGraph::new());
    let renderer = world.root_scope_mut().spawn(Renderer::new(context));
    // The pass draws every source below it, so the scene's sources belong to
    // it rather than to the world's root.
    let pass = world.root_scope_mut().spawn(Pass::new(SCENE_PASS_ORDER));
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    world
        .scope_mut(pass)
        .spawn(bundle![Source::new(FrameSourceOrder::MESH, source)]);
    if let Some(screen) = panel {
        let mut source = EguiSource::new();
        source.publish_target(screen);
        world
            .scope_mut(pass)
            .spawn(bundle![Source::new(FrameSourceOrder::EGUI, source)]);
    }
    (world.remote(renderer), world.remote(pass))
}

/// A job that gives the render world this scene's camera: one entity
/// holding a camera aimed at the scene, and the frame behaviour that walks
/// it around its orbit.
///
/// Where a scene looks from is the scene's to say, so it reaches the
/// render world the same way its meshes and poses do: as a job the scene
/// sends when it is built. The render world runs no job until it is drained,
/// and it is drained only where its frame is driven — after its own setup has
/// run — so the mesh source the camera hangs under exists by the time the job
/// executes. The behaviour keeps the projection matched to the target the pass
/// binds each frame, and advances the orbit by each frame's delta, so the speed
/// does not follow the redraw rate.
fn camera_job() -> Job {
    let mut azimuth = ORBIT_START;
    Box::new(move |world: &mut World| {
        // The camera belongs to the mesh source: a source frames what its own
        // subtree holds, so a camera beside it would never be found.
        let Some((source, _)) = mesh_source(world) else {
            panic!("the render world carries no MeshSource");
        };
        let size = frame_size(world).unwrap_or(WINDOW_SIZE);
        let camera_entity = world
            .scope_mut(source.entity)
            .spawn(bundle![camera_looking_at(
                size,
                orbit_eye(azimuth),
                ORBIT_TARGET,
                60f32.to_radians(),
                0.1
            ),]);
        world
            .root_scope_mut()
            .spawn(bundle![OnFrame::new(move |world, _entity, frame| {
                azimuth += ORBIT_SPEED * frame.delta_time;
                let size = frame_size(world).unwrap_or(WINDOW_SIZE);
                let camera = camera_looking_at(
                    size,
                    orbit_eye(azimuth),
                    ORBIT_TARGET,
                    60f32.to_radians(),
                    0.1,
                );
                let _ = world
                    .root_scope()
                    .try_query_one::<&mut Camera>(camera_entity)
                    .map(|mut current| {
                        *current.get() = camera;
                    });
            }),]);
    })
}

/// The pixel size the scene's camera builds its projection for: the target the
/// scene's pass drew into on the last frame it was bound, or `None` before it
/// has been bound once.
///
/// A frame carries no size, so the pass states the target's size where it binds
/// attachments — the one place both a windowed swap chain and an offscreen
/// target pass through. The demo's pass letterboxes nothing, so the region the
/// camera frames is the target itself.
fn frame_size(world: &World) -> Option<(u32, u32)> {
    let mut passes = world.root_scope().query::<&FramePass>();
    passes
        .iter()
        .find(|pass| pass.order == SCENE_PASS_ORDER)
        .and_then(|pass| pass.target_size)
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
    // Two tables take turns: `previous` holds what the last snapshot carried,
    // which is what the next step compares against, and `gathered` is filled from
    // the world. Filling and exchanging them rather than building a fresh table
    // each step is what keeps a step whose poses are unchanged allocation-free;
    // the snapshot itself cannot be the working table, because it outlives the
    // step that filled it.
    let mut previous: HashMap<RigidBodyHandle, (Vec3, Quat)> = HashMap::new();
    let mut gathered: HashMap<RigidBodyHandle, (Vec3, Quat)> = HashMap::new();
    sender
        .watch(move |world: &World| {
            gathered.clear();
            if let Some(physics) = world
                .root_scope()
                .query::<(Entity, &PhysicsWorld)>()
                .iter()
                .next()
            {
                for (handle, body) in physics.1.bodies.iter() {
                    if body.is_dynamic() || body.is_kinematic() {
                        gathered.insert(handle, (body.translation(), *body.rotation()));
                    }
                }
            }
            if gathered == previous {
                return None;
            }
            std::mem::swap(&mut previous, &mut gathered);
            // `previous` holds the new poses and `gathered` the buffer the next
            // step refills, so the snapshot takes a copy of what was published:
            // the reader owns it and the collector has to keep comparing.
            Some(GamePhysics {
                transforms: previous.clone(),
            })
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
        Some(WorldState::Stopped | WorldState::Failed(_))
    )
}

/// The clickable rectangles the control panel laid out most recently.
///
/// A headless client uses these positions to send the same pointer input a
/// window receives, without duplicating egui's layout in the client.
#[derive(Default)]
pub struct PanelLayout {
    /// The button rectangles, keyed by their visible labels.
    pub buttons: HashMap<&'static str, unlit3d::egui::egui::Rect>,
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
    panel: Snapshot<EguiTarget>,
    refresh_millihertz: Arc<AtomicU32>,
    input: &InputWriter,
    automatic_drops: bool,
) -> SceneWorlds {
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
        output: match audio_setup {
            AudioSetup::Device => AudioOutputMode::OnRequest,
            _ => AudioOutputMode::Pulled,
        },
        ..AudioConfig::default()
    };
    let audio = spawn_audio_world_with::<R>(host, config);
    // The voice is installed from the gameplay world, as a job whose answer
    // is the entity the scenes' sequences address. The soundfont is kept for
    // that job rather than loaded here, so the decode runs where the job
    // runs instead of stalling the host's setup.
    let (soundfont, autoplay_background) = match audio_setup {
        AudioSetup::Silent => (None, true),
        AudioSetup::Pulled {
            soundfont,
            autoplay_background,
        } => (Some(soundfont), autoplay_background),
        AudioSetup::Device => (Some(unlit3dx_asset_bytes::soundfont().into()), false),
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
    // does not pace itself; a thread of its own wakes as often as the
    // display redraws and sleeps between ticks.
    let gameplay = R::spawn(
        host,
        "gameplay",
        // Nothing sends here after setup: the queue holds the install jobs
        // and any first drops, so the default depth is plenty.
        64,
        {
            let scene = scene.clone();
            let soundfont = soundfont.clone();
            let panel = panel.clone();
            let input = input.clone();
            move |world: &mut World| {
                // This world is a sibling of the host, not a world nested in
                // it, so the host's fan-out never reaches it: it takes its own
                // buffer, once, before the first tick reads one.
                world.root_scope_mut().spawn(input.subscribe());
                world
                    .root_scope_mut()
                    .spawn(bundle![OnFrame::new(|world, _entity, _frame| {
                        dispatch_input(world);
                    }),]);
                // The scene's own camera: it hangs under the mesh source the
                // render world builds on the GPU's answer, which is later than
                // this setup, so it travels as a job and lands before the first
                // frame draws.
                if let Err(error) = scene.render.send(camera_job()) {
                    log::error!("the render world refused its camera ({error:?})");
                }
                // Space drops a ball. The key arrives on this world's own
                // buffer, the way every event it reacts to does.
                world
                    .root_scope_mut()
                    .spawn(bundle![OnKey::new(move |world, _entity, key| {
                        if !key.pressed || key.repeat || key.key != Key::Space {
                            return;
                        }
                        drop_ball_in(&world.root_scope());
                    },),]);
                spawn_floor(&scene);
                world.root_scope_mut().spawn(bundle![scene.clone()]);
                world.root_scope_mut().spawn(SceneBuild::new(&scene));
                world
                    .root_scope_mut()
                    .spawn(BallDrops::new(&scene, automatic_drops));
                if let Some(soundfont) = soundfont {
                    world.root_scope_mut().spawn(Music::new(
                        &scene,
                        soundfont,
                        autoplay_background,
                    ));
                }
                // The panel is the fifth object, and lives here with the rest.
                // The host that lays it out is an entity of its own, so the
                // world it belongs to is the only thing tying the two
                // together. Publishing states an order after every object,
                // since the panel shows what they wrote this frame, and the
                // render world draws whatever is there.
                world.root_scope_mut().spawn(bundle![EguiPanelHost::new(
                    render.sender.clone(),
                    panel.reader(),
                    unlit3dx::app::PANEL_SLOT,
                ),]);
                mount_controls(world);
                world.root_scope_mut().spawn(bundle![OnFrame::with_order(
                    PUBLISH_ORDER,
                    |world, _entity, frame| publish_panels(world, frame)
                ),]);
            }
        },
        {
            let refresh_millihertz = refresh_millihertz.clone();
            let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / GAMEPLAY_HZ));
            let mut index = 0;
            move |world: &mut World, control: &ThreadControl, (), delta_seconds: f32| {
                if control.stop_requested() {
                    return false;
                }
                // A tick is a frame whose duration is the one it measured:
                // the objects read it from the frame like any other
                // behaviour, and nothing paces itself off a clock.
                let mut frame = Frame {
                    delta_time: delta_seconds,
                    index,
                };
                index = index.wrapping_add(1);
                dispatch_frame(world, &mut frame);
                if R::SELF_DRIVEN {
                    // Waking faster than the display redraws only burns
                    // frames nobody sees, so the refresh rate the frame loop
                    // last read sets the pace; a run with no window, or one
                    // whose display reports nothing, keeps the default.
                    let millihertz = refresh_millihertz.load(Ordering::Relaxed);
                    let hz = if millihertz == 0 {
                        GAMEPLAY_HZ
                    } else {
                        f64::from(millihertz) / 1000.0
                    };
                    clock.set_step(Duration::from_secs_f64(1.0 / hz));
                    clock.wait(control);
                }
                true
            }
        },
    );

    SceneWorlds {
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
fn publish_panels(world: &World, frame: &Frame) {
    if let Some((_, host)) = world
        .root_scope()
        .query::<(Entity, &mut EguiPanelHost)>()
        .iter()
        .next()
    {
        host.publish(world, frame);
    }
}

/// Ask the balls to drop the next one.
pub fn drop_ball_in(world: &Scope<'_>) {
    if let Some((_, balls)) = world.query::<(Entity, &mut Balls)>().iter().next() {
        balls.queue_drop();
    }
}

/// Ask the balls to go away, from inside the gameplay world.
///
/// The music keeps playing; the panel drives it.
pub fn clear_balls_in(world: &Scope<'_>) {
    if let Some((_, balls)) = world.query::<(Entity, &mut Balls)>().iter().next() {
        balls.queue_clear();
    }
}

/// The panel buttons the last drawn frame laid out, keyed by label.
///
/// The layout lives on the gameplay world, refreshed on every tick. A headless
/// client uses these positions to send the same pointer input a window
/// receives, without duplicating egui's layout in the client.
pub fn panel_buttons(
    host: &World,
    worlds: &SceneWorlds,
) -> Vec<(&'static str, unlit3d::egui::egui::Rect)> {
    with_stepped_world(host, worlds.gameplay.entity, |world| {
        world
            .root_scope()
            .query::<&PanelLayout>()
            .iter()
            .next()
            .map(|layout| {
                layout
                    .buttons
                    .iter()
                    .map(|(label, rect)| (*label, *rect))
                    .collect()
            })
            .unwrap_or_default()
    })
    .expect("the gameplay world is a stepped world")
}

/// The scene state a caller asserts on: the balls and the sequence.
#[derive(Clone, Copy)]
pub struct SceneStatus {
    /// How many balls the gameplay world holds.
    pub balls: usize,
    /// Whether the MIDI sequence is paused, and where it has got to.
    pub sequence: Option<(bool, f64, bool)>,
}

/// Report the scene state: the balls the gameplay world holds and where its
/// MIDI sequence stands.
pub fn scene_status(host: &World, worlds: &SceneWorlds) -> SceneStatus {
    with_stepped_world(host, worlds.gameplay.entity, |world| {
        let scope = world.root_scope();
        let balls = scope.query::<&Ball>().iter().count();
        let sequence = scope
            .query::<&SceneTracks>()
            .iter()
            .next()
            .and_then(|tracks| {
                scope
                    .try_query_one::<&Track>(tracks.sequence)
                    .ok()
                    .map(|mut track| {
                        let track = track.get();
                        (track.midi.paused, track.midi.time, track.midi.finished)
                    })
            });
        SceneStatus { balls, sequence }
    })
    .expect("the gameplay world is a stepped world")
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use unlit_wgpu::egui::ScreenDescriptor;
    use unlit3d::egui::{EguiPanel, egui};

    /// The target a drawing side publishes once it has measured its pass.
    ///
    /// These tests drive only the gameplay world, so nothing draws and
    /// nothing would otherwise report a size to lay a frame out at.
    fn measured_target() -> EguiTarget {
        EguiTarget {
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
        let panel = Snapshot::new();
        // The target is published before the panel exists to subscribe to it:
        // a snapshot keeps its newest value, so the panel finds it on its
        // first tick instead of waiting for another draw.
        panel.publish(measured_target());
        let refresh = Arc::new(AtomicU32::new(0));
        let worlds = spawn_scene::<Stepped>(
            &mut host,
            render,
            AudioSetup::Silent,
            panel,
            refresh,
            &input,
            true,
        );
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        worlds
            .gameplay
            .sender
            .send(Box::new(move |world: &mut World| {
                world.root_scope_mut().spawn(bundle![EguiPanel::new(
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
            step_stepped_world(&host, worlds.gameplay.entity, FRAME_SECONDS)
                .expect("the stepped world starts");
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
            location: KeyLocation::Standard,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }));
        step_stepped_world(&host, worlds.gameplay.entity, FRAME_SECONDS)
            .expect("the stepped world starts");

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
        let panel = Snapshot::new();
        // Published before the panel exists to subscribe to it, which a
        // snapshot keeps for it.
        panel.publish(measured_target());
        let refresh = Arc::new(AtomicU32::new(0));
        // The wiring is the scene's, but the panel is this test's: it counts
        // the space presses it saw, the way the real one reads a click.
        let worlds = spawn_scene::<Stepped>(
            &mut host,
            render,
            AudioSetup::Silent,
            panel,
            refresh,
            &input,
            true,
        );
        let counted = Arc::clone(&pressed);
        worlds
            .gameplay
            .sender
            .send(Box::new(move |world: &mut World| {
                world.root_scope_mut().spawn(bundle![EguiPanel::new(
                    move |world, _, _ui: &mut egui::Ui| {
                        if let Some(mut row) = world
                            .singleton::<InputEvents>()
                            .and_then(|entity| world.try_query_one::<&InputEvents>(entity).ok())
                        {
                            row.get().with_batch(|batch| {
                                counted.fetch_add(batch.len(), Ordering::Relaxed);
                            });
                        }
                    },
                ),]);
            }))
            .expect("the gameplay world accepts a job");

        input.push(InputEvent::Key(KeyEvent {
            key: Key::Space,
            location: KeyLocation::Standard,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }));
        step_stepped_world(&host, worlds.gameplay.entity, FRAME_SECONDS)
            .expect("the stepped world starts");

        assert_eq!(
            pressed.load(Ordering::Relaxed),
            1,
            "the panel saw the event its tick dispatched"
        );
    }
}
