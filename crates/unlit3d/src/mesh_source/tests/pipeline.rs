use super::*;
use unlit_wgpu::context::RenderDevice;

#[test]
fn registering_a_family_compiles_nothing() {
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let mut world = World::new();
    let ctx = spawn_context(
        &mut world,
        RenderDevice::unwatched(device, queue),
        ResourceGraph::new(),
    );
    let mut source = MeshSource::new(&world.root_scope(), ctx);

    source.register_lit_family(&world.root_scope());

    // Registration records the family; its first concrete pipeline is
    // built only when a draw resolves a variant.
    assert!(source.pipelines.is_empty());
    assert_eq!(source.families.len(), 1);
}

#[test]
fn a_source_starts_with_no_pipelines() {
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let mut world = World::new();
    let ctx = spawn_context(
        &mut world,
        RenderDevice::unwatched(device, queue),
        ResourceGraph::new(),
    );
    let source = MeshSource::new(&world.root_scope(), ctx);

    // Nothing is privileged: pipelines arrive only through registration.
    assert!(source.pipelines.is_empty());
}

#[test]
fn a_unlit_mesh_packs_the_channels_its_desc_carries() {
    let mut h = harness();
    let key = LitPipelineKey::new(unlit_options());

    // The mesh packs the channels the caller passed. The options are pure
    // policy, so the key declares no channels for the mesh to follow.
    let textured = h.source.allocate_lit_mesh(
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
    let bare = h.source.allocate_lit_mesh(
        &h.world.root_scope(),
        &key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            indices: Some(&[0u32, 1, 2]),
            ..Default::default()
        },
    );
    let textured_channels = LitVertexChannels::for_vertex_layout(&textured.vertex_layout);
    assert!(textured_channels.secondary.contains(SecondaryFlags::UV));
    assert!(textured_channels.secondary.contains(SecondaryFlags::COLOR));
    let bare_channels = LitVertexChannels::for_vertex_layout(&bare.vertex_layout);
    assert!(!bare_channels.secondary.contains(SecondaryFlags::UV));
    assert!(!bare_channels.secondary.contains(SecondaryFlags::COLOR));

    // A material can be built for either: whether a draw samples it is the
    // draw's own answer — it binds the group or it does not.
    let (view, sampler) = test_material_resources(&mut h);
    let material =
        h.source
            .allocate_lit_material(&h.world.root_scope(), &key.options, view, sampler);
    let ctx = h.source.context();
    assert!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&material.bind_group_id)
            .is_some()
    );
}

/// One pipeline drawn through several cameras needs one group per camera,
/// and one camera drawn by several pipelines needs one per pipeline: each
/// group binds its own layout and the frame's records together.
#[test]
fn a_global_group_is_keyed_by_the_pipeline_and_the_uniforms() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    let a = h.target.surface;
    // A second target, so the family resolves two variants.
    let b = SurfaceKey {
        color_format: wgpu::TextureFormat::Bgra8Unorm,
        ..a
    };
    h.spawn_mesh(mesh.clone());
    resolve_draw(&mut h.source, &h.world.root_scope(), a);
    resolve_draw(&mut h.source, &h.world.root_scope(), b);
    assert_eq!(h.source.pipelines.len(), 2);

    let ctx = h.source.context();
    let device = h.source.device(&h.world.root_scope());
    let mut encoder = h.encoder();
    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    let camera_entity = h
        .world
        .root_scope_mut()
        .spawn(bundle![test_camera(glam::Vec3::new(0.0, 0.0, 5.0))]);
    let root = h.world.root_scope();
    let uniforms = {
        let mut graph_holder = ctx.graph(&root);
        let mut uniforms_holder = ctx.uniforms(&root);
        uniforms_holder.get().uniforms(
            &device,
            graph_holder.get(),
            &mut encoder,
            CameraKey::Entity(camera_entity),
            &CameraUniform::new(
                camera.view_from_world,
                camera.clip_from_view,
                camera.position(),
            ),
            ViewKey::new(TEST_SIZE as f32, TEST_SIZE as f32),
        )
    };

    // Each pipeline builds its own group for the same records, and asking
    // the same pipeline for the same records twice costs a lookup: the
    // second call hands back the group the first one registered.
    let mut ids = Vec::new();
    let loops = h.source.pipelines.len();
    for index in 0..loops {
        let recipe = h.source.pipelines[index]
            .global
            .as_ref()
            .expect("has a recipe")
            .recipe
            .clone();
        let mut graph_holder = ctx.graph(&root);
        let graph = graph_holder.get();
        let groups = &mut h.source.pipelines[index]
            .global
            .as_mut()
            .expect("has a recipe")
            .groups;
        let first = groups.group(graph, uniforms.key(), uniforms.frame, |graph| {
            recipe.insert(&device, graph, &uniforms)
        });
        let again = groups.group(graph, uniforms.key(), uniforms.frame, |graph| {
            recipe.insert(&device, graph, &uniforms)
        });
        assert_eq!(first, again, "a repeated key reuses its group");
        assert!(
            graph.get(&first).is_some(),
            "pipeline {index}'s group is in the graph"
        );
        assert!(
            !ids.contains(&first),
            "no two pipelines share a group: each binds its own layout"
        );
        ids.push(first);
    }
}

