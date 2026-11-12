use super::*;

#[test]
fn a_material_is_allocated_from_caller_owned_resources() {
    let mut h = harness();
    let (view, sampler) = test_material_resources(&mut h);

    let material = h.source.allocate_lit_material(
        &h.world.root_scope(),
        &h.key.options,
        view.clone(),
        sampler.clone(),
    );
    let ctx = h.source.context();
    assert!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&material.bind_group_id)
            .is_some()
    );
}

// -- removal -----------------------------------------------------------

#[test]
fn removing_a_mesh_frees_every_resource_built_from_it() {
    let mut h = harness();
    // A morphing mesh, so the mesh holds a displacement range of its own
    // alongside its vertex and index ranges.
    let mesh = morph_mesh(&mut h);
    let ctx = h.source.context();

    // The root is the mesh's lifetime entry point; the mesh's vertices,
    // indices and displacements live in pools, so those are nodes the
    // source owns and survive the mesh — what the mesh loses is its share
    // of them, which `remove_mesh` hands back.
    let before = ctx.graph(&h.world.root_scope()).get().len();
    let index_pool_free = h.source.pools.index_pool.free_space();
    assert!(matches!(
        ctx.graph(&h.world.root_scope()).get().get(&mesh.parts.root),
        Some(Virtual)
    ));
    assert!(
        mesh.parts.morph_deltas_allocation.is_some(),
        "a morphing mesh holds a displacement range"
    );

    // Giving the mesh up drops the last id to its root; the nodes go at
    // the frame's own maintain, which is what this drives by hand. No id
    // to the root is kept: an id is a strong reference, so holding one
    // would keep the very node this test watches the graph collect.
    h.source.remove_mesh(mesh);
    h.source.maintain(&h.world.root_scope());

    assert!(
        ctx.graph(&h.world.root_scope()).get().len() < before,
        "the graph shrank"
    );
    assert!(
        h.source.pools.index_pool.free_space() > index_pool_free,
        "the mesh's index range is free again"
    );
}

#[test]
fn a_mesh_registers_its_pooled_parts_nowhere_under_its_root() {
    let mut h = harness();
    // A caller-supplied bind group, which is the one part a mesh can own
    // outright: the built-in variants bind nothing per mesh, so their
    // meshes carry none.
    let bind_group = h.empty_bind_group();
    let mesh = h.source.allocate_mesh(
        &h.world.root_scope(),
        MeshDesc {
            vertex_buffers: ArrayVec::new(),
            count: 3,
            bind_group: Some(bind_group.clone()),
            ..Default::default()
        },
    );
    let ctx = h.source.context();

    // The pools are the source's, not the mesh's: a mesh that goes away
    // must not take the shared buffers with it, so they sit outside the
    // root and the root holds only what is the mesh's own.
    let dependencies: Vec<_> = ctx
        .graph(&h.world.root_scope())
        .get()
        .dependencies(&mesh.parts.root)
        .collect();
    let bind_group = mesh
        .parts
        .bind_group_id
        .clone()
        .expect("the mesh has a group");
    assert!(
        dependencies.contains(&bind_group.erase()),
        "the bind group is under the root"
    );
    let pool = h.source.pools.index_pool_id;
    assert!(
        !dependencies.contains(&pool.erase()),
        "the index pool is not under the root"
    );
    assert!(
        ctx.graph(&h.world.root_scope()).get().get(&pool).is_some(),
        "the index pool survives"
    );
}

#[test]
fn a_removed_mesh_frees_its_metadata_slot() {
    let mut h = harness();
    let first = h.tri_mesh();
    let second = h.tri_mesh();
    let first_index = first.parts.metadata_index;
    assert_ne!(first_index, second.parts.metadata_index);

    h.source.remove_mesh(first);
    let reused = h.tri_mesh();

    assert_eq!(
        reused.parts.metadata_index, first_index,
        "the slot the removed mesh held is handed out again"
    );
    assert_ne!(reused.parts.metadata_index, second.parts.metadata_index);
}

#[test]
fn dropping_a_material_keeps_the_resources_it_reads() {
    let mut h = harness();
    let ctx = h.source.context();
    let (view, sampler) = test_material_resources(&mut h);
    let material = h.source.allocate_lit_material(
        &h.world.root_scope(),
        &h.key.options,
        view.clone(),
        sampler.clone(),
    );

    // Giving the handle up is the whole of it: an id is a strong
    // reference, so a clone kept here would keep the bind group alive and
    // the assertion below would have nothing to observe.
    let before = ctx.graph(&h.world.root_scope()).get().len();
    drop(material);
    h.source.maintain(&h.world.root_scope());

    assert!(
        ctx.graph(&h.world.root_scope()).get().len() < before,
        "the bind group is gone"
    );
    // The view and sampler are the caller's, so giving up the material
    // that reads them leaves them alone.
    assert!(
        ctx.graph(&h.world.root_scope()).get().get(&view).is_some(),
        "the view stays"
    );
    assert!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&sampler)
            .is_some(),
        "the sampler stays"
    );
}

