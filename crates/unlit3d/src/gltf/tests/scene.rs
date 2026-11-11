//! What a loaded document puts in the world: culling, morph targets and skins.

use super::*;

#[test]
fn a_double_sided_material_culls_nothing() {
    let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());

    let single =
        LitGltf::from_bytes(&sided_document(false)).expect("the single-sided document parses");
    assert_eq!(
        single
            .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0))
            .options
            .primitive
            .cull_mode,
        Some(wgpu::Face::Back),
        "a material that is not double-sided culls back faces"
    );

    let double = LitGltf::from_bytes(&sided_document(true)).expect("the sided document parses");
    assert_eq!(
        double
            .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0))
            .options
            .primitive
            .cull_mode,
        None,
        "a double-sided material culls nothing"
    );
}

#[test]
fn culling_alone_distinguishes_the_two_sided_variants() {
    let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let single = LitGltf::from_bytes(&sided_document(false))
        .expect("the single-sided document parses")
        .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0));
    let double = LitGltf::from_bytes(&sided_document(true))
        .expect("the double-sided document parses")
        .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0));
    assert_ne!(
        single, double,
        "the two sides must be separate variants so each keeps its own pipeline"
    );
    assert_eq!(
        single.options.primitive.cull_mode,
        Some(wgpu::Face::Back),
        "a single-sided primitive culls its back faces"
    );
    assert_eq!(
        double.options.primitive.cull_mode, None,
        "a double-sided material culls nothing"
    );
    // Nothing but the cull mode sets the two apart. The options are
    // hashed, so the rewrite goes through `update`: the word follows the
    // changed field and the equality check stays sound.
    let mut normalized = single.options.clone();
    normalized.update(|options| options.primitive.cull_mode = double.options.primitive.cull_mode);
    assert_eq!(normalized, double.options);
}

#[test]
fn a_morph_target_makes_the_mesh_morph() {
    let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[Some([0.0, 1.0, 0.0])]))
        .expect("the morphed document parses");
    let primitive = gltf.primitive(0, 0);
    assert_eq!(morph_target_count(&primitive), 1);
}

#[test]
fn a_target_without_positions_does_not_make_the_mesh_morph() {
    // A normals-only target displaces nothing this loader reads, so the
    // mesh stays in the rigid path rather than drawing an empty morph.
    let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[None]))
        .expect("the morphed document parses");
    let primitive = gltf.primitive(0, 0);
    assert_eq!(morph_target_count(&primitive), 0);
    assert!(gltf.morph_weights(NodeId::new(0)).is_empty());
}

#[test]
fn a_node_overrides_the_mesh_weights() {
    let gltf = LitGltf::from_bytes(&morphed_document(
        Some(&[0.25]),
        Some(&[0.75]),
        &[Some([0.0, 1.0, 0.0])],
    ))
    .expect("the morphed document parses");
    assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.75]);

    let gltf = LitGltf::from_bytes(&morphed_document(
        Some(&[0.25]),
        None,
        &[Some([1.0, 0.0, 0.0])],
    ))
    .expect("the morphed document parses");
    assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.25]);
}

#[test]
fn morph_weights_are_padded_to_the_target_count() {
    // A document that states fewer weights than it has targets would make
    // the renderer reject the mesh, so the shortfall is padded with the
    // undeformed weight rather than passed on.
    let gltf = LitGltf::from_bytes(&morphed_document(
        Some(&[0.5]),
        None,
        &[Some([0.0, 1.0, 0.0]), Some([1.0, 0.0, 0.0])],
    ))
    .expect("the morphed document parses");
    assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.5, 0.0]);

    // And one that states more is truncated, for the same reason.
    let gltf = LitGltf::from_bytes(&morphed_document(
        Some(&[0.5, 1.0]),
        None,
        &[Some([0.0, 1.0, 0.0])],
    ))
    .expect("the morphed document parses");
    assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.5]);
}

#[test]
fn a_mesh_without_targets_starts_undeformed() {
    let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[]))
        .expect("the morphed document parses");
    assert!(gltf.morph_weights(NodeId::new(0)).is_empty());
    assert_eq!(morph_target_count(&gltf.primitive(0, 0)), 0);
}

#[test]
fn a_rest_skin_pose_deforms_nothing() {
    // The joint sits at rest, so its world matrix is the inverse bind
    // matrix's inverse and every joint matrix comes out the identity: the
    // pose a document uploads at rest has to leave the mesh exactly where
    // it is.
    let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
        .expect("the skinned document parses");
    let pose = gltf.skin_pose(NodeId::new(0));
    assert_eq!(pose.matrices.len(), 1, "the skin has one joint");
    assert!(
        pose.matrices[0].abs_diff_eq(glam::Mat4::IDENTITY, 1e-6),
        "{}",
        pose.matrices[0]
    );
}

#[test]
fn a_joint_away_from_its_bind_pose_moves_the_mesh() {
    // An inverse bind matrix records where a joint was *bound*, so a joint
    // node that sits somewhere else pulls the vertices it weights to
    // itself: the pose is the joint's world matrix against its bind
    // matrix, not the identity.
    let gltf = LitGltf::from_bytes(&skinned_document([3.0, -2.0, 1.0]))
        .expect("the skinned document parses");
    let pose = gltf.skin_pose(NodeId::new(0));
    assert!(
        pose.matrices[0].abs_diff_eq(
            glam::Mat4::from_translation(glam::Vec3::new(3.0, -2.0, 1.0)),
            1e-6
        ),
        "{}",
        pose.matrices[0]
    );
}

#[test]
fn a_skinned_primitive_declares_its_joint_stream() {
    // The primitive carries JOINTS_0 and WEIGHTS_0, so the mesh is
    // uploaded with them and the variant a draw resolves reads them.
    let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
        .expect("the skinned document parses");
    let primitive = gltf.primitive(0, 0);
    assert!(primitive.get(&gltf::Semantic::Joints(0)).is_some());
    assert!(primitive.get(&gltf::Semantic::Weights(0)).is_some());
    assert!(!samples_base_color_texture(&primitive));
}

#[test]
fn a_node_without_a_skin_has_no_pose() {
    let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
        .expect("the skinned document parses");
    assert_eq!(gltf.skin_joint_count(NodeId::new(0)), Some(1));
    assert_eq!(gltf.skin_joint_count(NodeId::new(1)), None);
    assert!(gltf.skin_pose(NodeId::new(1)).matrices.is_empty());
}
