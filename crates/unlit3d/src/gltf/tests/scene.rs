//! What a loaded document puts in the world: culling, morph targets and skins.

use super::*;
use crate::lit::LitModel;
use crate::mesh_source::MeshSource;
use crate::source::spawn_context;
use unlit_ecs::World;
use unlit_wgpu::context::RenderDevice;
use unlit_wgpu::resources::ResourceGraph;

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
fn a_target_without_positions_still_owns_a_weight_slot() {
    // A target that displaces nothing this loader reads still occupies its
    // slot in the document's target array, and the weights are indexed by
    // that array: dropping the slot would shift every later target's weight
    // onto the wrong target, deforming the mesh into something the document
    // never states. The mesh therefore counts it and starts at zero.
    let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[None]))
        .expect("the morphed document parses");
    let primitive = gltf.primitive(0, 0);
    assert_eq!(morph_target_count(&primitive), 1);
    assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.0]);
}

#[test]
fn a_weight_lands_on_the_target_its_slot_names() {
    // The first target displaces nothing and the second moves the vertex up,
    // so the second weight is the one that has to reach the second target. A
    // loader that counted only position-displacing targets would give the
    // mesh a single target, put the up-displacement in slot 0 and hand the
    // weight meant for the second target to the first.
    let gltf = LitGltf::from_bytes(&morphed_document(
        None,
        Some(&[0.0, 1.0]),
        &[None, Some([0.0, 1.0, 0.0])],
    ))
    .expect("the morphed document parses");
    assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.0, 1.0]);
    let streams = super::streams::from_reader(&gltf.buffers, &gltf.primitive(0, 0));
    let morph = streams
        .morph_deltas
        .expect("the mesh carries displacements");
    assert_eq!(morph.target_count, 2);
    // Vertex 1 is the one the second target lifts; every vertex keeps three
    // floats per target, so its slot is the second target's triplet.
    let deltas = &morph.deltas;
    let stride = 2 * 3;
    assert_eq!(deltas[stride + 3..stride + 6], [0.0, 1.0, 0.0]);
    assert_eq!(deltas[stride..stride + 3], [0.0, 0.0, 0.0]);
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
    assert_eq!(base_color_uv_set(&primitive), None);
}

#[test]
fn a_node_without_a_skin_has_no_pose() {
    let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
        .expect("the skinned document parses");
    assert_eq!(gltf.skin_joint_count(NodeId::new(0)), Some(1));
    assert_eq!(gltf.skin_joint_count(NodeId::new(1)), None);
    assert!(gltf.skin_pose(NodeId::new(1)).matrices.is_empty());
}

#[test]
fn a_mirror_survives_the_decomposition() {
    // A reflection is not a rotation, so the sign has to land in the scale: a
    // transform that read the axes' positive lengths would recompose to a
    // rotation of the mirrored geometry, which is a different shape. The
    // winding a draw uses is derived from the same determinant, so a mirrored
    // entity draws the side its scale names.
    let matrix = glam::Mat4::from_scale(glam::Vec3::new(-2.0, 3.0, 4.0));
    let transform = Transform::from_matrix(glam::Affine3A::from_mat4(matrix));
    assert!(
        (transform.scale.x + 2.0).abs() < 1e-6,
        "{:?}",
        transform.scale
    );
    assert!(
        (transform.scale.y - 3.0).abs() < 1e-6,
        "{:?}",
        transform.scale
    );
    assert!(
        glam::Mat4::from(transform.compute_matrix()).abs_diff_eq(matrix, 1e-6),
        "{:?}",
        glam::Mat4::from(transform.compute_matrix())
    );
}

#[test]
fn a_model_import_keeps_only_the_default_scene_and_preserves_authored_trs() {
    let gltf = LitGltf::from_bytes(
        br#"{
        "asset":{"version":"2.0"},"scene":0,
        "scenes":[{"nodes":[2]},{"nodes":[0]}],
        "nodes":[{}, {"scale":[1,-2,3]}, {"children":[1]}]
    }"#,
    )
    .unwrap();
    let model = gltf.to_model(&GltfResources::default()).unwrap();
    assert_eq!(model.nodes.len(), 2);
    assert_eq!(model.nodes[0].parent, Some(1));
    assert_eq!(
        model.nodes[0].transform.scale,
        glam::Vec3::new(1.0, -2.0, 3.0)
    );
    assert_eq!(model.nodes[0].transform.rotation, glam::Quat::IDENTITY);
    assert_eq!(model.nodes[1].parent, None);
}

#[test]
fn a_skin_short_of_inverse_bind_matrices_binds_the_rest_at_rest() {
    // The specification asks for one inverse bind matrix per joint, and
    // nothing in the document's JSON states the count, so a skin can list
    // fewer than it has joints. The joints it leaves out bind at the
    // identity, so their own world matrix poses them, rather than the query
    // failing the draw.
    let gltf = LitGltf::from_bytes(&skinned_document_with([1.0, 0.0, 0.0], 2, Some(1)))
        .expect("a skin whose list is short is still a valid document");
    let pose = gltf.skin_pose(NodeId::new(0));
    assert_eq!(pose.matrices.len(), 2, "the skin names two joints");
    assert!(
        pose.matrices[0].abs_diff_eq(glam::Mat4::from_translation(glam::Vec3::X), 1e-6),
        "the joint its one matrix covers is posed by it: {}",
        pose.matrices[0]
    );
    assert!(
        pose.matrices[1].abs_diff_eq(glam::Mat4::from_translation(glam::Vec3::X * 2.0), 1e-6),
        "the joint the list leaves out poses by its own world matrix: {}",
        pose.matrices[1]
    );
}

