//! Multi-frame snapshot coverage for vertex skinning.
//!
//! A skinned cube is drawn over several frames while its pose animates: the
//! joint matrices are rewritten every frame through the source, and each frame
//! is stored as its own snapshot. The sequence is what shows a wrong joint
//! index, a stale pose or a misaligned joint stream — a static snapshot cannot
//! tell a correct bind pose from one that never reaches the shader.

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

/// Where the camera sits, close enough that a deformation moves many pixels.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.9, 1.1, 2.6);

/// The camera the whole sequence shares, so a frame's difference is the pose's.
fn camera() -> Camera {
    camera_looking_at(
        CAMERA_EYE,
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// The angle the bending joint holds in `frame`.
///
/// Frame zero is the rest pose, which gives the sequence a reference every
/// later frame departs from; the sign flips halfway so the cube bends both
/// ways instead of swinging through one arc.
fn bend_angle(frame: usize) -> f32 {
    let half = FRAMES / 2;
    let step = if frame < half {
        frame as isize
    } else {
        half as isize - (frame - half) as isize
    };
    step as f32 * 0.42
}

/// A skinned cube bends frame by frame, and every frame differs from the one
/// before it.
#[test]
fn ecs_skinned_cube_matches_snapshots() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    // The variant that reads the joint stream: its position buffer is wider by
    // the joint pair, so a stream packed for the plain cube would misread.
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, true, false));
    let (mesh, mut skin) = gpu.allocate_bent_cube_mesh(&world, &key, bend_angle(0));

    world.spawn((camera(),));
    world.spawn((
        Transform {
            scale: glam::Vec3::splat(0.9),
            ..Default::default()
        },
        InstanceColor::new(glam::Vec4::new(1.0, 0.85, 0.6, 1.0)),
        mesh.clone(),
        UnlitPipeline::new(key.clone()),
    ));
    let target = gpu.bind_offscreen_target(&world, "test::ecs_skinned");

    let mut previous: Option<Vec<u8>> = None;
    for frame in 0..FRAMES {
        // The pose is a per-frame buffer write, not a mesh re-upload: the
        // mesh's bind group is the same one the first frame drew with.
        skin.set_angle(bend_angle(frame));
        gpu.with_mesh_source(&world, |source, world| {
            source.update_skin(world, &mesh, &skin.pose);
        });

        gpu.render(&world);

        let pixels = Frame {
            rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
            width: WIDTH,
            height: HEIGHT,
        };

        // What the snapshot may freeze: the cube is on screen, and it has
        // bent on from the frame before it. Without this a blank frame or a
        // frozen one would be stored as the reference.
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
            &format!("ecs_skinned/frame_{frame:02}.webp"),
            &pixels,
            WIDTH,
            HEIGHT,
        );
    }
}

/// A skinned mesh with no skin data is rejected rather than drawn undeformed.
#[test]
#[should_panic(expected = "a variant that reads joints needs the mesh's skin")]
fn skinning_without_skin_data_panics() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, true, false));
    gpu.allocate_deformed_cube_mesh(&world, &key, None, None);
}