// -- draw ordering ------------------------------------------------------

#[test]
fn the_metadata_buffer_grows_only_when_the_array_outgrows_it() {
    let mut h = harness();
    let ctx = h.source.context();
    // The source starts with room for one entry and grows by 1.5x.
    assert_eq!(h.source.arrays.metadata_array.capacity(), 1);

    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert_eq!(
        h.source.arrays.metadata_array.capacity(),
        1,
        "one entry fills the room"
    );

    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert_eq!(
        h.source.arrays.metadata_array.capacity(),
        2,
        "a second entry outgrows room for one"
    );

    // A third and fourth entry outgrow the 1.5x capacity in turn.
    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert_eq!(h.source.arrays.metadata_array.capacity(), 3);

    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert_eq!(h.source.arrays.metadata_array.capacity(), 4);

    // A fifth entry outgrows four, so the buffer grows again.
    let before = ctx
        .graph(&h.world.root_scope())
        .get()
        .get(&h.source.arrays.metadata_buf)
        .cloned();
    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert_eq!(
        h.source.arrays.metadata_array.capacity(),
        6,
        "growing past four reaches six (4 * 1.5)"
    );
    assert_ne!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&h.source.arrays.metadata_buf),
        before.as_ref(),
        "growth replaced the buffer"
    );

    // A sixth entry fits in six, so the buffer is left alone.
    let before = ctx
        .graph(&h.world.root_scope())
        .get()
        .get(&h.source.arrays.metadata_buf)
        .cloned();
    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert_eq!(
        h.source.arrays.metadata_array.capacity(),
        6,
        "six entries fit"
    );
    assert_eq!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&h.source.arrays.metadata_buf),
        before.as_ref(),
        "no growth means the same buffer, so no global group is rebuilt"
    );
}

#[test]
fn a_removed_mesh_marks_the_metadata_array_for_upload() {
    let mut h = harness();
    let ctx = h.source.context();
    let mesh = h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert!(
        !h.source.arrays.metadata_dirty,
        "the mesh's own upload cleared it"
    );

    // Removing the mesh clears its entry in the array, so the array
    // reaches the GPU again on the next frame.
    h.source.remove_mesh(mesh);
    assert!(
        h.source.arrays.metadata_dirty,
        "removing a mesh marks the array"
    );
    let encoder = h.encoder();
    h.source
        .arrays
        .upload_metadata(&h.world.root_scope(), ctx, &mut { encoder });
    assert!(
        !h.source.arrays.metadata_dirty,
        "the upload cleared the mark"
    );
}

// -- a whole built scene --------------------------------------------------

/// Releasing the source removes the nodes it registered, so a program that
/// mounts and unmounts sources does not grow the graph forever.
#[test]
fn dropping_the_source_releases_the_nodes_it_registered() {
    let mut h = harness();
    let ctx = h.source.context();
    // A drawn frame, so the pools, the mesh parts and the global bind
    // groups all exist.
    let mesh = h.tri_mesh();
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    let drawn = h.spawn_mesh(mesh.clone());
    let mut encoder = h.encoder();
    let mut scene = Scene::new();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );

    let before = ctx.graph(&h.world.root_scope()).get().len();
    // Read the ids out before the source goes: once it is dropped there is
    // no `h.source` left to name them.
    let metadata_buf = h.source.arrays.metadata_buf.clone();
    let index_pool_id = h.source.pools.index_pool_id.clone();

    // The metadata buffer, the instance and index pools and the global
    // groups are the source's own; the mesh's parts hang off its root.
    // Despawning the drawn entity and giving the local handle up leaves the
    // mesh root unheld, and dropping the source gives up everything it
    // registered itself. The camera and view records are the frame's, kept
    // by the world's registry, so they are no part of what the source
    // registered.
    h.world.root_scope_mut().despawn(drawn).unwrap();
    drop(mesh);
    drop(h.source);
    ctx.graph(&h.world.root_scope()).get().maintain();
    let after = ctx.graph(&h.world.root_scope()).get().len();

    assert!(
        after < before,
        "dropping the source must shrink the graph: {after} was {before}"
    );
    // The two ids this test still holds are strong references of their
    // own, so what they name outlives the source that registered it. That
    // is the point of a counted handle: the resource goes when the last id
    // does, not when its creator does.
    assert!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&metadata_buf)
            .is_some(),
        "a held id keeps the source's metadata buffer alive"
    );
    assert!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&index_pool_id)
            .is_some(),
        "a held id keeps the index pool alive"
    );

    // Giving those ids up is what lets the graph collect them.
    drop(metadata_buf);
    drop(index_pool_id);
    ctx.graph(&h.world.root_scope()).get().maintain();
    assert!(
        ctx.graph(&h.world.root_scope()).get().len() < after,
        "the last ids going is what frees the source's resources"
    );
}
