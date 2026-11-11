use super::super::shared::SceneAssets;
use super::*;
use glam::Quat;

fn ball(instance: Option<RemoteEntity>, body: Option<RigidBodyHandle>, shown: bool) -> Ball {
    Ball {
        kind: 0,
        color: 0,
        instance,
        body,
        phase: if shown {
            BallPhase::Shown
        } else {
            BallPhase::Waiting
        },
        transform: Transform::default(),
    }
}

/// Only a shown ball with a body the snapshot names is posed: a ball still
/// waiting for a half, or one the physics world has not reported yet, keeps
/// the transform the show job gave it.
#[test]
fn only_shown_balls_are_posed() {
    let mut world = World::new();
    let handle = RigidBodyHandle::from_raw_parts(7, 0);
    let waiting_body = RigidBodyHandle::from_raw_parts(8, 0);
    let instance = world.remote(Entity::from_raw(10, 0));
    {
        let mut scope = world.root_scope_mut();
        scope.spawn(bundle![ball(Some(instance), Some(handle), true)]);
    }
    let waiting = world.remote(Entity::from_raw(11, 0));
    let without_body = world.remote(Entity::from_raw(12, 0));
    {
        let mut scope = world.root_scope_mut();
        scope.spawn(bundle![ball(Some(waiting), Some(waiting_body), false)]);
        scope.spawn(bundle![ball(Some(without_body), None, true)]);
    }

    let mut snapshot = GamePhysics::default();
    snapshot
        .transforms
        .insert(handle, (Vec3::new(1.0, 2.0, 3.0), Quat::IDENTITY));
    snapshot
        .transforms
        .insert(waiting_body, (Vec3::ZERO, Quat::IDENTITY));

    let mut poses = Vec::new();
    live_poses(&world.root_scope(), &snapshot, &mut poses);

    assert_eq!(poses.len(), 1, "only a shown ball with a body is posed");
    assert_eq!(poses[0].1.translation, Vec3::new(1.0, 2.0, 3.0));
}

/// A pose names the ball's render instance: the job that carries it runs in
/// the render world, where a ball entity from this world means something
/// else entirely.
#[test]
fn a_pose_names_the_instance_not_the_ball() {
    let mut world = World::new();
    let handle = RigidBodyHandle::from_raw_parts(7, 0);
    let instance = world.remote(Entity::from_raw(10, 0));
    let ball = {
        let mut scope = world.root_scope_mut();
        scope.spawn(bundle![ball(Some(instance), Some(handle), true)])
    };
    assert_ne!(
        world.remote(ball),
        instance,
        "the fixture must not let them coincide"
    );

    let mut snapshot = GamePhysics::default();
    snapshot
        .transforms
        .insert(handle, (Vec3::new(1.0, 2.0, 3.0), Quat::IDENTITY));

    let mut poses = Vec::new();
    live_poses(&world.root_scope(), &snapshot, &mut poses);

    assert_eq!(poses.len(), 1);
    assert_eq!(poses[0].0, instance, "the pose names the render instance");
    assert_ne!(
        poses[0].0,
        world.remote(ball),
        "the pose must not name the ball entity"
    );
}

/// A ball waits while a half is missing, is shown once both are in, and is
/// reclaimed when a request it is waiting on was given up on.
#[test]
fn a_ball_waits_shows_and_is_reclaimed() {
    let world = World::new();
    let entity = Entity::from_raw(1, 0);
    let instance = world.remote(Entity::from_raw(10, 0));
    let body = RigidBodyHandle::from_raw_parts(7, 0);

    let missing_instance = ball(None, Some(body), false);
    assert_eq!(
        waiting(&missing_instance, entity, &[], false),
        Waiting::Wait
    );
    assert_eq!(
        waiting(&missing_instance, entity, &[entity], false),
        Waiting::Reclaim,
        "an instance that was given up on takes the ball with it"
    );

    let ready = ball(Some(instance), Some(body), false);
    assert_eq!(waiting(&ready, entity, &[], false), Waiting::Show(instance));
    assert_eq!(waiting(&ready, entity, &[], true), Waiting::Reclaim);

    let requested = Ball {
        phase: BallPhase::Requested,
        ..ball(Some(instance), Some(body), false)
    };
    assert_eq!(waiting(&requested, entity, &[], false), Waiting::Wait);
    assert_eq!(
        waiting(&requested, entity, &[entity], false),
        Waiting::Reclaim,
        "a show nobody answered must not keep the ball alive forever"
    );
}

/// A shown ball owes no request, so only the world it wears an instance in
/// ending takes it away: a wait measured against the tick the ball was
/// dropped on would reclaim it a fixed time after it landed.
#[test]
fn a_shown_ball_is_only_taken_by_its_world_ending() {
    let world = World::new();
    let entity = Entity::from_raw(1, 0);
    let instance = world.remote(Entity::from_raw(10, 0));
    let body = RigidBodyHandle::from_raw_parts(7, 0);
    let shown = ball(Some(instance), Some(body), true);
    assert_eq!(waiting(&shown, entity, &[], false), Waiting::Wait);
    assert_eq!(
        waiting(&shown, entity, &[], true),
        Waiting::Reclaim,
        "a world that ended takes the instance the ball wears with it"
    );
}