/// Uploads a document's meshes and materials, the way an application does
/// before converting it to a model: `to_model` names the uploaded meshes of
/// its primitives, so it has nothing to point at without them.
fn uploaded(gltf: &LitGltf) -> (GltfResources, World) {
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let mut world = World::new();
    let ctx = spawn_context(
        &mut world,
        RenderDevice::unwatched(device, queue),
        ResourceGraph::new(),
    );
    let mut source = MeshSource::new(&world.root_scope(), ctx);
    source.register_lit_family(&world.root_scope());
    let resources = gltf
        .insert_resources(&mut source, &world.root_scope())
        .expect("every image, material and mesh the document names uploads");
    (resources, world)
}

/// The pose a model states for the node it imported, as the matrices the
/// document's own `skin_pose` multiplies out.
fn model_pose(gltf: &LitGltf, model: &LitModel, node: usize) -> Vec<glam::Mat4> {
    let skin = model.nodes[node]
        .skin
        .as_ref()
        .expect("the node is skinned");
    skin.joints()
        .iter()
        .zip(skin.inverse_bind())
        .map(|(&joint, &bind)| gltf.node_matrices[joint] * bind)
        .collect()
}

#[test]
fn a_model_import_poses_a_skin_short_of_inverse_bind_matrices_the_same_way() {
    // The document path and the model path read one skin through one decoder,
    // so a list that falls short of the joints it names binds the joints it
    // leaves out at the identity in both, rather than the model holding an
    // empty list that the frame pass indexes past.
    let gltf = LitGltf::from_bytes(&skinned_document_with([1.0, 0.0, 0.0], 2, Some(1)))
        .expect("a skin whose list is short is still a valid document");
    let (resources, _world) = uploaded(&gltf);
    let model = gltf.to_model(&resources).expect("the model imports");
    let pose = gltf.skin_pose(NodeId::new(0));
    let imported = model_pose(&gltf, &model, 0);
    assert_eq!(
        imported.len(),
        pose.matrices.len(),
        "both paths pose one matrix per joint"
    );
    for (matrix, posed) in imported.iter().zip(&pose.matrices) {
        assert!(
            matrix.abs_diff_eq(*posed, 1e-6),
            "the model poses the joint as the document does: {matrix} against {posed}"
        );
    }
    let skin = model.nodes[0].skin.as_ref().unwrap();
    assert_eq!(skin.joints(), [1, 2], "the skin names both joints");
    assert_eq!(
        skin.inverse_bind()[1],
        glam::Mat4::IDENTITY,
        "the joint the list leaves out binds at the identity"
    );
}

#[test]
fn a_model_import_binds_a_skin_without_inverse_bind_matrices_at_rest() {
    // A skin that states no `inverseBindMatrices` is a valid document: every
    // joint binds at the identity, so the joint nodes' own world transforms
    // are the whole pose.
    let gltf = LitGltf::from_bytes(&skinned_document_with([1.0, 0.0, 0.0], 2, None))
        .expect("a skin without inverse bind matrices is still a valid document");
    let (resources, _world) = uploaded(&gltf);
    let model = gltf.to_model(&resources).expect("the model imports");
    let skin = model.nodes[0].skin.as_ref().unwrap();
    assert_eq!(skin.joints(), [1, 2]);
    assert_eq!(
        skin.inverse_bind(),
        [glam::Mat4::IDENTITY, glam::Mat4::IDENTITY],
        "every joint binds at the identity"
    );
    let pose = gltf.skin_pose(NodeId::new(0));
    assert_eq!(pose.matrices.len(), 2);
    assert!(
        pose.matrices[1].abs_diff_eq(glam::Mat4::from_translation(glam::Vec3::X * 2.0), 1e-6),
        "the second joint poses by its own world matrix: {}",
        pose.matrices[1]
    );
}

#[test]
fn a_model_import_truncates_a_skin_with_more_inverse_bind_matrices_than_joints() {
    // Nothing in the JSON ties the accessor's count to the joint list, so a
    // document can state more matrices than it names joints. The extra
    // matrices belong to joints the skin does not name and are dropped.
    let gltf = LitGltf::from_bytes(&skinned_document_with([1.0, 0.0, 0.0], 2, Some(3)))
        .expect("a skin whose list runs past its joints is still a valid document");
    let (resources, _world) = uploaded(&gltf);
    let model = gltf.to_model(&resources).expect("the model imports");
    let skin = model.nodes[0].skin.as_ref().unwrap();
    assert_eq!(skin.joints(), [1, 2]);
    assert_eq!(
        skin.inverse_bind().len(),
        skin.joints().len(),
        "the pose states one matrix per joint"
    );
    // Matrix `i` scales by `i + 1`, so the two kept are the only ones that can
    // land here.
    assert_eq!(
        skin.inverse_bind()[0],
        glam::Mat4::from_scale(glam::Vec3::splat(1.0))
    );
    assert_eq!(
        skin.inverse_bind()[1],
        glam::Mat4::from_scale(glam::Vec3::splat(2.0))
    );
}
