//! GPU coverage for position morph targets: the validation of a morphing
//! mesh's targets and weight binding, and a weight entity shared by several
//! meshes.
//!
//! The multi-frame snapshot coverage that used to live here now runs in
//! `unlit3d_examples` (the `ecs_morphed` scene), whose snapshot test verifies
//! the same frames against the stored snapshots.

pub mod common;

use common::*;
use unlit_wgpu_test_util::{gpu_test_main, gpu_tests};
use unlit3d::prelude::*;

/// The displacement of the two targets, packed the way a mesh stores them:
/// per vertex, one target after the other, three components each.
///
/// The first target tapers the cube's top to a point, the second widens its
/// base. Both are functions of the vertex's own position, so the two produce
/// visibly different shapes and a wrong target stride reads the wrong one.
fn morph_deltas(positions: &[[f32; 3]]) -> MorphDeltas {
    let mut deltas = Vec::with_capacity(positions.len() * 2 * 3);
    for position in positions {
        let height = (position[1] + 1.0) * 0.5;
        deltas.extend_from_slice(&[
            -position[0] * height * 0.7,
            0.0,
            -position[2] * height * 0.7,
        ]);
        let height = (1.0 - position[1]) * 0.5;
        deltas.extend_from_slice(&[position[0] * height * 0.6, 0.0, position[2] * height * 0.6]);
    }
    MorphDeltas {
        deltas,
        target_count: 2,
    }
}

/// The camera the remaining tests share.
fn camera() -> Camera {
    camera_looking_at(
        glam::Vec3::new(1.0, 1.0, 2.8),
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// A mesh whose weights do not match its target count is rejected rather than
/// read past the end of the packed pose.
async fn mismatched_morph_weight_count_panics() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(unlit_options(&ctx.device));
    let (positions, _uvs, _colors, _indices) = cube();
    let deltas = morph_deltas(&positions);
    // Two targets, one weight: the shader would loop past the end of the pose.
    let mesh = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, Some(deltas));
    let weights_entity = world.spawn((MorphWeights::new(vec![0.5]),));

    world.spawn((camera(),));
    world.spawn((
        mesh,
        UnlitPipeline::new(key),
        MorphBinding::new(weights_entity),
    ));
    gpu.bind_offscreen_target(&world, "test::ecs_morphed_mismatch");
    gpu.render(&world);
}

/// A morphed mesh drawn without a weight entity is rejected rather than blended
/// by whatever the frame packed for somebody else.
async fn morphing_without_a_weight_binding_panics() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(unlit_options(&ctx.device));
    let (positions, _uvs, _colors, _indices) = cube();
    let deltas = morph_deltas(&positions);
    let mesh = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, Some(deltas));

    world.spawn((camera(),));
    world.spawn((mesh, UnlitPipeline::new(key)));
    gpu.bind_offscreen_target(&world, "test::ecs_morphed_unbound");
    gpu.render(&world);
}

/// Several meshes can share one weight entity, and moving it moves all of them.
async fn meshes_can_share_one_morph_weights() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(unlit_options(&ctx.device));
    let (positions, _uvs, _colors, _indices) = cube();
    let deltas = morph_deltas(&positions);
    let first = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, Some(deltas.clone()));
    let second = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, Some(deltas));

    // One weight entity, named by both meshes: a crowd of copies of one mesh
    // shares one blended pose instead of holding a copy each.
    let weights_entity = world.spawn((MorphWeights::new(vec![0.0, 0.0]),));
    world.spawn((camera(),));
    for (mesh, x) in [(first, -0.7), (second, 0.7)] {
        world.spawn((
            Transform {
                translation: glam::Vec3::new(x, 0.0, 0.0),
                ..Default::default()
            },
            mesh,
            UnlitPipeline::new(key.clone()),
            MorphBinding::new(weights_entity),
        ));
    }
    let target = gpu.bind_offscreen_target(&world, "test::ecs_morphed_shared");

    // Zero weights and a blended pose must render differently: if the source
    // packed one pose per mesh rather than one per instance, the shared entity
    // would reach only one of them.
    let read = |gpu: &TestGpu| {
        gpu.render(&world);
        read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target))
    };
    let unweighted = read(&gpu);
    world
        .get_mut::<MorphWeights>(weights_entity)
        .expect("the weight entity carries a `MorphWeights`")
        .weights = vec![1.0, 0.0];
    let blended = read(&gpu);

    assert_ne!(
        unweighted, blended,
        "the shared weights must reach both meshes"
    );
}

// The registry both runners drive: `cargo nextest` natively, and a
// browser through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    #[should_panic(expected = "a mesh's morph weights must hold one weight per morph target")]
    mismatched_morph_weight_count_panics,
    #[should_panic(expected = "a mesh with morph targets needs a `MorphBinding`")]
    morphing_without_a_weight_binding_panics,
    meshes_can_share_one_morph_weights,
}

gpu_test_main!(all_tests());
