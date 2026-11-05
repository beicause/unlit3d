use super::*;
use crate::components::{GpuRenderPipeline, InstanceVisibility, Transform, ZSortedDrawing};
use crate::lit::{LitMeshDesc, LitPipelineKey};
use crate::mesh::MorphDeltas;
use crate::pass::PassInfo;
use crate::scene::DrawShape;
use crate::source::spawn_context;
use unlit_ecs::bundle;
use unlit_ecs::{Entity, World};
use unlit_wgpu::mesh::{MeshInstance, SecondaryFlags};
use unlit_wgpu::pipeline::{LitOptions, LitVertexChannels};
use unlit_wgpu::render_attachments::{RenderAttachments, create_render_target};
use unlit_wgpu::scene::DrawRange;

/// The size every test target is built with.
const TEST_SIZE: u32 = 64;

fn test_perspective() -> glam::Mat4 {
    glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0)
}

/// A source on wgpu's noop backend, ready to draw into a bound target.
///
/// The source is driven directly — not through a
/// [`Renderer`](crate::Renderer) — so the tests exercise the 3D path
/// itself. The world holds only the frame's context and target; the source
/// stays outside it and reaches the world through its `RenderContext`.
struct Harness {
    world: World,
    source: MeshSource,
    key: LitPipelineKey,
    target: PassInfo,
}

impl Harness {
    /// The triangle mesh every mesh-level test allocates.
    fn tri_mesh(&mut self) -> GpuMesh {
        self.tri_mesh_at(0.0)
    }

    /// The triangle mesh every mesh-level test allocates, with its geometry
    /// shifted along Z so its bounds centre sits away from the entity
    /// origin.
    ///
    /// A mesh whose bounds are offset from its origin is what tells a
    /// depth that reads the bounds apart from one that reads the origin.
    fn tri_mesh_at(&mut self, z: f32) -> GpuMesh {
        self.source.allocate_lit_mesh(
            &self.world.root_scope(),
            &self.key.options,
            LitMeshDesc {
                positions: &[[0.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]],
                uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
        )
    }

    /// Spawn `mesh` drawn by the test pipeline, at the identity transform.
    fn spawn_mesh(&mut self, mesh: GpuMesh) -> Entity {
        self.spawn_mesh_at(mesh, Transform::default())
    }

    /// Spawn `mesh` drawn by the test pipeline, placed at `transform`.
    fn spawn_mesh_at(&mut self, mesh: GpuMesh, transform: Transform) -> Entity {
        self.world.root_scope_mut().spawn(bundle![
            transform,
            mesh,
            GpuRenderPipeline::new(self.key.clone()),
        ])
    }

    /// Spawn `mesh` as a z-sorted draw, placed at `transform`.
    fn spawn_z_sorted(&mut self, mesh: GpuMesh, transform: Transform) -> Entity {
        self.world.root_scope_mut().spawn(bundle![
            transform,
            mesh,
            GpuRenderPipeline::new(self.key.clone()),
            ZSortedDrawing,
        ])
    }

    /// A command encoder to record a test's uploads into.
    fn encoder(&self) -> wgpu::CommandEncoder {
        self.source
            .device(&self.world.root_scope())
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("test::encoder"),
            })
    }

    /// An empty bind group, for a mesh that carries a group of its own.
    fn empty_bind_group(&self) -> wgpu::BindGroup {
        let layout = self
            .source
            .device(&self.world.root_scope())
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("test::mesh::empty"),
                entries: &[],
            });
        self.source
            .device(&self.world.root_scope())
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test::mesh::empty"),
                layout: &layout,
                entries: &[],
            })
    }
}

/// Resolve every entity in `world` on `surface` and return the concrete
/// pipeline the last one was drawn with.
fn resolve_draw(
    source: &mut MeshSource,
    world: &Scope<'_>,
    surface: SurfaceKey,
) -> RenderPipelineId {
    let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    source.collect_and_sort_visible(world, &camera, surface);
    source
        .visible_cache
        .last()
        .expect("the entity is visible")
        .pipeline_id
}

fn harness() -> Harness {
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let mut world = World::new();
    let ctx = spawn_context(
        &mut world,
        device,
        queue,
        ResourceGraph::new(),
        DeviceCapabilities::default(),
    );
    let mut source = MeshSource::new(&world.root_scope(), ctx);
    source.register_lit_family(&world.root_scope());
    let key = LitPipelineKey::new(unlit_options());
    let target = bind_test_target(&world.root_scope(), ctx);
    Harness {
        world,
        source,
        key,
        target,
    }
}

