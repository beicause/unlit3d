//! The physics remote world: a fixed-step rapier simulation on its own thread.
//!
//! Gameplay sends jobs that build bodies; the thread steps the simulation at
//! a fixed rate and publishes a [`PhysicsSnapshot`] — body transforms, plus
//! which contacts began or ended — for whoever reads it back. Rapier's math
//! types are glam aliases, so a snapshot's vectors and rotations cross
//! threads with no conversion.

use std::sync::mpsc::channel;
use std::time::Duration;

use glam::{Quat, Vec3};
use rapier3d::prelude::*;

pub use rapier3d::prelude::{PhysicsWorld, RigidBodyHandle};
use unlit_ecs::{Entity, World};

use crate::channel::{SnapshotReader, SnapshotWriter, snapshot_pair};
use crate::thread::FixedClock;
use crate::world::spawn_remote_world_with;

/// The simulation's default step rate, in hertz.
pub const PHYSICS_HZ: f64 = 64.0;

/// How the physics world is set up before its first step.
#[derive(Clone, Debug)]
pub struct PhysicsConfig {
    /// The step rate, in hertz.
    pub hz: f64,
    /// The gravity every dynamic body feels.
    pub gravity: Vec3,
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self {
            hz: PHYSICS_HZ,
            gravity: Vec3::Y * -9.81,
        }
    }
}

/// One step's worth of state, published to the reader's side.
#[derive(Clone, Debug, Default)]
pub struct PhysicsSnapshot {
    /// Every moving body's transform.
    pub transforms: Vec<(RigidBodyHandle, Vec3, Quat)>,
    /// Contacts that began this step, with the closing speed of the pair.
    pub started: Vec<(RigidBodyHandle, RigidBodyHandle, f32)>,
    /// Contacts that ended this step.
    pub stopped: Vec<(RigidBodyHandle, RigidBodyHandle)>,
}

/// The reader side of the physics world's snapshot mailbox, on the host
/// entity the proxy lives on.
#[derive(Clone, Debug)]
pub struct PhysicsInbox(pub SnapshotReader<PhysicsSnapshot>);

/// A rigid body built by a job, named for whoever built it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhysicsBody(pub RigidBodyHandle);

/// A falling, bouncing ball: the simulation the example drives.
#[derive(Clone, Copy, Debug)]
pub struct BallDesc {
    /// Where the ball starts, in metres.
    pub position: Vec3,
    /// The velocity the ball starts with, in metres per second.
    pub velocity: Vec3,
    /// The ball's radius, in metres.
    pub radius: f32,
    /// How bouncy the ball is, from zero to one.
    pub restitution: f32,
}

/// Spawn a dynamic ball with a collision-event collider, returning its body.
///
/// # Panics
///
/// Panics when the world carries no [`PhysicsWorld`] yet, which only the
/// physics thread's first tick creates.
#[must_use]
pub fn spawn_ball(world: &mut World, desc: BallDesc) -> RigidBodyHandle {
    ensure_physics_world(world);
    let mut physics = physics_world(world);
    let body = physics.insert_body(
        RigidBodyBuilder::dynamic()
            .translation(desc.position)
            .linvel(desc.velocity)
            .build(),
    );
    physics.insert_collider(
        ColliderBuilder::ball(desc.radius)
            .restitution(desc.restitution)
            .active_events(ActiveEvents::COLLISION_EVENTS),
        Some(body),
    );
    body
}

/// Spawn a fixed cuboid: a floor, a wall, whatever gameplay needs.
///
/// # Panics
///
/// Panics when the world carries no [`PhysicsWorld`] yet.
pub fn spawn_fixed_cuboid(world: &mut World, position: Vec3, half_extents: Vec3, friction: f32) {
    ensure_physics_world(world);
    let mut physics = physics_world(world);
    let body = physics.insert_body(RigidBodyBuilder::fixed().translation(position).build());
    physics.insert_collider(
        ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z).friction(friction),
        Some(body),
    );
}

/// The physics world as a mutable guard, for jobs that build bodies.
fn physics_world(world: &World) -> unlit_ecs::CellRefMut<'_, PhysicsWorld> {
    world
        .query::<&mut PhysicsWorld>()
        .next()
        .expect("the physics world exists")
        .1
}