/// A scene whose queues a test runs itself to answer the requests on them,
/// and fills again to keep whatever a tick sends away in the lists, where
/// the test can count it.
fn stalled_scene() -> (
    Scene,
    async_channel::Receiver<Job>,
    async_channel::Receiver<Job>,
) {
    let (render_tx, render_rx) = async_channel::bounded(1);
    let (physics_tx, physics_rx) = async_channel::bounded(1);
    let (audio_tx, _audio_rx) = async_channel::bounded(1);
    let scene = Scene {
        render: JobSender::new(render_tx),
        physics: JobSender::new(physics_tx),
        audio: JobSender::new(audio_tx),
        poses: Snapshot::new().reader(),
        render_state: Snapshot::new().reader(),
        physics_state: Snapshot::new().reader(),
        audio_state: Snapshot::new().reader(),
    };
    (scene, render_rx, physics_rx)
}

/// Run every job waiting on a queue, so the answer it publishes is there
/// for the next tick to fold. The world those jobs are for does not exist
/// in a test, so each runs against an empty world of its own.
fn answer(jobs: &async_channel::Receiver<Job>) {
    let mut other = World::new();
    while let Ok(job) = jobs.try_recv() {
        job(&mut other);
    }
}

/// Fill every queue, so a send a tick makes on the way out reports `Full`
/// and the cleanup stays in the lists, where a test can count it.
fn stall(scene: &Scene) {
    for queue in [&scene.render, &scene.physics, &scene.audio] {
        let _ = queue.send(Box::new(|_: &mut World| {}));
    }
}

/// A scene state whose assets are loaded, so a tick does not turn back at
/// the door: no render world builds them in a test.
fn ready_scene_state(world: &mut World, scene: &Scene, jobs: &async_channel::Receiver<Job>) {
    let entity = world
        .root_scope_mut()
        .spawn(bundle![SceneState::new(scene)]);
    // The asset build has no render world to run in, so the job it sent is
    // dropped rather than run: the assets the tick needs are put in by
    // hand.
    while jobs.try_recv().is_ok() {}
    let Ok(mut state) = world.root_scope().query_one::<&mut SceneState>(entity) else {
        panic!("the state was just spawned")
    };
    let state = state.get();
    state.ready = true;
    state.loaded = Some(SceneAssets {
        meshes: vec![world.remote(Entity::from_raw(1, 0))],
        materials: vec![world.remote(Entity::from_raw(2, 0)); BALL_COLORS.len()],
    });
}

/// The answers land before the clear, so a clear takes a ball away only
/// once: the halves it just received are sent away exactly once, not once
/// by the reclaim and once again by the clear.
#[test]
fn a_clear_takes_the_halves_a_fold_just_landed() {
    let mut world = World::new();
    let (scene, render, physics) = stalled_scene();
    ready_scene_state(&mut world, &scene, &render);
    let instance = world.remote(Entity::from_raw(10, 0));
    let body = RigidBodyHandle::from_raw_parts(7, 0);
    let entity = world
        .root_scope_mut()
        .spawn(bundle![ball(None, None, false)]);
    let mut balls = Balls::new(&scene);
    // Both halves are asked of the worlds the tick is handed and answered
    // by running what they were sent, so the fold finds them in flight
    // rather than published by hand.
    balls
        .instances
        .request(&scene.render, entity, move |_: &mut World| instance)
        .expect("the queue was drained");
    balls
        .bodies
        .request(&scene.physics, entity, move |_: &mut World| body)
        .expect("the queue was drained");
    answer(&render);
    answer(&physics);
    stall(&scene);
    balls.clear = true;

    BallDrops::tick(&mut balls, &world, &scene, FRAME_SECONDS);
    let _ = world.apply();

    assert_eq!(
        world.root_scope().query::<&Ball>().iter().count(),
        0,
        "the clear takes the ball the answers landed on"
    );
    assert_eq!(
        balls.despawns,
        vec![body],
        "the body the fold landed is sent away once"
    );
    assert_eq!(
        balls.orphans,
        vec![instance],
        "and so is the instance it was shown on"
    );
}

/// An answer whose ball is already gone is cleaned up: an instance with
/// nobody to be shown on is despawned rather than left in the scene.
#[test]
fn an_answer_without_its_ball_is_cleaned_up() {
    let mut world = World::new();
    let (scene, render, physics) = stalled_scene();
    ready_scene_state(&mut world, &scene, &render);
    let instance = world.remote(Entity::from_raw(10, 0));
    let body = RigidBodyHandle::from_raw_parts(7, 0);
    let gone = world
        .root_scope_mut()
        .spawn(bundle![ball(None, None, false)]);
    world
        .root_scope_mut()
        .despawn(gone)
        .expect("the ball the answers are about is gone");
    let _ = world.apply();

    let mut balls = Balls::new(&scene);
    balls
        .instances
        .request(&scene.render, gone, move |_: &mut World| instance)
        .expect("the queue was drained");
    balls
        .bodies
        .request(&scene.physics, gone, move |_: &mut World| body)
        .expect("the queue was drained");
    answer(&render);
    answer(&physics);
    stall(&scene);
    BallDrops::tick(&mut balls, &world, &scene, FRAME_SECONDS);

    assert_eq!(
        balls.orphans,
        vec![instance],
        "an instance whose ball is gone is despawned"
    );
    assert_eq!(
        balls.despawns,
        vec![body],
        "and so is the body that answered for it"
    );
}
