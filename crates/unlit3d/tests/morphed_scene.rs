//! Multi-frame snapshot coverage for position morph targets.
//!
//! A cube with two morph targets is drawn over several frames while its target
//! weights animate: the weights are rewritten every frame through the source,
//! and each frame is stored as its own snapshot. The sequence is what shows a
//! wrong target stride, a stale weight or a mispacked displacement — a static
//! snapshot cannot tell a correct morph from one whose weight never reaches the
//! shader.

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
    // displacement and weight buffers a plain cube's does not.
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, false, true));
    let (positions, _uvs, _colors, _indices) = cube();
    let (taper, widen) = morph_targets(&positions);
    let initial = weights(0);
    let targets = [
        UnlitMorphTarget {
            positions: &taper,
            weight: initial[0],
        },
        UnlitMorphTarget {
            positions: &widen,
            weight: initial[1],
        },
    ];
    let mesh = gpu.allocate_deformed_cube_mesh(&world, &key, None, &targets);

    world.spawn((camera(),));
    world.spawn((
        Transform {
            scale: glam::Vec3::splat(0.9),
            ..Default::default()
        },
        InstanceColor::new(glam::Vec4::new(0.55, 0.85, 1.0, 1.0)),
        mesh.clone(),
        UnlitPipeline::new(key.clone()),
    ));
    let target = gpu.bind_offscreen_target(&world, "test::ecs_morphed");

    let mut previous: Option<Vec<u8>> = None;
    for frame in 0..FRAMES {
        // The weights are a per-frame buffer write, not a mesh re-upload: the
        // mesh's bind group is the same one the first frame drew with.
        gpu.with_mesh_source(&world, |source, world| {
            source.update_morph_weights(world, &mesh, &weights(frame));
        });

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
    gpu.allocate_deformed_cube_mesh(&world, &key, None, &[]);
}
