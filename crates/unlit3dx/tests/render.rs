//! Integration tests for the render bridge: the jobs that build assets and
//! instances inside the render world, and the way a render job that cannot
//! run fails its request without taking the host down.

use unlit_ecs::{Entity, World};
use unlit3d::components::Transform;
use unlit3dx::prelude::*;

/// A host world with one sub-world in it, plus the sub-world's handle.
fn sub_world() -> (World, WorldHandle) {
    let mut host = World::new();
    let world = spawn_sub_world(&mut host, "render", 16);
    (host, world)
}

/// The nested world behind a handle, ready to drain.
fn nested<'a>(host: &'a mut World, world: &WorldHandle) -> unlit_ecs::CellRefMut<'a, World> {
    host.get_mut::<World>(world.entity())
        .expect("the sub-world is a child of the host")
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

    let missing = Entity::PLACEHOLDER;
    let doomed = world.request(spawn_instance(missing, missing, Transform::default()));
    {
        let mut nested = nested(&mut host, &world);
        drain_sub_world(&mut nested);
    }

    assert_eq!(doomed.recv(), Err(RequestError::Dropped));
}
