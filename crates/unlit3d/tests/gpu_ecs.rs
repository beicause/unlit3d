//! End-to-end GPU tests for `unlit3d` — the ECS-based rendering API.
//!
//! Each test builds a world, spawns a renderer + geometry, renders one or more
//! frames offscreen, and inspects the returned pixels.  Snapshot tests compare
//! against stored WebP references with the SSIMULACRA2 perceptual metric.

pub mod common;

use common::*;
use unlit3d::prelude::*;

// ---------------------------------------------------------------------------
// Pixel-level assertion tests
// ---------------------------------------------------------------------------

/// A single cube via the ECS API covers a meaningful part of the frame.
#[test]
fn ecs_cube_covers_the_frame() {
    let ctx = Ctx::headless();
    let (renderer, mut world, key) = test_world(&ctx);

    // Spawn the renderer as a resource entity.
    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;

    // Upload a cube mesh through the renderer.
    let mesh = world
        .with_mut::<Renderer, _>(*renderer, |r| allocate_cube_mesh(r, &key))
        .unwrap();

    // Dedicated camera entity, then the renderable entity. The mesh carries
    // its own AABB now, so no bounding component is needed.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        mesh,
        UnlitPipeline::new(key),
    ));

    // Render offscreen.
    let target = world
        .with_mut::<Renderer, _>(*renderer, |r| {
            let target = bind_offscreen_target(r, "test::ecs_cube");
            r.update_metadata_buffer();
            r.render(&world);
            target
        })
        .expect("renderer is a resource entity");

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

/// Two cubes at different z-positions, each with its own Transform and an
/// InstanceColor tint.
/// The nearer (green) cube must win the depth test at centre.
#[test]
fn ecs_depth_ordering_hides_the_far_instance() {
    let ctx = Ctx::headless();
    let (renderer, mut world, key) = test_world(&ctx);

    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;

    // Dedicated camera entity.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));

    let mesh_far = world
        .with_mut::<Renderer, _>(*renderer, |r| allocate_cube_mesh(r, &key))
        .unwrap();
    let mesh_near = world
        .with_mut::<Renderer, _>(*renderer, |r| allocate_cube_mesh(r, &key))
        .unwrap();

    // Far cube, red.
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, -0.6),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        InstanceColor::new(glam::Vec4::new(1.0, 0.0, 0.0, 1.0)),
        mesh_far,
        UnlitPipeline::new(key.clone()),
    ));

    // Near cube, green.
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.6),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        InstanceColor::new(glam::Vec4::new(0.0, 1.0, 0.0, 1.0)),
        mesh_near,
        UnlitPipeline::new(key.clone()),
    ));

    let target = world
        .with_mut::<Renderer, _>(*renderer, |r| {
            let target = bind_offscreen_target(r, "test::depth");
            r.update_metadata_buffer();
            r.render(&world);
            target
        })
        .expect("renderer is a resource entity");

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

