use super::*;

#[test]
fn entities_without_a_sort_marker_are_drawn_first() {
    let mut h = harness();
    let mesh = h.tri_mesh();

    // The z-sorted entity is the nearer of the two, so depth alone would
    // put it first: it is drawn second because it carries the marker.
    let z_sorted = h.spawn_z_sorted(
        mesh.clone(),
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, 4.0),
            ..Default::default()
        },
    );
    let opaque = h.spawn_mesh(mesh);

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);
    assert!(h.source.caches.visible_cache[0].depth > h.source.caches.visible_cache[1].depth);
}

#[test]
fn z_sorted_entries_carry_depths_that_sort_back_to_front() {
    let mut h = harness();
    let mesh = h.tri_mesh();

    let near = h.spawn_z_sorted(
        mesh.clone(),
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, 4.0),
            ..Default::default()
        },
    );
    let middle = h.spawn_z_sorted(mesh.clone(), Transform::default());
    let far = h.spawn_z_sorted(
        mesh.clone(),
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, -4.0),
            ..Default::default()
        },
    );

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    // The depth each entry carries is what the pass's composite orders
    // its draws by: the farthest entity is the deepest, the nearest the
    // shallowest, so the pool draws far before near.
    let depth_of = |entity: Entity| {
        h.source
            .caches
            .visible_cache
            .iter()
            .find(|entry| entry.mesh.entity == entity)
            .unwrap()
            .depth
    };
    assert!(
        depth_of(far) > depth_of(middle),
        "{far:?} against {middle:?}"
    );
    assert!(
        depth_of(middle) > depth_of(near),
        "{middle:?} against {near:?}"
    );
}

/// The depth an entry sorts by is measured at the geometry the mesh draws,
/// not at the entity's origin: two meshes whose bounds sit on opposite
/// sides of their own origin are equally deep by origin and only their
/// bounds can order them.
#[test]
fn z_sorted_entries_sort_by_the_mesh_bounds_not_the_entity_origin() {
    let mut h = harness();
    let near = h.tri_mesh_at(1.5);
    let near_aabb = near.aabb;
    let far = h.tri_mesh_at(-1.5);
    let far_aabb = far.aabb;

    // Both entity origins are at the same depth; only the geometry differs.
    let translation = glam::Vec3::new(0.0, 0.0, 2.0);
    let near_entity = h.spawn_z_sorted(
        near,
        Transform {
            translation,
            ..Default::default()
        },
    );
    let far_entity = h.spawn_z_sorted(
        far,
        Transform {
            translation,
            ..Default::default()
        },
    );

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    // Each entry's depth reads off its own mesh's bounds centre, not the
    // entity origin both share: the far mesh is the deeper draw even
    // though its entity origin is not deeper than the other's.
    let depth_of = |entity: Entity| {
        h.source
            .caches
            .visible_cache
            .iter()
            .find(|entry| entry.mesh.entity == entity)
            .unwrap()
            .depth
    };
    let near_centre = near_aabb.center + translation;
    let far_centre = far_aabb.center + translation;
    assert!(
        (depth_of(near_entity) - camera.view_depth(near_centre)).abs() < 1e-5,
        "the near mesh's depth is not its bounds centre: {} against {}",
        depth_of(near_entity),
        camera.view_depth(near_centre),
    );
    assert!(
        (depth_of(far_entity) - camera.view_depth(far_centre)).abs() < 1e-5,
        "the far mesh's depth is not its bounds centre: {} against {}",
        depth_of(far_entity),
        camera.view_depth(far_centre),
    );
    assert!(depth_of(far_entity) > depth_of(near_entity));
}

/// The depth is the distance along the camera's view axis, not the
/// euclidean distance to the eye: a mesh offset to the side is at the same
/// depth as one straight ahead.
#[test]
fn z_sorted_depth_is_measured_along_the_view_axis() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    let aabb = mesh.aabb;
    let translation = glam::Vec3::ZERO;
    let entity = h.spawn_z_sorted(
        mesh,
        Transform {
            translation,
            ..Default::default()
        },
    );

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    let entry = &h.source.caches.visible_cache[0];
    assert_eq!(entry.mesh.entity, entity);
    // The geometry centre, not the entity origin.
    let centre = translation + aabb.center;
    assert!(
        (entry.depth - camera.view_depth(centre)).abs() < 1e-5,
        "the depth is not the view-axis depth: {} against {}",
        entry.depth,
        camera.view_depth(centre),
    );
    // And the view-axis depth, not the euclidean distance to the eye.
    let euclidean = (centre - camera.position()).length();
    assert!(
        (entry.depth - euclidean).abs() > 1e-3,
        "the depth is still the euclidean distance: {} against {}",
        entry.depth,
        euclidean,
    );
}