#[test]
fn a_variant_is_compiled_when_a_draw_resolves_it() {
    let mut h = harness();

    // A family has no concrete pipeline until a draw asks for a variant.
    let mesh = h.tri_mesh();
    let surface = h.target.surface;
    h.spawn_mesh(mesh);
    resolve_draw(&mut h.source, &h.world.root_scope(), surface);

    assert_eq!(h.source.pipelines.len(), 1);
}

#[test]
fn a_render_target_change_respecializes_the_family() {
    let mut h = harness();
    let mesh = h.tri_mesh();

    let internal = h.target.surface;
    h.spawn_mesh(mesh.clone());
    let first = resolve_draw(&mut h.source, &h.world.root_scope(), internal);
    // The same key twice reuses one compiled pipeline.
    assert_eq!(
        resolve_draw(&mut h.source, &h.world.root_scope(), internal),
        first
    );

    // A different color format is a different surface, so the family
    // compiles a second variant for it.
    let other = SurfaceKey {
        color_format: wgpu::TextureFormat::Bgra8Unorm,
        ..internal
    };
    let second = resolve_draw(&mut h.source, &h.world.root_scope(), other);
    assert_ne!(second, first);
    assert_eq!(h.source.pipelines.len(), 2);
}

#[test]
fn a_mesh_layout_change_respecializes_the_family() {
    let mut h = harness();
    let surface = h.target.surface;
    let standard = h.tri_mesh();
    h.spawn_mesh(standard.clone());
    let first = resolve_draw(&mut h.source, &h.world.root_scope(), surface);

    // A second mesh whose layout declares fewer channels implies a
    // different variant. Dropping only the color attribute keeps the UV
    // the base-color flag needs, so the variant differs without
    // invalidating the base options.
    const COLOR: u32 = 2;
    let mut other = standard.clone();
    let layout: Vec<_> = other
        .vertex_layout
        .iter()
        .map(|(slot, layout)| {
            let mut layout = layout.clone();
            layout
                .attributes
                .retain(|attribute| attribute.shader_location != COLOR);
            (*slot, layout)
        })
        .collect();
    other.vertex_layout = VertexLayout::new(layout);
    assert_ne!(other.vertex_layout, standard.vertex_layout);

    h.spawn_mesh(other.clone());
    let second = resolve_draw(&mut h.source, &h.world.root_scope(), surface);

    assert_ne!(second, first);
    assert_eq!(h.source.pipelines.len(), 2);
}

#[test]
fn meshes_that_share_flags_share_one_variant() {
    let mut h = harness();
    let surface = h.target.surface;
    let base = h.tri_mesh();
    h.spawn_mesh(base.clone());
    let first = resolve_draw(&mut h.source, &h.world.root_scope(), surface);

    // A layout the flags cannot tell from `base`: the attribute is at the
    // same location, so the derived flags are identical even though the
    // raw attribute list differs. The raw keys differ, so a canonical key
    // is what makes both resolve to one compiled pipeline.
    let mut twin = base.clone();
    let layout: Vec<_> = twin
        .vertex_layout
        .iter()
        .map(|(slot, layout)| {
            let mut layout = layout.clone();
            for attribute in &mut layout.attributes {
                attribute.offset += 4;
            }
            layout.array_stride += 4;
            (*slot, layout)
        })
        .collect();
    twin.vertex_layout = VertexLayout::new(layout);
    assert_ne!(twin.vertex_layout, base.vertex_layout);

    h.spawn_mesh(twin.clone());
    let second = resolve_draw(&mut h.source, &h.world.root_scope(), surface);
    assert_eq!(second, first);
    assert_eq!(h.source.pipelines.len(), 1);
}

#[test]
fn an_entity_without_a_pipeline_is_not_drawn() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    // No GpuRenderPipeline: the query filters it out.
    let _unpipelined = h
        .world
        .root_scope_mut()
        .spawn(bundle![Transform::default(), mesh.clone()]);
    let drawn_entity = h.spawn_mesh(mesh);

    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    assert_eq!(drawn(&h.source), vec![drawn_entity]);
}

// -- metadata buffer -----------------------------------------------------
