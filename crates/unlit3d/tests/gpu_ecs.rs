//! End-to-end GPU tests for `unlit3d` — the ECS-based rendering API.
//!
//! Each test builds a world, spawns a renderer + geometry, renders one or more
//! frames offscreen, and inspects the returned pixels.  Snapshot tests compare
//! against stored WebP references with the SSIMULACRA2 perceptual metric.

mod common;

use common::*;
use unlit3d::prelude::*;

// ---------------------------------------------------------------------------
// Pixel-level assertion tests
// ---------------------------------------------------------------------------

/// A single cube via the ECS API covers a meaningful part of the frame.
#[test]
fn ecs_cube_covers_the_frame() {
    let ctx = Ctx::headless();
    let (renderer, mut world) = test_world(&ctx);

    // Spawn the renderer as a resource entity.
    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;

    // Upload a cube mesh through the renderer.
    let mesh = world
        .with_mut::<Renderer, _>(*renderer, allocate_cube_mesh)
        .unwrap();

    // Dedicated camera entity, then the renderable entity.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        mesh,
        BoundingSphere {
            center: glam::Vec3::ZERO,
            radius: 1.8,
        },
    ));

    // Render offscreen.
    let (target, target_view) = offscreen_target(&ctx.device, "test::ecs_cube");
    let _ = world.with_mut::<Renderer, _>(*renderer, |r| {
        r.render(&world, Some(&target_view));
    });

    // Read back and check.
    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };

    let covered = count_pixels_off_background(&frame, CLEAR, 12);
    let total = (WIDTH * HEIGHT) as usize;
    assert!(
        covered > total / 8,
        "the cube should cover a meaningful area, got {covered}/{total} pixels"
    );
}

/// Two cubes at different z-positions, each carrying `Transform` and
/// `InstanceData` (the latter overrides the model matrix).
/// The nearer (green) cube must win the depth test at centre.
#[test]
fn ecs_depth_ordering_hides_the_far_instance() {
    let ctx = Ctx::headless();
    let (renderer, mut world) = test_world(&ctx);

    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;

    // Dedicated camera entity.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));

    let mesh_far = world
        .with_mut::<Renderer, _>(*renderer, allocate_cube_mesh)
        .unwrap();
    let mesh_near = world
        .with_mut::<Renderer, _>(*renderer, allocate_cube_mesh)
        .unwrap();

    // Far cube, red — Transform with InstanceData color override.
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, -0.6),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        InstanceData {
            matrix: glam::Affine3A::from_scale_rotation_translation(
                glam::Vec3::splat(0.7),
                glam::Quat::from_rotation_y(0.6),
                glam::Vec3::new(0.0, 0.2, -0.6),
            ),
            base_color: glam::Vec4::new(1.0, 0.0, 0.0, 1.0),
        },
        mesh_far,
        BoundingSphere {
            center: glam::Vec3::ZERO,
            radius: 1.8,
        },
    ));

    // Near cube, green.
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.6),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        InstanceData {
            matrix: glam::Affine3A::from_scale_rotation_translation(
                glam::Vec3::splat(0.7),
                glam::Quat::from_rotation_y(0.6),
                glam::Vec3::new(0.0, 0.2, 0.6),
            ),
            base_color: glam::Vec4::new(0.0, 1.0, 0.0, 1.0),
        },
        mesh_near,
        BoundingSphere {
            center: glam::Vec3::ZERO,
            radius: 1.8,
        },
    ));

    let (target, target_view) = offscreen_target(&ctx.device, "test::depth");
    let _ = world.with_mut::<Renderer, _>(*renderer, |r| {
        r.render(&world, Some(&target_view));
    });

    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };

    let centre = frame.pixel_u8(WIDTH / 2, HEIGHT / 2);
    assert!(
        centre[1] > centre[0],
        "the near green cube should win the depth test, got {centre:?}"
    );
}

// ---------------------------------------------------------------------------
// Snapshot tests
// ---------------------------------------------------------------------------

/// The ECS path produces a correct frame matching the stored snapshot.
#[test]
fn ecs_unlit_cube_matches_snapshot() {
    let ctx = Ctx::headless();
    let (renderer, mut world) = test_world(&ctx);

    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;

    let mesh = world
        .with_mut::<Renderer, _>(*renderer, allocate_cube_mesh)
        .unwrap();

    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));

    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        mesh,
        BoundingSphere {
            center: glam::Vec3::ZERO,
            radius: 1.8,
        },
    ));

    let (target, target_view) = offscreen_target(&ctx.device, "test::snapshot");
    let _ = world.with_mut::<Renderer, _>(*renderer, |r| {
        r.render(&world, Some(&target_view));
    });

    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };

    assert_image_snapshot("ecs_unlit_cube.webp", &frame, frame.width, frame.height);
}
