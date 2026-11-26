//! Integration tests for the physics world: bodies built by jobs,
//! snapshots subscribed to on the host, contacts reported as events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glam::{Quat, Vec3};
use unlit_ecs::{World, bundle};
use unlit3d::frame::{Frame, OnFrame};
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

/// The test's own snapshot payload: every body's pose.
type Poses = Vec<(RigidBodyHandle, Vec3, Quat)>;

/// Collect the moving bodies' poses, the snapshot the tests subscribe to.
///
/// The library defines no payload of its own, so the demo and the tests each
/// name the state they want: this is the whole of the subscription's work.
fn collect_poses(world: &World) -> Option<Poses> {
    let mut physics_world_rows = world.root_scope().query::<&PhysicsWorld>();

    let physics = physics_world_rows.iter().next()?;
    Some(
        physics
            .bodies
            .iter()
            .filter(|(_, body)| body.is_dynamic() || body.is_kinematic())
            .map(|(handle, body)| (handle, body.translation(), *body.rotation()))
            .collect(),
    )
}

/// A physics world at the default rate, driven by `R`, with the floor already
/// queued and a pose subscription on the host.
fn floored_host<R: WorldRunner>() -> (
    World,
    WorldEndpoint,
    Subscription<SnapshotReader<Poses, unlit3dx::world::WorldError>>,
) {
    let mut host = World::new();
    let world = spawn_physics_world_with::<R>(&mut host, PhysicsConfig::default())
        .expect("the world thread starts");
    let reader = world
        .commands()
        .clone()
        .watch(collect_poses)
        .expect("the physics world accepts a subscription");
    world
        .commands()
        .send(Box::new(move |world: &mut World| {
            spawn_fixed_cuboid(world, FLOOR_POSITION, FLOOR_HALF, 0.9);
        }))
        .expect("the physics world accepts jobs");
    (host, world, reader)
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

/// The reader of a subscription a world the caller steps has installed.
fn installed<R: Clone>(subscription: &Subscription<R>) -> R {
    subscription
        .poll()
        .expect("the world installed the subscription")
        .expect("the subscription outlet is reachable")
}

/// The whole job round trip: a body built by a job falls, and the
/// subscription carries it down to rest a radius above the floor.
///
/// The contact itself is not checked here. A contact-started event appears in
/// exactly one snapshot, and the mailbox holds only the newest, so a reader
/// that is not scheduled within one step of the landing misses it. The speed
/// a landing reports is what [`a_landing_reports_the_closing_speed`] checks.
///
/// The host steps this world itself rather than letting a thread pace it: a
/// stepped world simulates exactly as many steps as the test asks for, so the
/// fall to rest cannot be starved out of a wall-clock window on a slow machine.
#[test]
fn a_dropped_ball_settles_in_its_snapshots() {
    let (host, world, poses) = floored_host::<Stepped>();
    world
        .commands()
        .send(Box::new(move |world: &mut World| {
            let _ball = spawn_body(world, ball_desc());
        }))
        .expect("the physics world accepts jobs");

    let settled = |poses: &Poses| {
        poses.iter().any(|&(_, translation, _)| {
            (translation.y - BALL_RADIUS).abs() < 0.05 && translation.y < BALL_START.y
        })
    };
    let mut poses_reader = None;
    let mut rested = false;
    for _ in 0..STEPS {
        step_stepped_world(&host, world.entity(), FIXED_FRAME_DELTA)
            .expect("the stepped world starts");
        let reader = poses_reader.get_or_insert_with(|| installed(&poses));
        rested = reader.latest().is_some_and(|poses| settled(&poses));
        if rested {
            break;
        }
    }
    assert!(
        rested,
        "the ball never came to rest: the last snapshot is {:?}",
        poses_reader.map(|reader| reader.latest()),
    );
}

/// A world on its own thread steps what a job builds: the job runs on that
/// thread, its tick simulates the body, and the subscription carries the
/// result back to the host.
///
/// The ball is only checked to be falling, not to have settled: a rest takes
/// seconds of simulated time, which would make this a race against how fast
/// the machine is. The fall to rest is the test above, on a world the host
/// steps; this one covers the thread mode at all.
#[test]
fn a_threaded_world_steps_what_a_job_builds() {
    let (_host, world, poses) = floored_host::<Threaded>();
    world
        .commands()
        .send(Box::new(move |world: &mut World| {
            let _ball = spawn_body(world, ball_desc());
        }))
        .expect("the physics world accepts jobs");

    let falling = |poses: &Poses| {
        poses
            .iter()
            .any(|&(_, translation, _)| translation.y < BALL_START.y - 0.05)
    };
    let mut poses_reader = None;
    let installed_here = wait_for(|| match poses.poll() {
        Some(reader) => {
            poses_reader = Some(reader.expect("the subscription outlet is reachable"));
            true
        }
        None => false,
    });
    assert!(
        installed_here,
        "the physics world never installed the subscription"
    );
    let poses = poses_reader.expect("the world installed the subscription");
    assert!(
        wait_for(|| poses.latest().is_some_and(|poses| falling(&poses))),
        "the threaded world never stepped the body its job built: {:?}",
        poses.latest(),
    );
}

/// A physics world dispatches its own frame: a tick is its frame, so a
/// behaviour pacing itself by the simulation sees one frame per tick.
#[test]
fn a_physics_tick_dispatches_its_frame() {
    let mut host = World::new();
    let world = spawn_physics_world_with::<Stepped>(&mut host, PhysicsConfig::default())
        .expect("a stepped world needs no thread");
    let seen: Arc<Mutex<Vec<(u32, f32)>>> = Arc::default();
    let record = Arc::clone(&seen);
    world
        .commands()
        .send(Box::new(move |world: &mut World| {
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                move |_world, _entity, frame: &mut Frame| {
                    record
                        .lock()
                        .unwrap()
                        .push((frame.index, frame.delta_time.as_secs_f32()));
                },
            ),]);
        }))
        .expect("the physics world accepts jobs");
    for _ in 0..3 {
        step_stepped_world(&host, world.entity(), FIXED_FRAME_DELTA)
            .expect("the stepped world starts");
    }
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (0, FIXED_FRAME_DELTA.as_secs_f32()),
            (1, FIXED_FRAME_DELTA.as_secs_f32()),
            (2, FIXED_FRAME_DELTA.as_secs_f32())
        ],
        "a frame per tick, indexed by the tick it ran in"
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