#[test]
fn entities_are_drawn_in_pipeline_id_order() {
    let mut h = harness();
    // A second key whose specialized options differ, so it resolves to its
    // own concrete pipeline.
    let alternative_key = LitPipelineKey::new(alternative_options());

    // Warm both variants, first key first, so their pipeline ids follow
    // the order they were resolved in.
    let standard_mesh = h.tri_mesh();
    let alternative_mesh = h.source.allocate_lit_mesh(
        &h.world.root_scope(),
        &alternative_key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            indices: Some(&[0u32, 1, 2]),
            ..Default::default()
        },
    );
    let surface = h.target.surface;
    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    let warm_standard = h.spawn_mesh(standard_mesh.clone());
    let warm_alternative = h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        alternative_mesh.clone(),
        GpuRenderPipeline::new(alternative_key.clone()),
    ]);
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, surface);
    let first_id = h
        .source
        .caches
        .visible_cache
        .iter()
        .find(|entry| entry.mesh.entity == warm_standard)
        .expect("the standard entity is visible")
        .pipeline_id;
    let second_id = h
        .source
        .caches
        .visible_cache
        .iter()
        .find(|entry| entry.mesh.entity == warm_alternative)
        .expect("the uv-less entity is visible")
        .pipeline_id;
    assert!(first_id < second_id);
    assert!(h.world.root_scope_mut().despawn(warm_standard).is_ok());
    assert!(h.world.root_scope_mut().despawn(warm_alternative).is_ok());

    // Spawn the later-drawn entity first: draw order is the pipeline id,
    // not the spawn order.
    let second = h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        alternative_mesh,
        GpuRenderPipeline::new(alternative_key),
    ]);
    let first = h.spawn_mesh(standard_mesh);

    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, surface);

    assert_eq!(drawn(&h.source), vec![first, second]);
}

#[test]
fn opaque_draws_sharing_a_material_stay_adjacent() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    let (view, sampler) = test_material_resources(&mut h);
    let shared = h.source.allocate_lit_material(
        &h.world.root_scope(),
        &h.key.options,
        view.clone(),
        sampler.clone(),
    );
    let (view, sampler) = test_material_resources(&mut h);
    let other = h.source.allocate_lit_material(
        &h.world.root_scope(),
        &h.key.options,
        view.clone(),
        sampler.clone(),
    );
    assert_ne!(shared.bind_group_id.index(), other.bind_group_id.index());

    let a = h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        mesh.clone(),
        GpuRenderPipeline::new(h.key.clone()),
        shared.clone(),
    ]);
    // The odd one out: sharing no material with the other two, so it is
    // the draw that has to sit on the other side of the pair.
    let _other_material = h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        mesh.clone(),
        GpuRenderPipeline::new(h.key.clone()),
        other.clone(),
    ]);
    let c = h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        mesh,
        GpuRenderPipeline::new(h.key.clone()),
        shared.clone(),
    ]);

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    // The two draws sharing a material are neighbours, so the source
    // binds its bind group once for the pair.
    let order = drawn(&h.source);
    assert_eq!(order.len(), 3);
    let (pos_a, pos_c) = (
        order.iter().position(|&e| e == a).expect("a is drawn"),
        order.iter().position(|&e| e == c).expect("c is drawn"),
    );
    assert_eq!(pos_a.abs_diff(pos_c), 1);
}

#[test]
fn sorting_survives_a_second_frame_on_the_reused_cache() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    let opaque = h.spawn_mesh(mesh.clone());
    let z_sorted = h.spawn_z_sorted(mesh, Transform::default());

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);
    assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);

    // The cache is cleared and refilled, not reallocated.
    let capacity = h.source.caches.visible_cache.capacity();
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);
    assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);
    assert_eq!(h.source.caches.visible_cache.capacity(), capacity);
}