/// Removing a mesh frees everything the mesh own, including the per-mesh
/// uniform that only feeds its bind group.
///
/// That uniform is a graph *orphan*: the mesh's buffers were built into the
/// bind group, not out of it, so the removal walk from the buffers reaches the
/// group but never the uniform. `remove_mesh` collects it through the graph's
/// cleanup, leaving the graph exactly as it was before the mesh existed.
#[test]
fn removing_a_mesh_leaves_no_resource_behind() {
    let ctx = Ctx::headless();
    let (mut renderer, key) = create_renderer(&ctx);

    // A first mesh warms up any lazily created global state, so the second
    // mesh's resources are the only difference measured below.
    let baseline_mesh = allocate_cube_mesh(&mut renderer, &key);
    let baseline = renderer.graph.len();

    let mesh = allocate_cube_mesh(&mut renderer, &key);
    assert!(
        renderer.graph.len() > baseline,
        "allocating a mesh should add resources"
    );

    renderer.remove_mesh(mesh);

    assert_eq!(
        renderer.graph.len(),
        baseline,
        "remove_mesh should free the mesh's buffers, its bind group and the \
         orphaned mesh-info uniform"
    );
    // The warmed-up mesh and the renderer's own resources are untouched.
    assert!(
        renderer
            .graph
            .get_buffer(baseline_mesh.vertex_buffers[0].1)
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Pooled-buffer lifecycle tests
// ---------------------------------------------------------------------------

/// A mesh drawn into the element range another mesh vacated renders with the
/// data written for it, not with whatever was there before.
///
/// A pool hands the range a removed mesh freed to the next one, whose data is
/// then written over the old at that range's offset. Getting an offset wrong
/// reads the previous mesh's vertices and indices — silently, since every
/// offset involved is still valid. Rendering the replacement and checking the
/// pixels it covers is what catches it.
#[test]
fn a_mesh_reusing_a_freed_range_draws_its_own_geometry() {
    let ctx = Ctx::headless();
    let (renderer, mut world, key) = test_world(&ctx);

    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));

    // A cube fills a roughly square block of the frame, and the replacement
    // draws at the same spot through the ranges it vacated.
    //
    // A placeholder mesh allocated before the cube stays off-screen for the
    // whole test and holds the pools' lowest element indices, so the cube and
    // the replacement draw from ranges that do not start at zero. Its vertices
    // are offset, so a draw that read the placeholder's range would paint a
    // different shape at the probes below rather than the same one twice.
    world
        .with_mut::<Renderer, _>(*renderer, |r| {
            allocate_offset_cube_mesh(r, &key, glam::Vec3::splat(10.0))
        })
        .unwrap();
    let cube = world
        .with_mut::<Renderer, _>(*renderer, |r| allocate_cube_mesh(r, &key))
        .unwrap();
    let cube_entity = world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        cube,
        UnlitPipeline::new(key.clone()),
    ));

    let target = world
        .with_mut::<Renderer, _>(*renderer, |r| {
            let target = bind_offscreen_target(r, "test::reused_range::before");
            r.update_metadata_buffer();
            r.render(&world);
            target
        })
        .expect("renderer is a resource entity");
    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };
    let covered_before = count_pixels_off_background(&frame, CLEAR, 12);
    assert!(covered_before > 0, "the cube should be visible");

    // Retire the cube, then hand its pools' ranges to a replacement mesh.
    let cube = world
        .get::<GpuMesh>(cube_entity)
        .expect("the cube carries a mesh")
        .clone();
    assert!(world.despawn(cube_entity));
    world
        .with_mut::<Renderer, _>(*renderer, move |r| r.remove_mesh(cube))
        .expect("renderer is a resource entity");

    // The replacement takes the ranges the cube vacated — one allocation per
    // pool — and draws at the spot the cube drew at, tinted pure red through
    // its per-instance colour so the pixel the cube used to own can only come
    // from the replacement's own data.
    let replacement = world
        .with_mut::<Renderer, _>(*renderer, |r| allocate_cube_mesh(r, &key))
        .unwrap();
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        InstanceColor::new(glam::Vec4::new(1.0, 0.0, 0.0, 1.0)),
        replacement,
        UnlitPipeline::new(key),
    ));

    let target = world
        .with_mut::<Renderer, _>(*renderer, |r| {
            let target = bind_offscreen_target(r, "test::reused_range::after");
            r.update_metadata_buffer();
            r.render(&world);
            target
        })
        .expect("renderer is a resource entity");
    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };

    // The centre pixel is the replacement's red, and the count of red pixels
    // is the replacement's silhouette. Both come only from the vertices and
    // indices written into the range the cube vacated: a draw whose base
    // vertex or first index pointed anywhere else would read the placeholder's
    // vertices — offset out of shape — and paint a different silhouette.
    let centre = frame.pixel_u8(WIDTH / 2, HEIGHT / 2);
    assert!(
        centre[0] > 200 && centre[1] < 60 && centre[2] < 60,
        "the replacement should draw pure red at the centre, got {centre:?}"
    );
    let mut red = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let [r, g, b, _] = frame.pixel_u8(x, y);
            if r > 200 && g < 60 && b < 60 {
                red += 1;
            }
        }
    }
    assert!(
        red > 1000,
        "the replacement's silhouette should cover a meaningful area, got {red}"
    );
}

/// Meshes allocated across many frames keep drawing their own geometry after
/// the pools have grown to fit the later ones.
///
/// A pool grows by creating a new buffer, copying the old into it and
/// re-pointing the graph node the meshes name. Each step is a way to lose a
/// mesh's data: a copy that misses the tail of the old buffer drops the last
/// meshes' vertices, and a node left behind leaves every draw reading a
/// replaced buffer. Rendering a frame per mesh while meshes accumulate, and
/// checking the first mesh each time, covers both.
#[test]
fn meshes_allocated_across_frames_survive_pool_growth() {
    let ctx = Ctx::headless();
    let (renderer, mut world, key) = test_world(&ctx);

    let mut renderer_entity = world.spawn((unlit_ecs::Resource, renderer));
    let renderer = &mut renderer_entity;
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));

    // The first mesh draws at the centre of the frame and never moves, so the
    // pixel there is its own as long as its data survives every grow.
    let first = world
        .with_mut::<Renderer, _>(*renderer, |r| allocate_cube_mesh(r, &key))
        .unwrap();
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        first,
        UnlitPipeline::new(key.clone()),
    ));

    // A grid cube apiece: the first is small enough for the pools as created,
    // and every later one is large enough to force a grow.
    let grids: [GpuMesh; 3] = std::array::from_fn(|_| {
        world
            .with_mut::<Renderer, _>(*renderer, |r| allocate_grid_cube_mesh(r, &key, 4))
            .unwrap()
    });
    for (frame_index, mesh) in grids.into_iter().enumerate() {
        world.spawn((
            Transform {
                translation: glam::Vec3::new(-3.0 + 2.4 * frame_index as f32, -1.4, -2.0),
                rotation: glam::Quat::from_rotation_y(0.3),
                scale: glam::Vec3::splat(0.05),
            },
            mesh,
            UnlitPipeline::new(key.clone()),
        ));

        let target = world
            .with_mut::<Renderer, _>(*renderer, |r| {
                let target = bind_offscreen_target(r, "test::pool_growth");
                r.update_metadata_buffer();
                r.render(&world);
                target
            })
            .expect("renderer is a resource entity");
        let frame = Frame {
            rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
            width: WIDTH,
            height: HEIGHT,
        };

        // The centre pixel is the first mesh's cube. A grow that lost the
        // first mesh's vertices, or a stream node left pointing at the buffer
        // the grow replaced, draws background there instead.
        let centre = frame.pixel_u8(WIDTH / 2, HEIGHT / 2);
        assert!(
            centre[2] > 100,
            "the first mesh should still draw on its \
             {frame_index}-th added mesh, got {centre:?}"
        );
    }
}