/// A subscriber sees the contacts of every step, which a mailbox cannot.
///
/// Contacts are events: a step's contacts are overwritten by the next step's,
/// so a reader that is not scheduled within one step of a landing misses it.
/// Two balls dropped from different heights land on different steps, and the
/// collector must report both — a newest-wins mailbox polled at the end could
/// hold only one. That is what a game needs for something like bullet damage.
///
/// The host steps this world itself rather than letting a thread pace it: a
/// stepped world simulates exactly as many steps as the test asks for, so the
/// landings cannot be starved out of a wall-clock window on a slow machine.
#[test]
fn a_collector_sees_contacts_from_every_step() {
    let mut host = World::new();
    let world = spawn_physics_world_with::<Stepped>(&mut host, PhysicsConfig::default())
        .expect("a stepped world needs no thread");
    let sender = world.commands().clone();
    let (queue, contacts) = std::sync::mpsc::channel();
    // The collector queues what it sees and publishes nothing: the
    // subscription's state is the queue, which is the app's to own. The
    // reader is only the subscription's liveness token here.
    let _events: Subscription<SnapshotReader<(), unlit3dx::world::WorldError>> = sender
        .watch(move |world: &World| {
            if let Some(contacts) = world.root_scope().query::<&PhysicsContacts>().iter().next() {
                for &contact in &contacts.started {
                    let _ = queue.send(contact);
                }
            }
            None
        })
        .expect("the physics world accepts a subscription");
    // Two balls, far enough apart in height that their landings fall in
    // different steps.
    let built = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&built);
    sender
        .send(Box::new(move |world: &mut World| {
            spawn_fixed_cuboid(world, FLOOR_POSITION, FLOOR_HALF, 0.9);
            let _ = spawn_body(
                world,
                BodyDesc {
                    position: Vec3::new(-1.0, BALL_START.y, 0.0),
                    restitution: 0.6,
                    ..ball_desc()
                },
            );
            let _ = spawn_body(
                world,
                BodyDesc {
                    position: Vec3::new(1.0, BALL_START.y + 4.0, 0.0),
                    restitution: 0.6,
                    ..ball_desc()
                },
            );
            seen.store(true, Ordering::SeqCst);
        }))
        .expect("the physics world accepts jobs");

    let mut landed: Vec<RigidBodyHandle> = Vec::new();
    for _ in 0..STEPS {
        step_stepped_world(&host, world.entity(), FIXED_FRAME_DELTA)
            .expect("the stepped world starts");
        while let Ok((one, two, _speed)) = contacts.try_recv() {
            for body in [one, two] {
                if !landed.contains(&body) {
                    landed.push(body);
                }
            }
        }
        if landed.len() == 3 {
            break;
        }
    }
    // Both balls and the floor: three bodies, so both landings were reported.
    assert_eq!(
        landed.len(),
        3,
        "the collector saw {landed:?}, so a landing was dropped"
    );
    assert!(
        built.load(Ordering::SeqCst),
        "the bodies were built before the contacts arrived"
    );
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
        assert!(step_once(&world), "the world carries a physics world");
        // The contacts live in the world until the next step overwrites
        // them, so read them before stepping again.
        {
            let mut physics_contacts_rows = world.root_scope().query::<&PhysicsContacts>();

            let contacts = physics_contacts_rows
                .iter()
                .next()
                .expect("the setup creates the contact buffer");
            landings.extend(contacts.started.iter().copied());
        }
        let mut physics_world_rows_2 = world.root_scope().query::<&PhysicsWorld>();

        let physics = physics_world_rows_2
            .iter()
            .next()
            .expect("the setup created the world");
        height = physics
            .bodies
            .get(ball)
            .map_or(height, |body| body.translation().y);
    }

    assert!(!landings.is_empty(), "the ball never touched the floor");
    let (one, two, speed) = landings[0];
    let partner = if one == ball { two } else { one };
    assert_ne!(partner, ball, "a landing names two different bodies");
    let mut physics_world_rows_3 = world.root_scope().query::<&PhysicsWorld>();

    let physics = physics_world_rows_3
        .iter()
        .next()
        .expect("the setup created the world");
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