// -- specialization cache ------------------------------------------------

#[test]
fn rendering_a_world_twice_reuses_every_per_frame_cache() {
    let mut h = harness();
    // A variant that binds no material group, so the frame needs no
    // material to be a complete draw.
    let key = LitPipelineKey::new(alternative_options());
    let mesh = h.source.allocate_lit_mesh(
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
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        mesh,
        GpuRenderPipeline::new(key),
        ZSortedDrawing,
    ]);

    let ctx = h.source.context();
    let mut encoder = h.encoder();
    // The driver owns the scene and clears it before every build, so the
    // same one stands in for two frames here.
    let mut scene = Scene::new();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );
    let drawn_first = drawn(&h.source);
    assert_eq!(drawn_first.len(), 1, "the entity was drawn");
    let capacities = (
        h.source.caches.visible_cache.capacity(),
        h.source.caches.entry_handle_cache.capacity(),
        h.source.caches.pipeline_handle_cache.capacity(),
        scene.draws.capacity(),
    );

    scene.draws.clear();
    let mut encoder = h.encoder();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );
    assert_eq!(drawn(&h.source), drawn_first, "the same draws, in order");
    assert_eq!(
        (
            h.source.caches.visible_cache.capacity(),
            h.source.caches.entry_handle_cache.capacity(),
            h.source.caches.pipeline_handle_cache.capacity(),
            scene.draws.capacity(),
        ),
        capacities,
        "a second frame allocates nothing"
    );
}

/// Entities that share a mesh and a material are drawn as one instanced
/// draw, so their pose state rides the instance stream instead of each
/// entity paying for a draw.
#[test]
fn entities_sharing_a_mesh_are_drawn_as_one_instanced_draw() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    for i in 0..3 {
        h.spawn_mesh_at(
            mesh.clone(),
            Transform {
                translation: glam::Vec3::new(i as f32, 0.0, 0.0),
                ..Default::default()
            },
        );
    }

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

    assert_eq!(scene.draws.len(), 1, "one draw for all three");
    let range = &scene.draws[0].range;
    let instances = match range {
        DrawRange::Indexed { instances, .. } | DrawRange::Vertices { instances, .. } => instances,
    };
    assert_eq!(
        instances.clone(),
        0..3,
        "the draw covers every shared instance, in order"
    );
}

/// Many entities sharing one mesh resolve to one handle set, not one per
/// entity: the handles are interned per distinct draw state, which is what
/// keeps a scene of many entities per mesh from cloning the same bind
/// groups and buffers over and over.
#[test]
fn entities_sharing_a_draw_state_share_one_handle_set() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    for i in 0..3 {
        h.spawn_mesh_at(
            mesh.clone(),
            Transform {
                translation: glam::Vec3::new(i as f32, 0.0, 0.0),
                ..Default::default()
            },
        );
    }

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

    assert_eq!(
        h.source.caches.visible_cache.len(),
        3,
        "all three are visible"
    );
    assert_eq!(
        h.source.caches.entry_handle_cache.len(),
        1,
        "one mesh, one draw state, one handle set"
    );
    assert_eq!(
        scene.draws.len(),
        1,
        "and therefore one merged instanced draw"
    );
}

/// A hidden entity keeps its components and its mesh handle but reaches no
/// draw: it is dropped while culling, before a family sees it, so nothing
/// is specialized, packed or bound for it.
#[test]
fn a_hidden_entity_is_not_drawn() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    let shown = h.spawn_mesh(mesh.clone());
    let hidden = h.world.root_scope_mut().spawn(bundle![
        Transform::default(),
        mesh.clone(),
        GpuRenderPipeline::new(h.key.clone()),
        InstanceVisibility { visible: false },
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

    let drawn: Vec<_> = h
        .source
        .caches
        .visible_cache
        .iter()
        .map(|entry| entry.mesh.entity)
        .collect();
    assert_eq!(drawn, [shown], "the hidden entity reached no family");
    assert_ne!(drawn, [hidden]);

    // Flipping the flag back draws it again with the components it already
    // had: no respawn, no new mesh, no reallocated handles.
    *h.world
        .root_scope()
        .query_one::<&mut InstanceVisibility>(hidden)
        .expect("the hidden entity still carries its visibility")
        .get() = InstanceVisibility::default();
    let mut scene = Scene::new();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );
    assert_eq!(
        h.source.caches.visible_cache.len(),
        2,
        "the entity is drawn again once the flag says so"
    );
}

