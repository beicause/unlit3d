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
/// How many steps a thread-free world is driven for: long enough for the
/// ball to reach the floor, bounce and come to rest.
const STEPS: usize = 256;

/// A physics world at the default rate, with the floor already queued.
fn floored_host() -> (World, WorldHandle) {
    let mut host = World::new();
    let world = spawn_physics_world_with::<Threaded>(&mut host, PhysicsConfig::default());
    world
        .send(Box::new(move |world: &mut World| {
            spawn_fixed_cuboid(world, FLOOR_POSITION, FLOOR_HALF, 0.9);
        }))
        .expect("the physics world accepts jobs");
    (host, world)
}

/// The newest snapshot from the physics inbox, if one has landed.
fn latest(host: &World, world: &WorldHandle) -> Option<std::sync::Arc<PhysicsSnapshot>> {
    host.get::<PhysicsInbox>(world.entity())
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

/// The threaded world does the whole job round trip: a body built by a job
/// falls, and its snapshots carry it down to rest a radius above the floor.
///
/// The contact itself is not checked here. A contact-started event appears in
/// exactly one snapshot, and the mailbox holds only the newest, so a reader
/// that is not scheduled within one step of the landing misses it. The speed
/// a landing reports is what [`a_landing_reports_the_closing_speed`] checks,
/// on a world stepped in this thread where no step can be skipped.
#[test]
fn a_dropped_ball_settles_in_its_snapshots() {
    let (host, world) = floored_host();
    world
        .send(Box::new(move |world: &mut World| {
            let _ball = spawn_body(world, ball_desc());
        }))
        .expect("the physics world accepts jobs");

    let rested = wait_for(|| {
        let Some(snapshot) = latest(&host, &world) else {
            return false;
        };
        snapshot.transforms.iter().any(|&(_, translation, _)| {
            (translation.y - BALL_RADIUS).abs() < 0.05 && translation.y < BALL_START.y
        })
    });
    assert!(
        rested,
        "the ball never came to rest: the last snapshot is {latest:?}",
        latest = latest(&host, &world).map(|snapshot| snapshot.transforms.clone()),
    );
}

/// The ball every drop test releases: still, resting on nothing, and bouncy
/// enough that it leaves the floor again rather than sticking to it.
fn ball_desc() -> BodyDesc {
    BodyDesc {
        position: BALL_START,
        velocity: Vec3::ZERO,
        restitution: 0.3,
        shape: BodyShape::Ball(BALL_RADIUS),
    }
}

/// A landing is reported as the speed the pair closed at, not the speed it
/// separated at.
///
/// Rapier's solver runs inside the step, so the body's velocity once the step
/// returns is the bounce. The snapshot has to carry the approach instead,
/// which is what this asserts: free fall over the gap between the ball and the
/// floor's surface reaches the floor at about 3.8 m/s, while the 0.3
/// restitution leaves it separating at about 1.2 m/s.
#[test]
fn a_landing_reports_the_closing_speed() {
    let mut world = World::new();
    init_physics_world(&mut world, &PhysicsConfig::default());
    spawn_fixed_cuboid(&mut world, FLOOR_POSITION, FLOOR_HALF, 0.9);
    let ball = spawn_body(&mut world, ball_desc());

    // The gap the ball falls through, from its centre to a radius above the
    // floor's surface, and the speed that gap implies under this world's
    // own gravity.
    let gravity = PhysicsConfig::default().gravity.length();
    let surface = FLOOR_POSITION.y + FLOOR_HALF.y;
    let fall = BALL_START.y - BALL_RADIUS - surface;
    let reaching = (2.0 * gravity * fall).sqrt();

    let mut landings = Vec::new();
    let mut height = BALL_START.y;
    for _ in 0..STEPS {
        let snapshot = step_once(&world).expect("the world carries a physics world");
        landings.extend(snapshot.started.iter().copied());
        height = snapshot
            .transforms
            .iter()
            .find(|(handle, _, _)| *handle == ball)
            .map_or(height, |(_, translation, _)| translation.y);
    }

    assert!(!landings.is_empty(), "the ball never touched the floor");
    let (one, two, speed) = landings[0];
    let partner = if one == ball { two } else { one };
    assert_ne!(partner, ball, "a landing names two different bodies");
    let physics = world
        .query::<&PhysicsWorld>()
        .next()
        .expect("the setup created the world")
        .1;
    let Some(partner_body) = physics.bodies.get(partner) else {
        panic!("the body the ball landed on is not in the world");
    };
    assert!(
        partner_body.is_fixed(),
        "the ball landed on something other than the floor"
    );
    // The reported speed is the approach, not the separation. Rapier
    // integrates in fixed steps — velocity first, then position — so the
    // discrete fall overshoots the continuous one by roughly a step, and
    // contact prediction reports it a step early. Both effects stay inside
    // a tenth of the analytic speed; the bounce this test exists to rule out
    // is a factor of the restitution below it.
    let bounce = reaching * 0.3;
    assert!(
        speed > reaching * 0.9 && speed < reaching * 1.25,
        "the first landing reported {speed}: the analytic closing speed is \
         {reaching} and the bounce away would be about {bounce}"
    );
    assert!(
        speed > bounce * 2.0,
        "the first landing reported the separation speed {speed}, not the approach"
    );
    assert!(
        (height - BALL_RADIUS).abs() < 0.05,
        "the ball ended at {height}, not resting a radius above the surface"
    );
}

/// A world with no bodies still steps and publishes empty snapshots, so
/// readers can tell a quiet world from a stopped one.
#[test]
fn an_empty_world_publishes_empty_snapshots() {
    let mut host = World::new();
    let world = spawn_physics_world_with::<Threaded>(&mut host, PhysicsConfig::default());

    assert!(
        wait_for(|| latest(&host, &world).is_some()),
        "the empty world never published a snapshot"
    );
    let snapshot = latest(&host, &world).expect("the snapshot exists");
    assert!(snapshot.transforms.is_empty());
    assert!(snapshot.started.is_empty());
    assert!(snapshot.stopped.is_empty());
}

/// The one-time setup runs before any job, so a probe sent immediately
/// after the spawn reads back the configured gravity without waiting for
/// a first snapshot.
#[test]
fn the_configured_gravity_reaches_the_world() {
    let gravity = Vec3::new(0.0, -1.5, 0.0);
    let mut host = World::new();
    let world = spawn_physics_world_with::<Threaded>(
        &mut host,
        PhysicsConfig {
            gravity,
            ..PhysicsConfig::default()
        },
    );
    // Sent before any snapshot has been published: the setup phase must
    // already have created the world, so this answers without waiting.
    let probed = world.request(Box::new(|world: &mut World| {
        world
            .query::<&PhysicsWorld>()
            .next()
            .expect("the setup creates the world before any job")
            .1
            .gravity
    }));

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