/// Make sure a physics world exists before a job touches it.
///
/// Jobs drain before the first tick, so a job may arrive while the tick has
/// not yet created the world. One created here carries the default gravity;
/// the tick's own creation applies the configured gravity.
fn ensure_physics_world(world: &mut World) {
    if world.query::<&PhysicsWorld>().next().is_none() {
        world.spawn((PhysicsWorld::new(),));
    }
}

/// Spawn the physics remote world on a fresh thread, and return the proxy
/// entity carrying its command channel and [`PhysicsInbox`].
///
/// The thread owns the rapier world as a [`PhysicsWorld`] component.
/// Gameplay never touches it: jobs build bodies, and every step publishes a
/// snapshot the host reads through the inbox. The caller usually opens with
/// a job spawning the static geometry, since jobs only run after the
/// thread's first tick.
pub fn spawn_physics_world(host: &mut World, config: PhysicsConfig) -> Entity {
    let (writer, reader) = snapshot_pair::<PhysicsSnapshot>();
    let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / config.hz));
    spawn_remote_world_with(
        host,
        "physics",
        64,
        PhysicsInbox(reader),
        move |world, control| {
            if control.stop_requested() {
                return false;
            }
            if world.query::<&PhysicsWorld>().next().is_none() {
                let mut physics = PhysicsWorld::new();
                physics.gravity = config.gravity;
                world.spawn((physics,));
            }
            step_world(world, &writer);
            clock.wait(control);
            true
        },
    )
}

/// Step the rapier world once and publish what happened.
fn step_world(world: &World, writer: &SnapshotWriter<PhysicsSnapshot>) {
    if let Some(snapshot) = step_once(world) {
        writer.publish(snapshot);
    }
}

/// Step the rapier world once and return the snapshot a threaded world would
/// publish. Thread-free callers — deterministic tests, offline rendering —
/// drive the same simulation on a cadence of their own.
pub fn step_once(world: &World) -> Option<PhysicsSnapshot> {
    let mut physics = world.query::<&mut PhysicsWorld>().next()?.1;
    let (collision_sender, collision_receiver) = channel();
    // The unused event kinds just vanish: a dropped receiver discards them.
    let (force_sender, force_receiver) = channel::<ContactForceEvent>();
    drop(force_receiver);
    let (tear_sender, tear_receiver) = channel::<SoftBodyTearEvent>();
    drop(tear_receiver);
    let collector = ChannelEventCollector::new(collision_sender, force_sender, tear_sender);
    physics.step_with_events(&(), &collector);
    let (mut transforms, mut started, mut stopped) = (Vec::new(), Vec::new(), Vec::new());
    for (handle, body) in physics.bodies.iter() {
        if body.is_dynamic() || body.is_kinematic() {
            transforms.push((handle, body.translation(), *body.rotation()));
        }
    }
    while let Ok(event) = collision_receiver.try_recv() {
        let (one, two) = match event {
            CollisionEvent::Started(one, two, _) => (one, two),
            CollisionEvent::Stopped(one, two, _) => (one, two),
        };
        let Some((body_one, body_two)) = body_pair(&physics, one, two) else {
            continue;
        };
        match event {
            CollisionEvent::Started(..) => {
                let speed = contact_speed(&physics, one, two);
                started.push((body_one, body_two, speed));
            }
            _ => stopped.push((body_one, body_two)),
        }
    }
    Some(PhysicsSnapshot {
        transforms,
        started,
        stopped,
    })
}

/// The speed two colliders close at, from the difference of their bodies'
/// linear velocities.
fn contact_speed(physics: &PhysicsWorld, one: ColliderHandle, two: ColliderHandle) -> f32 {
    let velocity = |handle: ColliderHandle| {
        physics
            .colliders
            .get(handle)
            .and_then(|collider| collider.parent())
            .and_then(|body| physics.bodies.get(body))
            .map_or(Vec3::ZERO, |body| body.linvel())
    };
    (velocity(one) - velocity(two)).length()
}

/// The bodies a collider pair belongs to, or nothing when either side is
/// parentless.
fn body_pair(
    physics: &PhysicsWorld,
    one: ColliderHandle,
    two: ColliderHandle,
) -> Option<(RigidBodyHandle, RigidBodyHandle)> {
    let body = |handle: ColliderHandle| physics.colliders.get(handle)?.parent();
    Some((body(one)?, body(two)?))
}
