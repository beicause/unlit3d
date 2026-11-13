//! Deforming the drawn mesh: morph weights, skin poses and an animation clip.

use super::documents::*;
use super::*;

/// A morphed quad follows its weights: spawning binds the mesh to a weight
/// entity holding the document's starting weights, and writing that entity
/// blends the targets into the geometry.
///
/// The weights are a component of an entity of their own — the one the spawn
/// created — so writing it is how a caller animates a morph.
pub(super) async fn a_morphed_quad_follows_its_weights() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&morphed_document()).expect("the embedded document parses");

    assert_eq!(
        gltf.morph_weights(NodeId::new(0)),
        vec![0.0, 0.0],
        "the mesh's own weights are the starting pose"
    );

    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    assert!(
        resources.meshes[0].mesh.morph_targets > 0,
        "the primitive declares targets, so the uploaded mesh reads displacements"
    );

    let entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    assert_eq!(entities.len(), 1, "one mesh node draws one entity");

    let weights_entity = {
        let mut morph_binding_rows = world.root_scope().query::<&MorphBinding>();

        let morph_binding = morph_binding_rows
            .iter()
            .next()
            .expect("a morphed mesh binds to a weight entity");
        morph_binding.weights
    };
    assert_eq!(
        world
            .root_scope()
            .query_one::<&MorphWeights>(weights_entity)
            .expect("the weight entity carries a `MorphWeights`")
            .get()
            .weights,
        vec![0.0, 0.0],
        "the spawn starts at the document's weights"
    );

    gpu.spawn(&mut world, bundle![camera()]);
    let target = gpu.bind_offscreen_target(&world, "test::gltf_morphed");

    let read = async |gpu: &TestGpu| {
        gpu.render(&world);
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await
    };
    let undeformed = read(&gpu).await;
    let red = count_colour(&undeformed, [255, 0, 0], 4);
    assert!(
        red > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the unweighted quad covers the frame (red = {red})"
    );

    // The first target collapses the quad onto a point, so weighting it fully
    // must empty the frame.
    world
        .root_scope()
        .query_one::<&mut MorphWeights>(weights_entity)
        .expect("the weight entity carries a `MorphWeights`")
        .get()
        .weights = vec![1.0, 0.0];
    let collapsed = read(&gpu).await;
    assert_eq!(
        count_colour(&collapsed, [255, 0, 0], 4),
        0,
        "a fully weighted collapsing target leaves nothing drawn"
    );

    // The second target pushes the quad past the viewport, so it empties the
    // frame too — by different geometry than the first, which is what shows
    // the two weights address their own target's displacements.
    world
        .root_scope()
        .query_one::<&mut MorphWeights>(weights_entity)
        .expect("the weight entity carries a `MorphWeights`")
        .get()
        .weights = vec![0.0, 1.0];
    let displaced = read(&gpu).await;
    assert_eq!(
        count_colour(&displaced, [255, 0, 0], 4),
        0,
        "a fully weighted displacement target takes the quad off-screen"
    );
}

/// An animation drives the drawn geometry: at `t = 0` the skinned quad covers
/// the frame, and by `t = 2` its animated joint has carried it off-screen.
///
/// This is the whole path in one assertion: a channel samples, the hierarchy
/// recomposes, the skin pose is rebuilt against the animated matrices, and the
/// renderer draws the result.
pub(super) async fn an_animation_moves_the_drawn_mesh() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf =
        LitGltf::from_bytes(&animated_skinned_document()).expect("the embedded document parses");

    assert_eq!(gltf.animation_count(), 1, "the document carries one clip");
    assert_eq!(gltf.animation_name(AnimationId::new(0)), Some("walk"));
    assert!(
        (gltf.animation_duration(AnimationId::new(0)) - 2.0).abs() < 1e-6,
        "the clip ends at its last keyframe"
    );

    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    let nodes = spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);
    let target = gpu.bind_offscreen_target(&world, "test::gltf_animated");

    let read = async |gpu: &TestGpu, time: f32| {
        gltf.apply_animation(&world.root_scope(), AnimationId::new(0), time, &nodes);
        gpu.render(&world);
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await
    };

    let rest = read(&gpu, 0.0).await;
    let red = count_colour(&rest, [255, 0, 0], 4);
    assert!(
        red > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the quad covers the frame at the animation's start (red = {red})"
    );

    // The joint's second keyframe takes it past the viewport's edge, so the
    // quad it skins follows it out of the frame.
    let moved = read(&gpu, 2.0).await;
    assert_eq!(
        count_colour(&moved, [255, 0, 0], 4),
        0,
        "the animation carries the skinned quad off-screen"
    );
}

/// A skinned quad follows its joint: at rest it covers the frame, and moving
/// the joint entity slides the quad across it.
///
/// The pose is a component of its own entity — the one `spawn_default_scene`
/// created and the drawn mesh binds to — so writing it is how a caller
/// animates the skin.
pub(super) async fn a_skinned_quad_follows_its_joint() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    // The joint at rest: the pose's own matrix is the identity, so the quad
    // is exactly where the document put it.
    let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
        .expect("the embedded document parses");
    assert_eq!(
        gltf.skin_joint_count(NodeId::new(0)),
        Some(1),
        "the skin has one joint"
    );

    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    assert!(
        resources.meshes[0].mesh.skinned,
        "the primitive declares JOINTS_0, so the uploaded mesh reads joints"
    );

    let entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    assert_eq!(entities.len(), 1, "one mesh node draws one entity");

    // The drawn entity names the pose entity the spawn created, and that
    // entity holds the rest pose: the identity, one matrix per joint.
    let pose = {
        let mut skin_binding_rows = world.root_scope().query::<&SkinBinding>();

        let skin_binding = skin_binding_rows
            .iter()
            .next()
            .expect("a skinned mesh binds to a pose");
        skin_binding.pose
    };
    let rest = {
        let mut pose_row = world
            .root_scope()
            .query_one::<&SkinPose>(pose)
            .expect("the pose entity carries a `SkinPose`");
        pose_row.get().matrices.clone()
    };
    assert_eq!(rest.len(), 1, "one joint, one matrix");
    assert!(
        rest[0].abs_diff_eq(glam::Mat4::IDENTITY, 1e-6),
        "the rest pose deforms nothing: {}",
        rest[0]
    );

    gpu.spawn(&mut world, bundle![camera()]);
    let target = gpu.bind_offscreen_target(&world, "test::gltf_skinned");

    let read = async |gpu: &TestGpu| {
        gpu.render(&world);
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await
    };
    let before = read(&gpu).await;
    let red = count_colour(&before, [255, 0, 0], 4);
    assert!(
        red > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the skinned quad covers the frame at rest (red = {red})"
    );

    // Move the joint a whole screen to the left: the quad follows it, and the
    // right side of the frame is left empty.
    *world
        .root_scope()
        .query_one::<&mut SkinPose>(pose)
        .expect("the pose entity carries a `SkinPose`")
        .get() = SkinPose {
        matrices: vec![glam::Mat4::from_translation(glam::Vec3::new(
            -5.0, 0.0, 0.0,
        ))],
    };
    let after = read(&gpu).await;
    let red_after = count_colour(&after, [255, 0, 0], 4);
    assert!(
        red_after < red,
        "moving the joint must move the geometry (before = {red}, after = {red_after})"
    );
    assert_eq!(
        red_after, 0,
        "a joint moved past the viewport takes the quad with it"
    );
}
