use super::*;
use crate::components::{GpuRenderPipeline, InstanceVisibility, Transform, ZSortedDrawing};
use crate::lit::{LitMeshDesc, LitPipelineKey};
use crate::mesh::MorphDeltas;
use crate::pass::PassInfo;
use crate::scene::DrawShape;
use crate::source::spawn_context;
use unlit_ecs::bundle;
use unlit_ecs::{Entity, World};
use unlit_wgpu::context::RenderDevice;
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
        .caches
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
        RenderDevice::unwatched(device, queue),
        ResourceGraph::new(),
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
        camera: None,
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
        .caches
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

mod draws;
mod frame;
mod materials;
mod meshes;
mod pipeline;
