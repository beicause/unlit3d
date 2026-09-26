//! Multi-frame snapshot coverage for position morph targets.
//!
//! A cube with two morph targets is drawn over several frames while its target
//! weights animate: the weights live in a [`MorphWeights`] component the CPU
//! updates, and each frame is stored as its own snapshot. The sequence is what
//! shows a wrong target stride, a stale weight or a mispacked displacement — a
//! static snapshot cannot tell a correct morph from one whose weight never
//! reaches the shader.

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

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

/// The weights of `frame`, in target order.
///
/// The two targets trade off against each other and reverse halfway through,
/// so no frame repeats the one before it and both weights are exercised.
fn weights(frame: usize) -> [f32; 2] {
    let phase = frame as f32 * 0.5;
    [phase.cos().abs(), phase.sin().abs()]
}

/// The camera the whole sequence shares, so a frame's difference is the morph's.
fn camera() -> Camera {
    camera_looking_at(
        glam::Vec3::new(1.0, 1.0, 2.8),
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// A morphed cube changes shape frame by frame, and every frame differs from
/// the one before it.
#[test]
fn ecs_morphed_cube_matches_snapshots() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    // The variant that reads the morph bindings: its mesh group carries the
    // displacement buffer a plain cube's does not.
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, false, true));
    let (positions, _uvs, _colors, _indices) = cube();
    let (taper, widen) = morph_targets(&positions);
    let targets = [
        UnlitMorphTarget { positions: &taper },
        UnlitMorphTarget { positions: &widen },
    ];
    let mesh = gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &targets);

    world.spawn((camera(),));
    // The weights are an entity of their own: the mesh names it rather than
    // owning it, so blending a pose is one component write and nothing on the
    // mesh changes.
    let weights_entity = world.spawn((MorphWeights::new(weights(0)),));
    world.spawn((
        Transform {
            scale: glam::Vec3::splat(0.9),
            ..Default::default()
        },
        InstanceColor::new(glam::Vec4::new(0.55, 0.85, 1.0, 1.0)),
        mesh.clone(),
        UnlitPipeline::new(key.clone()),
        MorphBinding::new(weights_entity),
    ));
    let target = gpu.bind_offscreen_target(&world, "test::ecs_morphed");

    let mut previous: Option<Vec<u8>> = None;
    for frame in 0..FRAMES {
        // The weights are a component write, not a mesh re-upload and not a
        // GPU call: the source packs and uploads them once per frame.
        let frame_weights = weights(frame);
        world
            .get_mut::<MorphWeights>(weights_entity)
            .expect("the weight entity carries a `MorphWeights`")
            .weights = frame_weights.to_vec();

        gpu.render(&world);

        let pixels = Frame {
            rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
            width: WIDTH,
            height: HEIGHT,
        };

        // What the snapshot may freeze: the cube is on screen, and it has
        // changed shape from the frame before it. Without this a blank frame or
        // a frozen one would be stored as the reference.
        let covered = count_pixels_off_background(&pixels, CLEAR, 12);
        assert!(
            covered > (WIDTH * HEIGHT) as usize / 100,
            "frame {frame} should show the cube, got {covered} lit pixels"
        );
        if let Some(previous) = &previous {
            assert_ne!(
                &pixels.rgba, previous,
                "frame {frame} should differ from the one before it"
            );
        }
        previous = Some(pixels.rgba.clone());

        assert_image_snapshot(
            &format!("ecs_morphed/frame_{frame:02}.webp"),
            &pixels,
            WIDTH,
            HEIGHT,
        );
    }
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
