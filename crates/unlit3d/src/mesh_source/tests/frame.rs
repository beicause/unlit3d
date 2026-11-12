use super::*;

#[test]
fn a_frame_uses_the_first_active_camera() {
    let mut world = World::new();
    // Spawned first, so a selection that ignored `active` would pick this
    // one instead of the active camera below.
    let inactive = world.root_scope_mut().spawn(bundle![Camera {
        view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-1.0, 0.0, 0.0)),
        clip_from_view: glam::Mat4::IDENTITY,
        active: true,
    },]);
    world
        .root_scope()
        .query_one::<&mut Camera>(inactive)
        .map(|mut camera| {
            let camera = camera.get();
            camera.active = false
        })
        .expect("the entity carries a camera");
    let active = world.root_scope_mut().spawn(bundle![Camera {
        view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-2.0, 0.0, 0.0)),
        clip_from_view: glam::Mat4::IDENTITY,
        active: true,
    },]);

    let (_entity, camera) = frame_camera(&world.root_scope()).expect("a camera is active");
    assert_eq!(camera.position(), glam::Vec3::new(2.0, 0.0, 0.0));
    assert!(camera.active);

    // With every camera inactive, the frame has nothing to draw through.
    world
        .root_scope()
        .query_one::<&mut Camera>(active)
        .map(|mut camera| {
            let camera = camera.get();
            camera.active = false
        })
        .expect("the entity carries a camera");
    assert!(frame_camera(&world.root_scope()).is_none());
}

#[test]
fn each_scope_frames_through_its_own_camera() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Transform::default()]);
    world.scope_mut(scene).unwrap().spawn(bundle![Camera {
        view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-2.0, 0.0, 0.0)),
        clip_from_view: glam::Mat4::IDENTITY,
        active: true,
    },]);
    // A second scene, with no camera of its own.
    let sibling = world.root_scope_mut().spawn(bundle![Transform::default()]);

    let (_entity, camera) =
        frame_camera(&world.scope(scene).unwrap()).expect("the scene has a camera");
    assert_eq!(camera.position(), glam::Vec3::new(2.0, 0.0, 0.0));

    // A sibling's camera is out of reach in both directions, which is what
    // lets two sources frame different parts of one world.
    assert!(
        frame_camera(&world.scope(sibling).unwrap()).is_none(),
        "a scope does not borrow its sibling's camera"
    );
    assert!(
        frame_camera(&world.root_scope()).is_some(),
        "the root sees the scene's camera"
    );
}

#[test]
fn a_frame_without_a_camera_only_clears() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    // A drawable entity, but nothing the source can view it from.
    h.spawn_mesh(mesh);

    // The source builds nothing, so the driver's pass draws nothing while
    // still applying the frame's clears.
    let mut encoder = h.encoder();
    let ctx = h.source.context();
    let mut scene = Scene::new();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );
    assert!(
        h.source.caches.visible_cache.is_empty(),
        "nothing was drawn"
    );
    assert!(scene.draws.is_empty(), "the scene is empty");
}

#[test]
fn grown_capacity_never_shrinks_and_covers_the_need() {
    // Beyond the 1.5x growth, the need itself wins.
    assert_eq!(grown_capacity(16, 30), 30);
    assert_eq!(grown_capacity(16, 40), 40);
    // Otherwise the capacity grows by 1.5x, which is at least `needed`.
    assert_eq!(grown_capacity(16, 17), 24);
    assert_eq!(grown_capacity(16, 24), 24);
    // An empty allocator still ends up with one unit.
    assert_eq!(grown_capacity(0, 0), 1);
}

/// A camera at `eye` looking at the origin, with an orthographic
/// projection.
///
/// An orthographic projection has no perspective divide, so a depth read
/// off the clip-space w alone would order nothing.
fn test_orthographic_camera(eye: glam::Vec3) -> Camera {
    Camera {
        view_from_world: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        clip_from_view: glam::camera::rh::proj::directx::orthographic(
            -8.0, 8.0, -8.0, 8.0, 0.1, 100.0,
        ),
        active: true,
    }
}

/// An orthographic camera still sorts z-sorted entries by view-axis depth.
///
/// Under an orthographic projection the perspective divide is gone, so a
/// depth that reads the fourth row alone would report the same value for
/// every entry and leave their order to the sort's stability.
#[test]
fn orthographic_z_sorted_entries_sort_by_view_axis_depth() {
    let mut h = harness();
    let near = h.tri_mesh_at(1.5);
    let near_aabb = near.aabb;
    let far = h.tri_mesh_at(-1.5);
    let far_aabb = far.aabb;

    // Both entity origins are at the same depth; only the geometry differs.
    let translation = glam::Vec3::new(0.0, 0.0, 2.0);
    let near_entity = h.spawn_z_sorted(
        near,
        Transform {
            translation,
            ..Default::default()
        },
    );
    let far_entity = h.spawn_z_sorted(
        far,
        Transform {
            translation,
            ..Default::default()
        },
    );

    let camera = test_orthographic_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    // No perspective divide to lean on: the depths still come out along
    // the view axis, each off its own mesh's bounds centre, and the far
    // mesh is the deeper draw.
    let depth_of = |entity: Entity| {
        h.source
            .caches
            .visible_cache
            .iter()
            .find(|entry| entry.mesh.entity == entity)
            .unwrap()
            .depth
    };
    let near_centre = near_aabb.center + translation;
    let far_centre = far_aabb.center + translation;
    assert!(
        (depth_of(near_entity) - camera.view_depth(near_centre)).abs() < 1e-5,
        "the near mesh's depth is not its bounds centre: {} against {}",
        depth_of(near_entity),
        camera.view_depth(near_centre),
    );
    assert!(
        (depth_of(far_entity) - camera.view_depth(far_centre)).abs() < 1e-5,
        "the far mesh's depth is not its bounds centre: {} against {}",
        depth_of(far_entity),
        camera.view_depth(far_centre),
    );
    assert!(depth_of(far_entity) > depth_of(near_entity));
}