/// A world with no bodies still steps and publishes empty contacts, so a
/// reader can tell a quiet world from a stopped one.
#[test]
fn an_empty_world_publishes_empty_contacts() {
    let mut host = World::new();
    let world = spawn_physics_world_with::<Threaded>(&mut host, PhysicsConfig::default())
        .expect("the world thread starts");
    let reader = world
        .commands()
        .clone()
        .watch(|world: &World| {
            let mut physics_contacts_rows_2 = world.root_scope().query::<&PhysicsContacts>();

            let contacts = physics_contacts_rows_2.iter().next()?;
            Some((contacts.started.len(), contacts.stopped.len()))
        })
        .expect("the physics world accepts a subscription");

    let mut installed_reader = None;
    assert!(
        wait_for(|| match reader.poll() {
            Some(installed) => {
                installed_reader = Some(installed.expect("the subscription outlet is reachable"));
                true
            }
            None => false,
        }),
        "the physics world never installed the subscription"
    );
    let reader = installed_reader.expect("the world installed the subscription");
    assert!(
        wait_for(|| reader.latest().is_some()),
        "the empty world never published a snapshot"
    );
    assert_eq!(*reader.latest().expect("the snapshot exists"), (0, 0));
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
    )
    .expect("the world thread starts");
    // Sent before any snapshot has been published: the setup phase must
    // already have created the world, so this answers without waiting.
    let probed = Snapshot::new();
    let reader = probed.reader();
    let publish = probed.clone();
    world
        .commands()
        .send(Box::new(move |world: &mut World| {
            publish.publish(
                world
                    .root_scope()
                    .query::<&PhysicsWorld>()
                    .iter()
                    .next()
                    .expect("the setup creates the world before any job")
                    .gravity,
            );
        }))
        .expect("the physics world accepts jobs");

    assert!(
        wait_for(|| reader.latest().is_some()),
        "the gravity job never published"
    );
    assert_eq!(reader.latest().map(|gravity| *gravity), Some(gravity));
}
