//! Multi-frame snapshot coverage for vertex skinning.
//!
//! A skinned cube is drawn over several frames while its pose animates: the
//! joint matrices live in a [`SkinPose`] component the CPU updates, and each
//! frame is stored as its own snapshot. The sequence is what shows a wrong
//! joint index, a stale pose or a misaligned joint stream — a static snapshot
//! cannot tell a correct bind pose from one that never reaches the shader.

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
    // The pose is an entity of its own: the mesh names it rather than owning
    // it, so animating the skeleton is one component write and nothing on the
    // mesh changes.
    let pose_entity = world.spawn((skin.pose(),));
    world.spawn((
        Transform {
            scale: glam::Vec3::splat(0.9),
            ..Default::default()
        },
        InstanceColor::new(glam::Vec4::new(1.0, 0.85, 0.6, 1.0)),
        mesh.clone(),
        UnlitPipeline::new(key.clone()),
        SkinBinding::new(pose_entity),
    ));
    let target = gpu.bind_offscreen_target(&world, "test::ecs_skinned");

    let mut previous: Option<Vec<u8>> = None;
    for frame in 0..FRAMES {
        // The pose is a component write, not a mesh re-upload and not a GPU
        // call: the source packs and uploads it once per frame.
        skin.set_angle(bend_angle(frame));
        let pose = skin.pose();
        *world
            .get_mut::<SkinPose>(pose_entity)
            .expect("the pose entity carries a `SkinPose`") = pose;

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

/// A skinned mesh with no joint stream is rejected rather than drawn
/// undeformed.
#[test]
#[should_panic(expected = "a variant that reads joints needs the mesh's joints")]
fn skinning_without_skin_data_panics() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, true, false));
    gpu.allocate_deformed_cube_mesh(&world, &key, None, None, &[]);
}

/// A skinned mesh drawn without a pose entity is rejected rather than deformed
/// by whatever the frame packed for somebody else.
#[test]
#[should_panic(expected = "a skinned mesh needs a `SkinBinding`")]
fn skinning_without_a_pose_binding_panics() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, true, false));
    let (mesh, _skin) = gpu.allocate_bent_cube_mesh(&world, &key, 0.0);

    world.spawn((camera(),));
    world.spawn((mesh, UnlitPipeline::new(key)));
    gpu.bind_offscreen_target(&world, "test::ecs_skinned_unbound");
    gpu.render(&world);
}

/// One pose entity drives two meshes, and moving it moves both.
#[test]
fn meshes_can_share_one_skin_pose() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, true, false));
    let (first, skin) = gpu.allocate_bent_cube_mesh(&world, &key, 0.0);
    let (second, _) = gpu.allocate_bent_cube_mesh(&world, &key, 0.0);

    // One pose entity, named by both meshes: a crowd of copies of one mesh
    // shares one skeleton instead of uploading a pose each.
    let pose_entity = world.spawn((skin.pose(),));
    world.spawn((camera(),));
    world.spawn((
        Transform {
            translation: glam::Vec3::new(-0.7, 0.0, 0.0),
            ..Default::default()
        },
        first,
        UnlitPipeline::new(key.clone()),
        SkinBinding::new(pose_entity),
    ));
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.7, 0.0, 0.0),
            ..Default::default()
        },
        second,
        UnlitPipeline::new(key),
        SkinBinding::new(pose_entity),
    ));
    let target = gpu.bind_offscreen_target(&world, "test::ecs_skinned_shared");

    // The rest pose and a bent one must render differently: if the source
    // packed one pose per mesh rather than one per instance, the shared entity
    // would reach only one of them.
    let read = |gpu: &TestGpu| {
        gpu.render(&world);
        read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target))
    };
    let rest = read(&gpu);
    let mut bent = skin;
    bent.set_angle(0.6);
    let pose = bent.pose();
    *world
        .get_mut::<SkinPose>(pose_entity)
        .expect("the pose entity carries a `SkinPose`") = pose;
    let deformed = read(&gpu);

    assert_ne!(rest, deformed, "the shared pose must reach both meshes");
}