/// Two entities that share a pipeline but draw different meshes cannot
/// share one draw: each keeps its own slice of the vertex pool, so the
/// shapes differ even though the buffers do not.
#[test]
fn entities_drawing_different_meshes_stay_separate_draws() {
    let mut h = harness();
    let first = h.tri_mesh();
    let second = h.tri_mesh();
    assert_ne!(first.first, second.first, "the meshes own different slices");
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    let key = h.key.clone();
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

    assert_eq!(scene.draws.len(), 2, "different meshes stay one draw each");
}

/// A translucent entry never merges, with another translucent entry or with
/// an opaque one: its draw order is its blend order, so the source must
/// not let the instance stream reorder it.
#[test]
fn z_sorted_entries_never_merge() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    h.world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    // Two translucent copies of the same mesh, drawn back to front.
    let key = h.key.clone();
    h.world.root_scope_mut().spawn(bundle![
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, 1.0),
            ..Default::default()
        },
        mesh.clone(),
        GpuRenderPipeline::new(key.clone()),
        ZSortedDrawing,
    ]);
    h.world.root_scope_mut().spawn(bundle![
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, 2.0),
            ..Default::default()
        },
        mesh,
        GpuRenderPipeline::new(key),
        ZSortedDrawing,
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

    assert_eq!(
        scene.draws.len(),
        2,
        "translucent entries keep their own draw order"
    );
    // Each translucent draw joins the pass's depth-sorted pool at the
    // depth its entry resolved to: the first-spawned entity sits nearer
    // the camera at z=1 than the one at z=2, so its depth is smaller.
    let depth_of_draw = |index: usize| match scene.draws[index].order {
        unlit_wgpu::scene::DrawOrder::Sorted(depth) => depth,
        other => panic!("the translucent draw is not depth-sorted: {other:?}"),
    };
    assert!(
        depth_of_draw(0) > depth_of_draw(1),
        "the deeper entity carries the larger depth: {} against {}",
        depth_of_draw(0),
        depth_of_draw(1),
    );
}

/// Two z-sorted entities resolving to DIFFERENT concrete pipelines must still
/// be ordered by view-axis depth, not by pipeline id.
///
/// The discriminating case: the entity that resolves to the LOWER pipeline id
/// is the NEARER one, so a comparator that consulted the pipeline before the
/// depth would draw near-before-far and composite it wrongly.
#[test]
fn z_sorted_entries_of_different_variants_sort_by_depth() {
    let mut h = harness();
    let plain = h.tri_mesh();
    // A different policy specializes to a different variant, hence a
    // different RenderPipelineId.
    let other_key = LitPipelineKey::new(alternative_options());
    let other = h.source.allocate_lit_mesh(
        &h.world.root_scope(),
        &other_key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            indices: Some(&[0u32, 1, 2]),
            ..Default::default()
        },
    );

    let near = h.spawn_z_sorted(
        plain,
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, 4.0),
            ..Default::default()
        },
    );
    let far = h.world.root_scope_mut().spawn(bundle![
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, -4.0),
            ..Default::default()
        },
        other,
        GpuRenderPipeline::new(other_key),
        ZSortedDrawing,
    ]);

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 40.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    let order: Vec<Entity> = h
        .source
        .caches
        .visible_cache
        .iter()
        .map(|entry| entry.mesh.entity)
        .collect();
    let pipelines: Vec<_> = h
        .source
        .caches
        .visible_cache
        .iter()
        .map(|entry| entry.pipeline_id)
        .collect();
    let depth_of = |entity: Entity| {
        h.source
            .caches
            .visible_cache
            .iter()
            .find(|entry| entry.mesh.entity == entity)
            .unwrap()
            .depth
    };

    assert_eq!(order.len(), 2, "both entities are visible");
    assert_ne!(
        pipelines[0], pipelines[1],
        "the two resolve to different pipelines"
    );
    assert!(
        depth_of(far) > depth_of(near),
        "the deeper entity sorts first in the pass's pool, whatever pipeline it resolved to: {} against {}",
        depth_of(far),
        depth_of(near),
    );
}
