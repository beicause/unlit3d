use super::*;

#[test]
fn mesh_instance_from_transform() {
    let t = Transform {
        translation: glam::Vec3::new(1.0, 2.0, 3.0),
        rotation: glam::Quat::IDENTITY,
        scale: glam::Vec3::ONE,
    };
    let instance = MeshInstance::new(t.compute_matrix(), glam::Vec4::new(1.0, 1.0, 1.0, 1.0));
    assert_eq!(instance.translation(), glam::Vec3::new(1.0, 2.0, 3.0));
    // The color is quantized to the `Unorm8x4` the stream carries.
    assert_eq!(instance.base_color, [u8::MAX; 4]);
}

// -- pipeline registration ---------------------------------------------

#[test]
fn indexed_meshes_share_one_index_buffer() {
    let mut h = harness();
    let first = h.tri_mesh();
    let second = h.tri_mesh();

    // Both name the pool's node, and each names its own slice of it.
    let pool = h.source.index_pool_id.clone();
    assert_eq!(
        first.parts.index_buffer.as_ref().map(|(id, _)| id.clone()),
        Some(pool.clone())
    );
    assert_eq!(
        second.parts.index_buffer.as_ref().map(|(id, _)| id.clone()),
        Some(pool)
    );
    assert_ne!(first.first, second.first, "the slices do not overlap");

    // The ranges tile the pool in allocation order.
    let first_range = h.source.index_pool.allocation_size(
        first
            .parts
            .index_allocation
            .expect("the mesh holds an allocation"),
    );
    assert_eq!(
        first.first + first_range / size_of::<u16>() as u32,
        second.first,
        "the second mesh starts where the first ends"
    );
}

#[test]
fn a_removed_mesh_frees_its_index_range_for_the_next_mesh() {
    let mut h = harness();
    let first = h.tri_mesh();
    let first_offset = first.first;
    h.source.remove_mesh(first);

    let again = h.tri_mesh();
    assert_eq!(
        again.first, first_offset,
        "the freed range is handed out again"
    );
}

#[test]
fn an_index_pool_that_grows_keeps_every_meshes_slice() {
    let mut h = harness();
    let ctx = h.source.context();
    let first = h.tri_mesh();
    let first_node = first
        .parts
        .index_buffer
        .clone()
        .expect("the mesh is indexed")
        .0;
    let first_offset = first.first;

    // Enough meshes to push the pool past its starting size.
    let mut last = None;
    for _ in 0..40 {
        last = Some(h.tri_mesh());
    }
    let last = last.expect("the loop runs");

    // The node still names the pool, and the graph holds the buffer the
    // pool currently does: a mesh needs no update after a grow.
    assert_eq!(
        last.parts.index_buffer.as_ref().map(|(id, _)| id.clone()),
        Some(first_node.clone())
    );
    assert_eq!(
        ctx.graph(&h.world.root_scope()).get().get(&first_node),
        Some(h.source.index_pool.buffer()),
        "the node follows the pool's buffer"
    );
    assert_eq!(
        first.first, first_offset,
        "the first mesh's slice did not move"
    );
    assert!(
        h.source.index_pool.size() > first_offset as u64,
        "the pool grew"
    );
}

#[test]
fn a_non_indexed_mesh_holds_no_index_allocation() {
    let mut h = harness();
    let key = LitPipelineKey::new(unlit_options());
    let mesh = h.source.allocate_lit_mesh(
        &h.world.root_scope(),
        &key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            indices: None,
            ..Default::default()
        },
    );

    assert!(!mesh.indexed);
    assert!(mesh.parts.index_buffer.is_none());
    assert!(mesh.parts.index_allocation.is_none());
    assert_eq!(mesh.first, 0);
}

