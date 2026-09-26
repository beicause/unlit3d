//! GPU coverage for vertex skinning: the validation of a skinned mesh's
//! joint stream and pose binding, and a pose shared by several meshes.
//!
//! The multi-frame snapshot coverage that used to live here now runs in
//! `unlit3d_examples` (the `ecs_skinned` scene), whose headless path verifies
//! the same frames against the stored snapshots.

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// Where the camera sits, close enough that a deformation moves many pixels.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.9, 1.1, 2.6);

/// The camera the remaining tests share.
fn camera() -> Camera {
    camera_looking_at(
        CAMERA_EYE,
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
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
