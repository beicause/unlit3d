//! Playing a clip: what it moves, how it resamples, and what it leaves alone.

use super::*;

#[test]
fn an_animation_reports_its_name_and_duration() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 1.5, 4.0],
        &[[0.0; 3]; 3],
        &[[0.0, 0.0, 0.0, 1.0]; 3],
    ))
    .expect("the animated document parses");
    assert_eq!(gltf.animation_count(), 1);
    assert_eq!(gltf.animation_name(AnimationId::new(0)), Some("walk"));
    assert!((gltf.animation_duration(AnimationId::new(0)) - 4.0).abs() < 1e-6);
}

#[test]
fn apply_animation_moves_a_node_and_carries_its_child() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[
            [0.0, 0.0, 0.0, 1.0],
            [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        ],
    ))
    .expect("the animated document parses");
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let child = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let nodes = [
        GltfNode {
            node: NodeId::new(0),
            entities: vec![parent],
            pose: None,
            weights: None,
        },
        GltfNode {
            node: NodeId::new(1),
            entities: vec![child],
            pose: None,
            weights: None,
        },
    ];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &nodes);
    // The parent's own channel moves it...
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Transform>(parent)
            .expect("the parent carries a transform")
            .get()
            .translation,
        glam::Vec3::new(2.0, 0.0, 0.0)
    );
    let mut transform_row = world
        .root_scope()
        .query_one::<&Transform>(child)
        .expect("the child carries a transform");

    // ...and the child, which no translation channel names, inherits it
    // along with the quarter turn its own channel reaches at the midpoint.
    let child = transform_row.get();
    assert_eq!(child.translation, glam::Vec3::new(2.0, 0.0, 0.0));
    let quarter = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
    assert!(child.rotation.dot(quarter).abs() > 1.0 - 1e-5, "{child:?}");
}

#[test]
fn apply_animation_holds_a_step_keyframe() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "STEP",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0]; 2],
    ))
    .expect("the animated document parses");
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![parent],
        pose: None,
        weights: None,
    }];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.999, &nodes);
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Transform>(parent)
            .expect("the parent carries a transform")
            .get()
            .translation,
        glam::Vec3::ZERO
    );
}

#[test]
fn apply_animation_resamples_a_clip_it_has_already_played() {
    // The keyframes are decoded once and kept, so a second frame has to
    // re-sample them rather than replay what the first one wrote.
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0]; 2],
    ))
    .expect("the animated document parses");
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![parent],
        pose: None,
        weights: None,
    }];
    let translation = |world: &mut World| {
        world
            .root_scope()
            .query_one::<&Transform>(parent)
            .expect("the parent carries a transform")
            .get()
            .translation
    };
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 0.5, &nodes);
    assert_eq!(translation(&mut world), glam::Vec3::new(1.0, 0.0, 0.0));
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.5, &nodes);
    assert_eq!(translation(&mut world), glam::Vec3::new(3.0, 0.0, 0.0));
}

#[test]
fn two_documents_animate_in_the_same_frame() {
    // One document's working set is handed back before the next takes it, so a
    // frame that animates two of them gets each one's own result.
    let left = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0]; 2],
    ))
    .expect("the animated document parses");
    let right = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [0.0, 8.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0]; 2],
    ))
    .expect("the animated document parses");
    let mut world = World::new();
    let left_entity = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let right_entity = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let left_nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![left_entity],
        pose: None,
        weights: None,
    }];
    let right_nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![right_entity],
        pose: None,
        weights: None,
    }];
    left.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &left_nodes);
    right.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &right_nodes);
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Transform>(left_entity)
            .expect("the entity carries a transform")
            .get()
            .translation,
        glam::Vec3::new(2.0, 0.0, 0.0)
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Transform>(right_entity)
            .expect("the entity carries a transform")
            .get()
            .translation,
        glam::Vec3::new(0.0, 4.0, 0.0)
    );
}

#[test]
fn apply_animation_rebuilds_a_skin_pose() {
    let gltf = LitGltf::from_bytes(&animated_skinned_document(
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
    ))
    .expect("the animated skinned document parses");
    let mut world = World::new();
    let mesh = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let pose = world.root_scope_mut().spawn(bundle![SkinPose::default()]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![mesh],
        pose: Some(pose),
        weights: None,
    }];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &nodes);
    let mut skin_pose_row = world
        .root_scope()
        .query_one::<&SkinPose>(pose)
        .expect("the pose entity carries a pose");

    let pose = skin_pose_row.get();
    assert!(
        pose.matrices[0].abs_diff_eq(
            glam::Mat4::from_translation(glam::Vec3::new(2.0, 0.0, 0.0)),
            1e-6
        ),
        "{}",
        pose.matrices[0]
    );
}

#[test]
fn apply_animation_writes_morph_weights() {
    let gltf = LitGltf::from_bytes(&animated_morphed_document(&[0.0, 2.0], &[0.0, 2.0]))
        .expect("the animated morphed document parses");
    let mut world = World::new();
    let mesh = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let weights = world
        .root_scope_mut()
        .spawn(bundle![MorphWeights { weights: vec![0.0] }]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![mesh],
        pose: None,
        weights: Some(weights),
    }];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &nodes);
    let mut morph_weights_row = world
        .root_scope()
        .query_one::<&MorphWeights>(weights)
        .expect("the weights entity carries weights");

    let weights = morph_weights_row.get();
    assert_eq!(weights.weights, vec![1.0]);
}
