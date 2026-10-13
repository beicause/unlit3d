//! The physics remote world: a fixed-step rapier simulation on its own thread.
//!
//! Gameplay sends jobs that build bodies; the thread steps the simulation at
//! a fixed rate and publishes a [`PhysicsSnapshot`] — body transforms, plus
//! which contacts began or ended — for whoever reads it back. Rapier's math
//! types are glam aliases, so a snapshot's vectors and rotations cross
//! threads with no conversion.

use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use glam::{Quat, Vec3};
use rapier3d::prelude::*;

pub use rapier3d::prelude::{PhysicsWorld, RigidBodyHandle};
use unlit_ecs::World;

use crate::channel::{SnapshotReader, SnapshotWriter, snapshot_pair};
use crate::handle::WorldHandle;
use crate::thread::FixedClock;
use crate::world::spawn_remote_world_with_init;

/// The simulation's default step rate, in hertz.
pub const PHYSICS_HZ: f64 = 64.0;

/// How the physics world is set up before its first step.
#[derive(Clone, Debug)]
pub struct PhysicsConfig {
    /// The step rate, in hertz.
    pub hz: f64,
    /// The gravity every dynamic body feels.
    pub gravity: Vec3,
    /// The command channel's capacity, in queued jobs.
    pub command_capacity: usize,
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self {
            hz: PHYSICS_HZ,
            gravity: Vec3::Y * -9.81,
            command_capacity: 64,
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

/// The event plumbing a physics world carries between steps.
///
/// Rapier reports contacts through channels: the collector it is stepped
/// with owns the sending ends, and this component keeps the collector and
/// the receiving end of the collision channel alive across steps, so each
/// step reuses them instead of allocating fresh channels.
pub struct PhysicsEvents {
    receiver: Receiver<CollisionEvent>,
    collector: ChannelEventCollector,
    /// The moving bodies' velocities, sampled before the step so a contact
    /// can be reported with the speed the pair was closing at.
    velocities: Vec<(RigidBodyHandle, Vec3)>,
}

impl PhysicsEvents {
    fn new() -> Self {
        let (collision_sender, receiver) = channel();
        // The unused event kinds just vanish: a dropped receiver discards them.
        let (force_sender, force_receiver) = channel::<ContactForceEvent>();
        drop(force_receiver);
        let (tear_sender, tear_receiver) = channel::<SoftBodyTearEvent>();
        drop(tear_receiver);
        Self {
            receiver,
            collector: ChannelEventCollector::new(collision_sender, force_sender, tear_sender),
            velocities: Vec::new(),
        }
    }

    /// Record the moving bodies' velocities before the step runs.
    fn sample_velocities(&mut self, physics: &PhysicsWorld) {
        self.velocities.clear();
        self.velocities.extend(
            physics
                .bodies
                .iter()
                .filter(|(_, body)| body.is_dynamic() || body.is_kinematic())
                .map(|(handle, body)| (handle, body.linvel())),
        );
    }

    /// The velocity a body had before the step, or zero if it was still.
    fn velocity(&self, body: RigidBodyHandle) -> Vec3 {
        self.velocities
            .iter()
            .find(|(known, _)| *known == body)
            .map_or(Vec3::ZERO, |(_, velocity)| *velocity)
    }
}

/// The event plumbing a physics world carries, if it has any.
fn physics_events(world: &World) -> Option<unlit_ecs::CellRefMut<'_, PhysicsEvents>> {
    world
        .query::<&mut PhysicsEvents>()
        .next()
        .map(|(_, events)| events)
}

/// The collider shape a dynamic body carries.
#[derive(Clone, Copy, Debug)]
pub enum BodyShape {
    /// A sphere of the given radius, in metres.
    Ball(f32),
    /// An axis-aligned box of the given half extents, in metres.
    Cuboid(Vec3),
    /// A cylinder along the y axis.
    Cylinder {
        /// Half the height, in metres.
        half_height: f32,
        /// The radius, in metres.
        radius: f32,
    },
    /// A cone pointing up the y axis.
    Cone {
        /// Half the height, in metres.
        half_height: f32,
        /// The base radius, in metres.
        radius: f32,
    },
}

/// A falling, bouncing body: the simulation the example drives.
#[derive(Clone, Copy, Debug)]
pub struct BodyDesc {
    /// Where the body starts, in metres.
    pub position: Vec3,
    /// The velocity the body starts with, in metres per second.
    pub velocity: Vec3,
    /// How bouncy the body is, from zero to one.
    pub restitution: f32,
    /// The collider shape the body carries.
    pub shape: BodyShape,
}

/// Spawn a dynamic body with a collision-event collider, returning it.
///
/// # Panics
///
/// Panics when the world carries no [`PhysicsWorld`]. The physics world is
/// created before any job runs, so jobs sent through
/// [`spawn_physics_world`] never see this.
#[must_use]
pub fn spawn_body(world: &mut World, desc: BodyDesc) -> RigidBodyHandle {
    let mut physics = physics_world(world);
    let body = physics.insert_body(
        RigidBodyBuilder::dynamic()
            .translation(desc.position)
            .linvel(desc.velocity)
            .build(),
    );
    let collider = match desc.shape {
        BodyShape::Ball(radius) => ColliderBuilder::ball(radius),
        BodyShape::Cuboid(half) => ColliderBuilder::cuboid(half.x, half.y, half.z),
        BodyShape::Cylinder {
            half_height,
            radius,
        } => ColliderBuilder::cylinder(half_height, radius),
        BodyShape::Cone {
            half_height,
            radius,
        } => ColliderBuilder::cone(half_height, radius),
    };
    physics.insert_collider(
        collider
            .restitution(desc.restitution)
            .active_events(ActiveEvents::COLLISION_EVENTS),
        Some(body),
    );
    body
}

/// Remove a body and its colliders from the physics world.
///
/// Returns whether the handle named a body. A caller that recycles a pool
/// of bodies — a shape that has rested long enough to be replaced — calls
/// this to give the simulation its memory back; the ground a scene builds
/// once is left alone.
///
/// # Panics
///
/// Panics when the world carries no [`PhysicsWorld`].
#[must_use]
pub fn despawn_body(world: &World, body: RigidBodyHandle) -> bool {
    physics_world(world).remove_body(body).is_some()
}

/// Spawn a fixed cuboid: a floor, a wall, whatever gameplay needs.
///
/// # Panics
///
/// Panics when the world carries no [`PhysicsWorld`].
pub fn spawn_fixed_cuboid(world: &mut World, position: Vec3, half_extents: Vec3, friction: f32) {
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

/// Spawn the physics remote world on a fresh thread, and return the handle
/// to it. Its entity carries the command channel and [`PhysicsInbox`].
///
/// The thread owns the rapier world as a [`PhysicsWorld`] component.
/// Gameplay never touches it: jobs build bodies, and every step publishes a
/// snapshot the host reads through the inbox. The world is created before
/// the first job runs, so a job that builds a body — the static geometry,
/// usually — finds it ready.
pub fn spawn_physics_world(host: &mut World, config: PhysicsConfig) -> WorldHandle {
    let (writer, reader) = snapshot_pair::<PhysicsSnapshot>();
    let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / config.hz));
    spawn_remote_world_with_init(
        host,
        "physics",
        config.command_capacity,
        (PhysicsInbox(reader),),
        move |world| init_physics_world(world, &config),
        move |world, control, ()| {
            if control.stop_requested() {
                return false;
            }
            step_world(world, &writer);
            clock.wait(control);
            true
        },
    )
}