/// Register a color texture and a matching depth texture in the context's
/// graph and describe them as the pass the tests drive the source with.
fn bind_test_target(scope: &Scope<'_>, ctx: RenderContext) -> PassInfo {
    let device = ctx.device(scope).expect("the context's device");
    let ft = create_render_target(
        &device,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        TEST_SIZE,
        TEST_SIZE,
        1,
    );
    let mut resource_graph_row_2 = ctx.graph(scope);

    let graph = resource_graph_row_2.get();
    let color = graph.insert(
        TextureExt::create_view(&ft.color, &wgpu::TextureViewDescriptor::default()),
        None,
    );
    let depth = graph.insert(
        TextureExt::create_view(&ft.depth, &wgpu::TextureViewDescriptor::default()),
        None,
    );
    let attachments =
        RenderAttachments::from_views(graph.get(&color).cloned(), graph.get(&depth).cloned(), None);
    PassInfo {
        surface: attachments.surface_key(),
        width: TEST_SIZE,
        height: TEST_SIZE,
        viewport: None,
    }
}

/// A camera at `eye` looking at the origin, with a frustum wide enough
/// that nothing in these tests is culled.
fn test_camera(eye: glam::Vec3) -> Camera {
    Camera {
        view_from_world: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        clip_from_view: test_perspective(),
        active: true,
    }
}

/// The entities of the source's visible cache in the order the frame
/// resolved them. The pass's composite, not the source, decides the order
/// the draws happen in; use the entries' depths to reason about that.
fn drawn(source: &MeshSource) -> Vec<Entity> {
    source
        .visible_cache
        .iter()
        .map(|entry| entry.mesh.entity)
        .collect()
}

/// The standard policy with lighting off: these tests state no lights.
fn unlit_options() -> LitOptions {
    LitOptions {
        unlit: true,
        ..LitOptions::standard()
    }
}

/// A second policy that differs from the standard one in a field the
/// pipeline is specialized on, so it resolves to its own variant.
fn alternative_options() -> LitOptions {
    LitOptions {
        srgb_to_linear_output: true,
        ..unlit_options()
    }
}

/// A triangle mesh with one morph target, for the tests that need the
/// mesh group a morphing variant binds.
fn morph_mesh(harness: &mut Harness) -> GpuMesh {
    let key = LitPipelineKey::new(unlit_options());
    // One target displacing all three vertices by the same amount, packed
    // vertex-major: three components per vertex.
    let deltas = vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    harness.source.allocate_lit_mesh(
        &harness.world.root_scope(),
        &key.options,
        LitMeshDesc {
            positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            indices: Some(&[0u32, 1, 2]),
            morph_deltas: Some(MorphDeltas {
                deltas,
                target_count: 1,
            }),
            ..Default::default()
        },
    )
}

/// Register a 2D texture with `source` and return a view id and a sampler
/// id, ready for [`MeshSource::allocate_lit_material`](crate::mesh_source::MeshSource::allocate_lit_material).
fn test_material_resources(
    harness: &mut Harness,
) -> (ResHandle<TextureView>, ResHandle<wgpu::Sampler>) {
    let texture = harness
        .source
        .device(&harness.world.root_scope())
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("test::texture"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
    let view = harness
        .source
        .register_texture_and_default_view(&harness.world.root_scope(), texture)
        .1;
    let sampler = harness
        .source
        .register_sampler(&harness.world.root_scope(), None);
    (view, sampler)
}

#[test]
fn a_frame_uses_the_first_active_camera() {
    let mut world = World::new();
    // Spawned first, so a selection that ignored `active` would pick this
    // one instead of the active camera below.
    let inactive = world.root_scope_mut().spawn(bundle![Camera {
        view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-1.0, 0.0, 0.0)),
        clip_from_view: glam::Mat4::IDENTITY,
        active: true,
    },]);
    world
        .root_scope()
        .query_one::<&mut Camera>(inactive)
        .map(|mut camera| {
            let camera = camera.get();
            camera.active = false
        })
        .expect("the entity carries a camera");
    let active = world.root_scope_mut().spawn(bundle![Camera {
        view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-2.0, 0.0, 0.0)),
        clip_from_view: glam::Mat4::IDENTITY,
        active: true,
    },]);

    let (_entity, camera) = frame_camera(&world.root_scope()).expect("a camera is active");
    assert_eq!(camera.position(), glam::Vec3::new(2.0, 0.0, 0.0));
    assert!(camera.active);

    // With every camera inactive, the frame has nothing to draw through.
    world
        .root_scope()
        .query_one::<&mut Camera>(active)
        .map(|mut camera| {
            let camera = camera.get();
            camera.active = false
        })
        .expect("the entity carries a camera");
    assert!(frame_camera(&world.root_scope()).is_none());
}

