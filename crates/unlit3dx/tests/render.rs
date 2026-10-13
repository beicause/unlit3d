//! Integration tests for the render bridge: the transform mailbox that
//! carries poses into the render world, and the way a render job that
//! cannot run fails its request without taking the host down.

use unlit_ecs::World;
use unlit3d::components::Transform;
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;
use unlit3dx::prelude::*;

/// A host world with one sub-world in it, plus the sub-world's handle.
fn sub_world() -> (World, WorldHandle) {
    let mut host = World::new();
    let world = spawn_sub_world(&mut host, 16);
    (host, world)
}

/// The nested world behind a handle, ready to drain.
fn nested<'a>(host: &'a mut World, world: &WorldHandle) -> unlit_ecs::CellRefMut<'a, World> {
    host.get_mut::<World>(world.entity())
        .expect("the sub-world is a child of the host")
}

/// A frame at a fixed size, for the transform applier to run under.
fn frame() -> Frame {
    Frame {
        delta_time: 1.0 / 60.0,
        index: 0,
        size: (256, 192),
    }
}

/// A job that cannot run fails its request instead of unwinding into the
/// host: the sub-world drain catches the panic, the request reports it,
/// and the jobs queued behind it still run.
#[test]
fn a_panicking_render_job_fails_only_its_own_request() {
    let (mut host, world) = sub_world();

    // No MeshSource is ever spawned into this sub-world, so allocating a
    // mesh panics inside the job.
    let doomed = world.request(create_mesh(cuboid_mesh(glam::Vec3::splat(0.5))));
    let survivor = world.request(Box::new(|_: &mut World| 7u64));

    {
        let mut nested = nested(&mut host, &world);
        drain_sub_world(&mut nested);
    }

    assert!(doomed.dropped(), "the failed request reports the drop");
    assert_eq!(
        doomed.recv(),
        Err(RequestError::Dropped),
        "a request whose job panicked answers with the failure"
    );
    assert_eq!(survivor.poll(), Some(7), "the queue kept draining");
}

/// An instance naming an asset entity that does not exist fails its
/// request, and the failure is a value rather than a thread panic.
#[test]
fn an_instance_with_a_missing_asset_fails_its_request() {
    let (mut host, world) = sub_world();

    let doomed = world.request(spawn_instance(0, 0, Transform::default()));
    {
        let mut nested = nested(&mut host, &world);
        drain_sub_world(&mut nested);
    }

    assert_eq!(doomed.recv(), Err(RequestError::Dropped));
}

/// The transform mailbox moves a live entity and skips a stale one: an
/// instance that has not been spawned yet is not an error.
#[test]
fn transform_frames_move_live_entities_and_skip_stale_ones() {
    let mut world = World::new();
    let live = world.spawn((Transform::default(),));
    let stale = live.to_bits() + 1_000;

    let (writer, reader) = snapshot_pair::<TransformFrame>();
    let _ = writer.publish(TransformFrame {
        transforms: vec![
            (
                live.to_bits(),
                glam::Vec3::new(1.0, 2.0, 3.0),
                glam::Quat::IDENTITY,
            ),
            (stale, glam::Vec3::new(9.0, 9.0, 9.0), glam::Quat::IDENTITY),
        ],
    });

    world.spawn_frame_behaviour(apply_transform_frames(reader));
    let mut frame = frame();
    dispatch_frame(&mut world, &mut frame);

    let transform = world
        .get::<Transform>(live)
        .expect("the live entity still exists");
    assert_eq!(transform.translation, glam::Vec3::new(1.0, 2.0, 3.0));
}

/// Without a frame published yet the applier is a no-op: the render world
/// starts before gameplay has anything to say.
#[test]
fn an_empty_mailbox_leaves_transforms_alone() {
    let mut world = World::new();
    let entity = world.spawn((Transform {
        translation: glam::Vec3::new(4.0, 5.0, 6.0),
        ..Default::default()
    },));

    let (_writer, reader) = snapshot_pair::<TransformFrame>();
    world.spawn_frame_behaviour(apply_transform_frames(reader));
    let mut frame = frame();
    dispatch_frame(&mut world, &mut frame);

    let transform = world
        .get::<Transform>(entity)
        .expect("the entity still exists");
    assert_eq!(transform.translation, glam::Vec3::new(4.0, 5.0, 6.0));
}