/// Create the physics world and its event plumbing in `world`, with
/// `config` applied.
///
/// Thread-free callers that drive [`step_once`] on their own cadence —
/// deterministic tests, offline rendering — call this to get the same world
/// the threaded [`spawn_physics_world`] builds, contact events included.
pub fn init_physics_world(world: &mut World, config: &PhysicsConfig) {
    let mut physics = PhysicsWorld::new();
    physics.gravity = config.gravity;
    world.spawn((physics, PhysicsEvents::new()));
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
    // The contact events rapier reports this step describe contacts that
    // began during it, and by the time the step returns the solver has
    // already turned the approach into a bounce. Sampling the velocities
    // before the step is what makes the reported speed the approach speed
    // rather than the separation it left behind.
    let mut events = physics_events(world);
    if let Some(events) = events.as_mut() {
        events.sample_velocities(&physics);
    }
    match events.as_ref() {
        Some(events) => physics.step_with_events(&(), &events.collector),
        None => physics.step(),
    }
    let (mut transforms, mut started, mut stopped) = (Vec::new(), Vec::new(), Vec::new());
    for (handle, body) in physics.bodies.iter() {
        if body.is_dynamic() || body.is_kinematic() {
            transforms.push((handle, body.translation(), *body.rotation()));
        }
    }
    let Some(events) = events.as_ref() else {
        return Some(PhysicsSnapshot {
            transforms,
            started,
            stopped,
        });
    };
    while let Ok(event) = events.receiver.try_recv() {
        let (one, two) = match event {
            CollisionEvent::Started(one, two, _) => (one, two),
            CollisionEvent::Stopped(one, two, _) => (one, two),
        };
        let Some((body_one, body_two)) = body_pair(&physics, one, two) else {
            continue;
        };
        match event {
            CollisionEvent::Started(..) => {
                let speed = contact_speed(&physics, events, one, two);
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

/// The speed two colliders close at along their contact normal, from the
/// velocities sampled before the step that reported the contact.
///
/// Rapier reports the normal in the first collider's local frame, so it is
/// rotated into world space before the relative velocity is projected onto
/// it. The result is negative while the pair is separating.
fn contact_speed(
    physics: &PhysicsWorld,
    events: &PhysicsEvents,
    one: ColliderHandle,
    two: ColliderHandle,
) -> f32 {
    // The pair stores its colliders in the broad phase's own order, which
    // need not match the event's order: the manifold normal belongs to the
    // pair's first collider, so project the pair's own relative velocity.
    let Some(pair) = physics.contact_pair(one, two) else {
        return 0.0;
    };
    let (first, second) = (pair.collider1, pair.collider2);
    let normal = pair
        .manifolds()
        .first()
        .map(|manifold| {
            let rotation = physics
                .colliders
                .get(first)
                .map_or(Quat::IDENTITY, |collider| collider.rotation());
            rotation * manifold.local_n1
        })
        .unwrap_or(Vec3::ZERO);
    let velocity = |handle: ColliderHandle| {
        physics
            .colliders
            .get(handle)
            .and_then(|collider| collider.parent())
            .map_or(Vec3::ZERO, |body| events.velocity(body))
    };
    normal.dot(velocity(first) - velocity(second))
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