#[test]
fn each_scope_frames_through_its_own_camera() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Transform::default()]);
    world.scope_mut(scene).unwrap().spawn(bundle![Camera {
        view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-2.0, 0.0, 0.0)),
        clip_from_view: glam::Mat4::IDENTITY,
        active: true,
    },]);
    // A second scene, with no camera of its own.
    let sibling = world.root_scope_mut().spawn(bundle![Transform::default()]);

    let (_entity, camera) =
        frame_camera(&world.scope(scene).unwrap()).expect("the scene has a camera");
    assert_eq!(camera.position(), glam::Vec3::new(2.0, 0.0, 0.0));

    // A sibling's camera is out of reach in both directions, which is what
    // lets two sources frame different parts of one world.
    assert!(
        frame_camera(&world.scope(sibling).unwrap()).is_none(),
        "a scope does not borrow its sibling's camera"
    );
    assert!(
        frame_camera(&world.root_scope()).is_some(),
        "the root sees the scene's camera"
    );
}

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
fn registering_a_family_compiles_nothing() {
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let mut world = World::new();
    let ctx = spawn_context(
        &mut world,
        device,
        queue,
        ResourceGraph::new(),
        DeviceCapabilities::default(),
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
        device,
        queue,
        ResourceGraph::new(),
        DeviceCapabilities::default(),
    );
    let source = MeshSource::new(&world.root_scope(), ctx);

    // Nothing is privileged: pipelines arrive only through registration.
    assert!(source.pipelines.is_empty());
}

#[test]
fn a_unlit_mesh_packs_the_channels_its_desc_carries() {
    let mut h = harness();
    let key = LitPipelineKey::new(unlit_options());

    // The mesh packs the channels the caller passed, not the ones the key
    // used to declare: the options are pure policy now.
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
    let index_pool_free = h.source.index_pool.free_space();
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
        h.source.index_pool.free_space() > index_pool_free,
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
    let pool = h.source.index_pool_id;
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
    assert!(h.source.visible_cache[0].depth > h.source.visible_cache[1].depth);
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

    let entry = &h.source.visible_cache[0];
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

/// A camera at `eye` looking at the origin, with an orthographic
/// projection.
///
/// An orthographic projection has no perspective divide, so a depth read
/// off the clip-space w alone would order nothing.
fn test_orthographic_camera(eye: glam::Vec3) -> Camera {
    Camera {
        view_from_world: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        clip_from_view: glam::camera::rh::proj::directx::orthographic(
            -8.0, 8.0, -8.0, 8.0, 0.1, 100.0,
        ),
        active: true,
    }
}

/// An orthographic camera still sorts z-sorted entries by view-axis depth.
///
/// Under an orthographic projection the perspective divide is gone, so a
/// depth that reads the fourth row alone would report the same value for
/// every entry and leave their order to the sort's stability.
#[test]
fn orthographic_z_sorted_entries_sort_by_view_axis_depth() {
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

    let camera = test_orthographic_camera(glam::Vec3::new(0.0, 0.0, 5.0));
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);

    // No perspective divide to lean on: the depths still come out along
    // the view axis, each off its own mesh's bounds centre, and the far
    // mesh is the deeper draw.
    let depth_of = |entity: Entity| {
        h.source
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
        .visible_cache
        .iter()
        .find(|entry| entry.mesh.entity == warm_standard)
        .expect("the standard entity is visible")
        .pipeline_id;
    let second_id = h
        .source
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
    let capacity = h.source.visible_cache.capacity();
    h.source
        .collect_and_sort_visible(&h.world.root_scope(), &camera, h.target.surface);
    assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);
    assert_eq!(h.source.visible_cache.capacity(), capacity);
}

// -- specialization cache ------------------------------------------------

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