#[test]
fn meshes_sharing_a_layout_share_one_buffer_per_stream() {
    let mut h = harness();
    let first = h.tri_mesh();
    let second = h.tri_mesh();
    let ctx = h.source.context();

    // Every stream of a mesh names its pool's node, and both meshes name
    // the same node per slot.
    for ((_, first_id), (_, second_id)) in first
        .parts
        .vertex_buffers
        .iter()
        .zip(second.parts.vertex_buffers.iter())
    {
        assert_eq!(first_id, second_id, "the streams share a buffer");
        assert!(
            ctx.graph(&h.world.root_scope())
                .get()
                .get(first_id)
                .is_some(),
            "the stream's node holds a buffer"
        );
    }
    // One element allocation covers both streams at the same index, which
    // is what makes a single `base_vertex` address them all.
    assert_eq!(first.base_vertex, 0);
    assert_ne!(second.base_vertex, first.base_vertex);
}

#[test]
fn a_removed_mesh_frees_its_vertex_range_for_the_next_mesh() {
    let mut h = harness();
    let first = h.tri_mesh();
    let vertex_offset = first.base_vertex;
    h.source.remove_mesh(first);

    let again = h.tri_mesh();
    assert_eq!(
        again.base_vertex, vertex_offset,
        "the freed element range is handed out again"
    );
}

#[test]
fn a_vertex_pool_that_grows_keeps_every_meshes_element_index() {
    let mut h = harness();
    let ctx = h.source.context();
    let first = h.tri_mesh();
    let nodes: Vec<_> = first
        .parts
        .vertex_buffers
        .iter()
        .map(|(_, id)| id.clone())
        .collect();
    let base_vertex = first.base_vertex;

    // Enough meshes to push the pool past its starting capacity.
    let mut last = None;
    for _ in 0..80 {
        last = Some(h.tri_mesh());
    }
    let last = last.expect("the loop runs");

    // The nodes still name the streams, and each holds the buffer the pool
    // currently does for its layout: a mesh needs no update after a grow.
    // Every slot of the mesh's layout is pool-backed: the per-instance
    // stream is the family's own and is not part of the layout at all.
    for (node, (_, layout)) in nodes.iter().zip(last.vertex_layout.iter()) {
        let pool_buffer = h
            .source
            .vertex_pool
            .buffer(layout)
            .expect("the layout has a buffer");
        assert_eq!(
            ctx.graph(&h.world.root_scope()).get().get(node),
            Some(pool_buffer),
            "the node follows the pool's buffer"
        );
    }
    assert_eq!(
        first.base_vertex, base_vertex,
        "the first mesh's element index did not move"
    );
    assert!(
        h.source.vertex_pool.element_capacity() > base_vertex,
        "the pool grew"
    );
}

/// Each entry carries its own shape and vertex buffers, so an indexed mesh
/// followed by another one is addressed by its own slice of the shared pool
/// rather than reading across entries.
#[test]
fn an_indexed_mesh_followed_by_another_keeps_its_own_slice() {
    let mut h = harness();
    let key = LitPipelineKey::new(alternative_options());
    // Both meshes are indexed and share the pool's index buffer, so what
    // tells their draws apart is the slice each one names.
    let first = h.source.allocate_lit_mesh(
        &h.world.root_scope(),
        &key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            indices: Some(&[0u32, 1, 2]),
            ..Default::default()
        },
    );
    let second = h.source.allocate_lit_mesh(
        &h.world.root_scope(),
        &key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            indices: Some(&[0u32, 1, 2]),
            ..Default::default()
        },
    );
    assert!(first.indexed && second.indexed, "both meshes are indexed");
    assert_ne!(first.first, second.first, "the slices do not overlap");

    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        first,
        GpuRenderPipeline::new(key.clone()),
    ]);
    h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        second,
        GpuRenderPipeline::new(key),
    ]);

    let ctx = h.source.context();
    let mut encoder = h.encoder();
    let mut scene = Scene::new();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );
    let shapes: Vec<DrawShape> = h
        .source
        .visible_cache
        .iter()
        .map(|entry| entry.handles_key.shape)
        .collect();
    assert_eq!(
        h.source.entry_handle_cache.len(),
        2,
        "each distinct slice interns its own handles"
    );
    assert_eq!(shapes[0].first, 0, "the first mesh starts at zero");
    assert_ne!(
        shapes[0].first, shapes[1].first,
        "each draw keeps its own slice of the pool"
    );
    assert_eq!(drawn(&h.source).len(), 2, "both entities were drawn");
}
