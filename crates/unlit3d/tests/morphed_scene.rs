//! GPU coverage for position morph targets: the validation of a morphing
//! mesh's targets and weight binding, and a weight entity shared by several
//! meshes.
//!
//! The multi-frame snapshot coverage that used to live here now runs in
//! `unlit3d_examples` (the `ecs_morphed` scene), whose headless path verifies
//! the same frames against the stored snapshots.

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// The per-vertex displacement of each target: the first tapers the cube's top
/// to a point, the second widens its base.
///
/// Both are functions of the vertex's own position, so the two targets produce
/// visibly different shapes and a wrong target stride reads the wrong one.
fn morph_targets(positions: &[[f32; 3]]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let taper = positions
        .iter()
        .map(|position| {
            let height = (position[1] + 1.0) * 0.5;
            [
                -position[0] * height * 0.7,
                0.0,
                -position[2] * height * 0.7,
            ]
        })
        .collect();
    let widen = positions
        .iter()
        .map(|position| {
            let height = (1.0 - position[1]) * 0.5;
            [position[0] * height * 0.6, 0.0, position[2] * height * 0.6]
        })
        .collect();
    (taper, widen)
}

/// The camera the remaining tests share.
fn camera() -> Camera {
    camera_looking_at(
        glam::Vec3::new(1.0, 1.0, 2.8),
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// A morph variant with no targets is rejected rather than drawn undeformed.
#[test]
#[should_panic(expected = "a variant that reads morph positions needs the mesh's morph targets")]
fn morphing_without_targets_panics() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, false, true));
    gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &[]);
}

/// A mesh whose weights do not match its target count is rejected rather than
/// read past the end of the packed pose.
#[test]
#[should_panic(expected = "a mesh's morph weights must hold one weight per morph target")]
fn mismatched_morph_weight_count_panics() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, false, true));
    let (positions, _uvs, _colors, _indices) = cube();
    let (taper, widen) = morph_targets(&positions);
    let targets = [
        UnlitMorphTarget { positions: &taper },
        UnlitMorphTarget { positions: &widen },
    ];
    // Two targets, one weight: the shader would loop past the end of the pose.
    let mesh = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &targets);
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
#[test]
#[should_panic(expected = "a mesh with morph targets needs a `MorphBinding`")]
fn morphing_without_a_weight_binding_panics() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, false, true));
    let (positions, _uvs, _colors, _indices) = cube();
    let (taper, widen) = morph_targets(&positions);
    let targets = [
        UnlitMorphTarget { positions: &taper },
        UnlitMorphTarget { positions: &widen },
    ];
    let mesh = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &targets);

    world.spawn((camera(),));
    world.spawn((mesh, UnlitPipeline::new(key)));
    gpu.bind_offscreen_target(&world, "test::ecs_morphed_unbound");
    gpu.render(&world);
}

/// Several meshes can share one weight entity, and moving it moves all of them.
#[test]
fn meshes_can_share_one_morph_weights() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, false, true));
    let (positions, _uvs, _colors, _indices) = cube();
    let (taper, widen) = morph_targets(&positions);
    let targets = [
        UnlitMorphTarget { positions: &taper },
        UnlitMorphTarget { positions: &widen },
    ];
    let first = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &targets);
    let second = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &targets);

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