#[test]
fn the_metadata_buffer_grows_only_when_the_array_outgrows_it() {
    let mut h = harness();
    let ctx = h.source.context();
    // The source starts with room for one entry and grows by 1.5x.
    assert_eq!(h.source.metadata_array.capacity(), 1);

    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert_eq!(
        h.source.metadata_array.capacity(),
        1,
        "one entry fills the room"
    );

    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert_eq!(
        h.source.metadata_array.capacity(),
        2,
        "a second entry outgrows room for one"
    );

    // A third and fourth entry outgrow the 1.5x capacity in turn.
    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert_eq!(h.source.metadata_array.capacity(), 3);

    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert_eq!(h.source.metadata_array.capacity(), 4);

    // A fifth entry outgrows four, so the buffer grows again.
    let before = ctx
        .graph(&h.world.root_scope())
        .get()
        .get(&h.source.metadata_buf)
        .cloned();
    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert_eq!(
        h.source.metadata_array.capacity(),
        6,
        "growing past four reaches six (4 * 1.5)"
    );
    assert_ne!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&h.source.metadata_buf),
        before.as_ref(),
        "growth replaced the buffer"
    );

    // A sixth entry fits in six, so the buffer is left alone.
    let before = ctx
        .graph(&h.world.root_scope())
        .get()
        .get(&h.source.metadata_buf)
        .cloned();
    h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert_eq!(h.source.metadata_array.capacity(), 6, "six entries fit");
    assert_eq!(
        ctx.graph(&h.world.root_scope())
            .get()
            .get(&h.source.metadata_buf),
        before.as_ref(),
        "no growth means the same buffer, so no global group is rebuilt"
    );
}

#[test]
fn a_removed_mesh_marks_the_metadata_array_for_upload() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert!(!h.source.metadata_dirty, "the mesh's own upload cleared it");

    // Removing the mesh clears its entry in the array, so the array
    // reaches the GPU again on the next frame.
    h.source.remove_mesh(mesh);
    assert!(h.source.metadata_dirty, "removing a mesh marks the array");
    let encoder = h.encoder();
    h.source
        .upload_metadata(&h.world.root_scope(), &mut { encoder });
    assert!(!h.source.metadata_dirty, "the upload cleared the mark");
}

// -- a whole built scene --------------------------------------------------

#[test]
fn a_frame_without_a_camera_only_clears() {
    let mut h = harness();
    let mesh = h.tri_mesh();
    // A drawable entity, but nothing the source can view it from.
    h.spawn_mesh(mesh);

    // The source builds nothing, so the driver's pass draws nothing while
    // still applying the frame's clears.
    let mut encoder = h.encoder();
    let ctx = h.source.context();
    let mut scene = Scene::new();
    h.source.build_scene(
        &h.world.root_scope(),
        ctx,
        h.target,
        &mut scene,
        &mut encoder,
    );
    assert!(h.source.visible_cache.is_empty(), "nothing was drawn");
    assert!(scene.draws.is_empty(), "the scene is empty");
}

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
        h.source.visible_cache.capacity(),
        h.source.entry_handle_cache.capacity(),
        h.source.pipeline_handle_cache.capacity(),
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
            h.source.visible_cache.capacity(),
            h.source.entry_handle_cache.capacity(),
            h.source.pipeline_handle_cache.capacity(),
            scene.draws.capacity(),
        ),
        capacities,
        "a second frame allocates nothing"
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

    assert_eq!(h.source.visible_cache.len(), 3, "all three are visible");
    assert_eq!(
        h.source.entry_handle_cache.len(),
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
        h.source.visible_cache.len(),
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
    let metadata_buf = h.source.metadata_buf.clone();
    let index_pool_id = h.source.index_pool_id.clone();

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

#[test]
fn grown_capacity_never_shrinks_and_covers_the_need() {
    // Beyond the 1.5x growth, the need itself wins.
    assert_eq!(grown_capacity(16, 30), 30);
    assert_eq!(grown_capacity(16, 40), 40);
    // Otherwise the capacity grows by 1.5x, which is at least `needed`.
    assert_eq!(grown_capacity(16, 17), 24);
    assert_eq!(grown_capacity(16, 24), 24);
    // An empty allocator still ends up with one unit.
    assert_eq!(grown_capacity(0, 0), 1);
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
        .visible_cache
        .iter()
        .map(|entry| entry.mesh.entity)
        .collect();
    let pipelines: Vec<_> = h
        .source
        .visible_cache
        .iter()
        .map(|entry| entry.pipeline_id)
        .collect();
    let depth_of = |entity: Entity| {
        h.source
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
