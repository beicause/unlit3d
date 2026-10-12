//! Integration tests for the physics remote world: bodies built by jobs,
//! snapshots read back on the host, contacts reported as events.

use std::time::Duration;

use glam::Vec3;
use unlit_ecs::World;
use unlit3dx::physics::PhysicsWorld;
use unlit3dx::prelude::*;

/// The floor the tests drop things onto, with its top surface at `y = 0`.
const FLOOR_POSITION: Vec3 = Vec3::new(0.0, -0.5, 0.0);
const FLOOR_HALF: Vec3 = Vec3::new(8.0, 0.5, 8.0);
/// The ball the tests drop, starting one metre above the floor.
const BALL_START: Vec3 = Vec3::new(0.0, 1.0, 0.0);
const BALL_RADIUS: f32 = 0.25;

/// A physics world at the default rate, with the floor already queued.
fn floored_host() -> (World, unlit_ecs::Entity) {
    let mut host = World::new();
    let world = spawn_physics_world(&mut host, PhysicsConfig::default());
    let sender = host
        .get::<RemoteWorld>(world)
        .expect("the proxy carries its commands")
        .sender();
    sender
        .send(Box::new(move |world: &mut World| {
            spawn_fixed_cuboid(world, FLOOR_POSITION, FLOOR_HALF, 0.9);
        }))
        .expect("the physics world accepts jobs");
    (host, world)
}

/// The newest snapshot from the physics inbox, if one has landed.
fn latest(host: &World, world: unlit_ecs::Entity) -> Option<std::sync::Arc<PhysicsSnapshot>> {
    host.get::<PhysicsInbox>(world)
        .expect("the proxy carries the inbox")
        .0
        .latest()
}

/// Waits until `probe` answers `true`, giving up after five seconds.
fn wait_for(mut probe: impl FnMut() -> bool) -> bool {
    for _ in 0..5_000 {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// A dropped ball lands on the floor and stays there: its snapshot
/// transform settles just above the surface, and the landing surfaces as
/// a contact-started event with a real closing speed.
#[test]
fn a_dropped_ball_lands_and_reports_the_contact() {
    let (host, world) = floored_host();
    let sender = host
        .get::<RemoteWorld>(world)
        .expect("the proxy carries its commands")
        .sender();
    sender
        .send(Box::new(move |world: &mut World| {
            let _ball = spawn_ball(
                world,
                BallDesc {
                    position: BALL_START,
                    velocity: Vec3::ZERO,
                    radius: BALL_RADIUS,
                    restitution: 0.3,
                },
            );
        }))
        .expect("the physics world accepts jobs");

    // The ball must appear in the snapshots, fall below its start, touch
    // the floor audibly and come to rest a radius above the surface.
    let landed = wait_for(|| {
        let Some(snapshot) = latest(&host, world) else {
            return false;
        };
        let touched = snapshot.started.iter().any(|&(_, _, speed)| speed > 0.5);
        let resting = snapshot.transforms.iter().any(|&(_, translation, _)| {
            (translation.y - BALL_RADIUS).abs() < 0.05 && translation.y < BALL_START.y
        });
        touched && resting
    });
    assert!(
        landed,
        "the ball never landed: the last snapshot is {latest:?}",
        latest = latest(&host, world).map(|snapshot| snapshot.transforms.clone()),
    );
}

/// A world with no bodies still steps and publishes empty snapshots, so
/// readers can tell a quiet world from a stopped one.
#[test]
fn an_empty_world_publishes_empty_snapshots() {
    let mut host = World::new();
    let world = spawn_physics_world(&mut host, PhysicsConfig::default());

    assert!(
        wait_for(|| latest(&host, world).is_some()),
        "the empty world never published a snapshot"
    );
    let snapshot = latest(&host, world).expect("the snapshot exists");
    assert!(snapshot.transforms.is_empty());
    assert!(snapshot.started.is_empty());
    assert!(snapshot.stopped.is_empty());
}

/// The world the thread's first tick creates carries the configured
/// gravity, which a job can read back through a request.
#[test]
fn the_configured_gravity_reaches_the_world() {
    let gravity = Vec3::new(0.0, -1.5, 0.0);
    let mut host = World::new();
    let world = spawn_physics_world(
        &mut host,
        PhysicsConfig {
            gravity,
            ..PhysicsConfig::default()
        },
    );
    let probed = {
        // Jobs drain before the thread's first tick, so wait for that
        // tick's first snapshot: only then does the world exist.
        assert!(
            wait_for(|| latest(&host, world).is_some()),
            "the physics thread never ran its first tick"
        );
        let remote = host
            .get::<RemoteWorld>(world)
            .expect("the proxy carries its commands");
        remote.request(Box::new(|world: &mut World| {
            world
                .query::<&PhysicsWorld>()
                .next()
                .expect("the tick creates the world")
                .1
                .gravity
        }))
    };

    // Polling consumes the answer, so keep the first one around.
    let mut answer = None;
    assert!(
        wait_for(|| {
            answer = probed.poll();
            answer.is_some()
        }),
        "the gravity request never answered"
    );
    assert_eq!(answer, Some(gravity));
}
