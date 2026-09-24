//! The top-level renderer — a resource entity component that owns GPU state and
//! drives rendering each frame from the ECS world.
//!
//! The [Renderer] is the bridge between [unlit_ecs] data and the
//! [wgpu_unlit_render] GPU pipeline. Spawn it once (typically as a resource
//! entity) and call [Renderer::render] every frame.

use arrayvec::ArrayVec;
use core::any::TypeId;
use hashbrown::HashMap;
use std::sync::Arc;

use unlit_ecs::{LocalWorld, TypeIdHashMap};
use wgpu_unlit_render::buffer_pool::BufferPool;
use wgpu_unlit_render::globals::{Globals, View};
use wgpu_unlit_render::mesh::{
    MeshInfo, MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use wgpu_unlit_render::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    INSTANCE_SLOT, MESH_INFO_BINDING, MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT,
    UnlitFlags, UnlitOptions, UnlitPipeline, apply_surface,
};
use wgpu_unlit_render::render_attachments::RenderAttachments;
use wgpu_unlit_render::resources::{Resource, ResourceGraph, ResourceId};
use wgpu_unlit_render::scene::{MAX_VERTEX_BUFFERS, Scene};
use wgpu_unlit_render::specialize::{
    Specializable, Specializer, SpecializerKey, SurfaceKey, VertexAttributes,
    VertexBufferLayoutDesc,
};
use wgpu_unlit_render::staging::StagingBuffer;
use zerocopy::IntoBytes;

use crate::bounds::Aabb;
use crate::components::{Camera, GpuMaterial, GpuMesh, RenderLoadOps};
use crate::culling::VisibleMesh;
use crate::mesh::MeshDesc;
use crate::pipeline::{
    DrawKey, FamilyContext, GlobalBinding, GlobalGroupRebuild, PipelineDesc, PipelineFactory,
    PipelineId, PipelineKey, RegisteredGlobal, RenderResources,
};
use crate::scene::{
    AnyFamily, DrawShape, EntryHandles, Family, PipelineHandles, SceneFrame, VisibleEntry,
    assemble_scene, collect_and_sort_visible,
};
use wgpu_unlit_render::vertex_pool::VertexStreamPool;

/// The most resources one mesh can be built from: every vertex buffer a pass
/// can bind, plus the index buffer, the mesh bind group and the mesh-info
/// uniform.
const MAX_MESH_PARTS: usize = MAX_VERTEX_BUFFERS + 3;

/// The renderer: ECS resource component that holds GPU state and orchestrates
/// frame rendering.
///
/// Spawn this as a component on a resource entity in a [LocalWorld]. Call
/// [Renderer::render] each frame to draw every entity that carries a
/// [GpuMesh] and a [GpuPipeline]. A [Transform] places it and an
/// [InstanceColor] tints it; both are optional, defaulting to the identity
/// transform and white. An entity missing a mesh or pipeline is not drawn.
pub struct Renderer {
    /// WGPU device.
    pub device: wgpu::Device,
    /// WGPU queue.
    pub queue: wgpu::Queue,

    /// Dependency-tracked GPU resource graph.
    pub graph: ResourceGraph,

    /// The color attachment the frame renders into, as a texture-view resource
    /// in [`Self::graph`], or `None` until [`Self::set_render_target`] binds
    /// one.
    color_view: Option<ResourceId>,
    /// The depth-stencil attachment the frame renders into, as a texture-view
    /// resource in [`Self::graph`], or `None` until [`Self::set_render_target`]
    /// binds one.
    depth_view: Option<ResourceId>,
    /// The multisample attachment, if any, as a texture-view resource in
    /// [`Self::graph`]; `None` for a non-multisampled pass or before
    /// [`Self::set_render_target`] binds one.
    msaa_view: Option<ResourceId>,
    /// The [SurfaceKey] of the currently bound attachments, cached so the
    /// surface does not have to be re-derived every draw. `None` until
    /// [`Self::set_render_target`] is called.
    surface: Option<SurfaceKey>,

    /// Resource id of the camera uniform buffer.
    camera_buf: ResourceId,
    /// Resource id of the frame-globals uniform buffer.
    globals_buf: ResourceId,
    /// Resource id of the mesh-metadata storage buffer.
    metadata_buf: ResourceId,
    /// Per-frame globals (advanced every call to [Renderer::render]).
    globals: Globals,
    /// Metadata entries, one per live uploaded mesh.
    metadata: Vec<MeshMetadata>,
    /// Metadata slots whose mesh was removed and whose index is free to hand
    /// out again.
    free_metadata: Vec<u32>,
    /// How many metadata entries the current storage buffer can hold.
    metadata_capacity: u32,
    /// Whether the metadata array changed since it was last uploaded.
    ///
    /// Allocating or removing a mesh marks it; the next
    /// [frame](Self::render) uploads the array then, so an upload always lands
    /// in the same encoder as the draws that read it.
    metadata_dirty: bool,

    /// A reused instance-data buffer, grown as needed.
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: u32,

    /// Reused staging buffers, one per buffer uploaded to every frame, so a
    /// steady frame reaches the GPU without a per-frame allocation or
    /// submission.
    camera_staging: StagingBuffer,
    globals_staging: StagingBuffer,
    metadata_staging: StagingBuffer,
    instance_staging: StagingBuffer,

    /// The pool every mesh uploaded through
    /// [`Self::allocate_unlit_mesh`](Self::allocate_unlit_mesh) keeps its
    /// indices in, as one large buffer shared by every indexed mesh.
    ///
    /// A mesh names it through [`GpuMesh::index_buffer`], which is why the
    /// buffer is a node in [`Self::graph`] and has to be replaced there when
    /// the pool grows; see [`Self::sync_pool`].
    index_pool: BufferPool,
    /// The graph node of [`Self::index_pool`]'s buffer.
    index_pool_id: ResourceId,
    /// The pool every mesh uploaded through
    /// [`Self::allocate_unlit_mesh`](Self::allocate_unlit_mesh) keeps its
    /// vertices in: one large buffer per vertex layout, so meshes that share
    /// a layout share a buffer, and one element allocation covers every
    /// stream of a mesh at the same element index.
    vertex_pool: VertexStreamPool,
    /// The graph node of each of [`Self::vertex_pool`]'s buffers, by the
    /// layout the buffer is shaped for.
    vertex_pool_ids: HashMap<VertexBufferLayoutDesc, ResourceId>,

    /// Every concrete pipeline registered with this renderer, in
    /// registration order.
    ///
    /// A pipeline is appended the first time a family resolves a key that
    /// needs it, so an entity's concrete pipeline exists once its family has
    /// resolved the draw's surface and mesh layout.
    pipelines: Vec<RegisteredPipeline>,

    /// Every pipeline family registered with this renderer, keyed by the
    /// [TypeId] of its key type.
    ///
    /// Registration compiles nothing: a family appends to [`Self::pipelines`]
    /// lazily, the first time one of its variant keys is resolved.
    families: TypeIdHashMap<Box<dyn AnyFamily>>,

    // -- cached per-frame allocations ------------------------------------------
    /// Reused Vec of the meshes that passed this frame's frustum culling.
    visible_meshes_cache: Vec<VisibleMesh>,
    /// Reused Vec for visible-entity collection and per-frame sorting.
    visible_cache: Vec<VisibleEntry>,
    /// Reused Vec for packed instance data.
    packed_instances_cache: Vec<MeshInstance>,
    /// Reused Vec for cloned pipeline handles while the scene is built.
    pipeline_handle_cache: Vec<PipelineHandles>,
    /// Reused Vec of per-entry handles while the scene is built.
    entry_handle_cache: Vec<EntryHandles>,
    /// Reused draw list, whose allocation survives between frames.
    scene_cache: Scene,
}

/// The byte size of one index of `format`.
///
/// A pooled index range starts at a multiple of [`wgpu::COPY_BUFFER_ALIGNMENT`],
/// which is a multiple of either format's size, so dividing its byte offset by
/// this yields a whole number of indices.
fn index_format_size(format: wgpu::IndexFormat) -> u32 {
    match format {
        wgpu::IndexFormat::Uint16 => size_of::<u16>() as u32,
        wgpu::IndexFormat::Uint32 => size_of::<u32>() as u32,
    }
}

/// Build the unlit shader's global bind group from the renderer's buffers.
///
/// A free function rather than a method so the rebuild closure the pipeline is
/// registered with can capture it without borrowing the renderer.
fn create_unlit_global_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    resources: &RenderResources,
    needs_metadata: bool,
) -> wgpu::BindGroup {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: CAMERA_BINDING,
            resource: resources.camera.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: FRAME_BINDING,
            resource: resources.globals.as_entire_binding(),
        },
    ];
    if needs_metadata {
        entries.push(wgpu::BindGroupEntry {
            binding: MESH_METADATA_BINDING,
            resource: resources.metadata.as_entire_binding(),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("unlit3d::global"),
        layout,
        entries: &entries,
    })
}

/// Register one concrete pipeline and return its index in `pipelines`.
///
/// A free function rather than a method: the family that resolves the key
/// holds the renderer mutably, so the closure it registers through can only
/// borrow the disjoint fields this needs — the pipeline list, the resource
/// graph and the ids of the renderer's global buffers.
fn register_concrete(
    pipelines: &mut Vec<RegisteredPipeline>,
    graph: &mut ResourceGraph,
    camera_buf: ResourceId,
    globals_buf: ResourceId,
    metadata_buf: ResourceId,
    desc: PipelineDesc,
) -> PipelineId {
    // The material and mesh layouts describe a pipeline's binding interface,
    // but they do not outlive registration: the renderer builds those groups
    // from the family it registered, and wgpu already holds the pipeline's own
    // layout internally.
    let PipelineDesc {
        pipeline, global, ..
    } = desc;

    // A globally bound pipeline reads the renderer's own camera, globals and
    // metadata buffers, so it depends on all three: a rebuild of any of them
    // marks the group dirty. The group is rebuilt through the pipeline's own
    // closure, so a custom pipeline keeps control of what its layout binds.
    let global = global.map(|binding| {
        let GlobalBinding {
            bind_group,
            rebuild,
        } = binding;
        let id = graph
            .insert_strong(
                Resource::BindGroup(bind_group),
                &[camera_buf, globals_buf, metadata_buf],
            )
            .expect("the renderer's buffers exist");
        RegisteredGlobal { id, rebuild }
    });

    pipelines.push(RegisteredPipeline { pipeline, global });
    // The length before the push is the index the pipeline landed on.
    PipelineId::new((pipelines.len() - 1) as u32)
}

/// The per-entity options the built-in unlit family draws with.
///
/// Two entities drawing the same family may start from different options, so
/// the options live on the entity's key rather than on the renderer. The
/// key's type is how the renderer finds the family it draws with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnlitPipelineKey {
    /// The options the entity's variants are specialized from.
    pub options: UnlitOptions,
}

impl UnlitPipelineKey {
    /// A key whose variants start from `options`.
    pub fn new(options: UnlitOptions) -> Self {
        Self { options }
    }
}

impl PipelineKey for UnlitPipelineKey {
    type Pipeline = UnlitPipeline;

    fn base_descriptor(&self) -> UnlitOptions {
        self.options.clone()
    }
}

/// The full specialization key of the built-in unlit family: the entity's
/// options, the frame's target and the mesh's vertex layout.
///
/// The mesh layout is not injective — two meshes whose raw attributes differ
/// but imply the same [UnlitFlags] rewrite the options to the same thing — so
/// the canonical form is the specialized options themselves, and those meshes
/// share one compiled pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UnlitDrawKey {
    options: UnlitOptions,
    surface: SurfaceKey,
    vertex_buffers: ArrayVec<(u32, VertexBufferLayoutDesc), MAX_VERTEX_BUFFERS>,
}

impl From<(UnlitPipelineKey, DrawKey)> for UnlitDrawKey {
    fn from((key, draw): (UnlitPipelineKey, DrawKey)) -> Self {
        Self {
            options: key.options,
            surface: draw.surface,
            vertex_buffers: draw.vertex_buffers,
        }
    }
}

impl SpecializerKey for UnlitDrawKey {
    // The raw mesh layout is not part of the descriptor, so the primary key
    // alone is not injective; the canonical form below is.
    const IS_CANONICAL: bool = false;
    type Canonical = UnlitOptions;
}

/// The flag set a mesh's vertex layout implies, from every slot it declares.
fn unlit_flags_for_layout(layout: &[(u32, VertexBufferLayoutDesc)]) -> UnlitFlags {
    layout
        .iter()
        .fold(UnlitFlags::empty(), |flags, (slot, buffer)| {
            flags | UnlitFlags::for_vertex_buffer(*slot, buffer)
        })
}

/// Specializes the built-in unlit pipeline per entity options, frame target
/// and mesh layout.
///
/// The descriptor starts from the entity's own options, so one family serves
/// entities that differ in material or target policy. The target-dependent
/// fields are rewritten through [apply_surface], the same helper the core
/// crate's own specializer uses, and only the mesh-derived bits of
/// [UnlitFlags::MESH_MASK] are replaced. The canonical key the cache indexes
/// on is the resulting options: two draws whose specialized options agree
/// share one compiled pipeline.
#[derive(Clone, Copy, Debug, Default)]
struct UnlitDrawSpecializer;

impl Specializer<UnlitPipeline> for UnlitDrawSpecializer {
    type Key = UnlitDrawKey;

    fn specialize(&self, key: UnlitDrawKey, options: &mut UnlitOptions) -> UnlitOptions {
        *options = key.options;
        apply_surface(options, key.surface);
        options.flags =
            (options.flags & !UnlitFlags::MESH_MASK) | unlit_flags_for_layout(&key.vertex_buffers);
        options.clone()
    }
}

/// Describes a specialized [UnlitPipeline] the way the renderer registers it.
///
/// This is what keeps the built-in pipeline an ordinary client of the family
/// machinery: it packages the shader's layouts and a closure over the
/// shader's global bindings — the camera, globals and, for variants that read
/// a compressed channel, the metadata buffer.
struct UnlitFactory;

impl PipelineFactory<UnlitPipeline> for UnlitFactory {
    fn descriptor(&self, context: &FamilyContext<'_>, value: &UnlitPipeline) -> PipelineDesc {
        let layout = value.global_layout.clone();
        let needs_metadata = value.options.needs_metadata();
        let device = context.device.clone();

        let rebuild_layout = layout.clone();
        let rebuild: GlobalGroupRebuild = Arc::new(move |resources| {
            create_unlit_global_group(&device, &rebuild_layout, resources, needs_metadata)
        });

        let bind_group = rebuild(context.resources);
        PipelineDesc {
            pipeline: value.pipeline.clone(),
            global: Some(GlobalBinding {
                bind_group,
                rebuild,
            }),
            material_layout: value.material_layout.clone(),
            mesh_layout: value.mesh_layout.clone(),
        }
    }
}

/// A pipeline registered with the renderer.
struct RegisteredPipeline {
    /// The compiled pipeline.
    pipeline: wgpu::RenderPipeline,
    /// The bind group bound at the global index (0), together with the id it
    /// lives under in the resource graph and how to rebuild it. `None` for a
    /// pipeline that binds nothing there.
    global: Option<RegisteredGlobal>,
}

impl Renderer {
    /// Build a new renderer and its GPU resources.
    ///
    /// The renderer starts with no families: register the ones you draw with
    /// through [`Renderer::register_family`] — for the built-in unlit shader,
    /// [`Renderer::register_unlit_family`] does it for you. Registration
    /// compiles nothing; a family's first concrete pipeline is built the first
    /// time an entity that uses it is drawn.
    ///
    /// The renderer starts with no render target: bind one with
    /// [`Self::set_render_target`] before the first [`Self::render`].
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let mut graph = ResourceGraph::new();

        // Camera uniform buffer.
        let camera_buf = graph
            .insert_strong(
                Resource::Buffer(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::camera"),
                    size: size_of::<View>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })),
                &[],
            )
            .expect("camera buffer has no dependencies");

        // Globals uniform buffer.
        let globals = Globals::default();
        let globals_buf = graph
            .insert_strong(
                Resource::Buffer(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::globals"),
                    size: size_of::<Globals>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })),
                &[],
            )
            .expect("globals buffer has no dependencies");

        // Metadata storage buffer (initially 1 entry).
        let metadata_buf = graph
            .insert_strong(
                Resource::Buffer(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::mesh_metadata"),
                    size: size_of::<MeshMetadata>() as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })),
                &[],
            )
            .expect("metadata buffer has no dependencies");

        // Initial upload of camera and globals.
        queue.write_buffer(
            graph.get_buffer(camera_buf).expect("just inserted"),
            0,
            View::new(glam::Mat4::IDENTITY, glam::Vec3::ZERO).as_bytes(),
        );
        queue.write_buffer(
            graph.get_buffer(globals_buf).expect("just inserted"),
            0,
            globals.as_bytes(),
        );

        // The pools the built-in meshes upload into. Each is one buffer in the
        // graph that many meshes read through, so the node is strong — the
        // renderer owns it, no mesh does — and starts out the size of the
        // first mesh's worth of data.
        let index_pool = BufferPool::new(
            &device,
            "unlit3d::mesh::indices",
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            size_of::<u32>() as u64 * 1024,
        );
        let index_pool_id = graph
            .insert_strong(Resource::Buffer(index_pool.buffer().clone()), &[])
            .expect("an index pool has no dependencies");
        let vertex_pool = VertexStreamPool::new(
            &device,
            "unlit3d::mesh::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            1024,
        );

        Self {
            device,
            queue,
            graph,
            color_view: None,
            depth_view: None,
            msaa_view: None,
            surface: None,
            camera_buf,
            globals_buf,
            metadata_buf,
            globals,
            metadata: Vec::new(),
            free_metadata: Vec::new(),
            metadata_capacity: 1,
            metadata_dirty: false,
            instance_buffer: None,
            instance_capacity: 0,
            camera_staging: StagingBuffer::new(),
            globals_staging: StagingBuffer::new(),
            metadata_staging: StagingBuffer::new(),
            instance_staging: StagingBuffer::new(),
            index_pool,
            index_pool_id,
            vertex_pool,
            vertex_pool_ids: HashMap::new(),
            pipelines: Vec::new(),
            families: TypeIdHashMap::default(),
            visible_meshes_cache: Vec::new(),
            visible_cache: Vec::new(),
            packed_instances_cache: Vec::new(),
            pipeline_handle_cache: Vec::new(),
            entry_handle_cache: Vec::new(),
            scene_cache: Scene::new(),
        }
    }

    /// Bind the render target the next frames draw into.
    ///
    /// `color_view`, `depth_view` and `msaa_view` are texture-view resources
    /// already registered in [`Self::graph`]. Any may be `None`: a depth-only
    /// pass omits the color view, a color-only pass omits the depth view, and
    /// a non-multisampled pass omits the MSAA view. At least one of the color
    /// and depth views must be present — a pass with neither cannot exist —
    /// and the MSAA view is only valid alongside a color view it resolves
    /// into.
    ///
    /// The renderer reads the views from the graph each frame, so replacing a
    /// view's texture (and re-binding it here, or relying on the graph's dirty
    /// propagation) is how a swapchain resize reaches the renderer.
    ///
    /// # Panics
    ///
    /// If a given id is not a texture view in the graph, if neither the color
    /// nor the depth view is present, or if an MSAA view is given without a
    /// color view.
    pub fn set_render_target(
        &mut self,
        color_view: Option<ResourceId>,
        depth_view: Option<ResourceId>,
        msaa_view: Option<ResourceId>,
    ) {
        // Resolve every view up front so a bad id panics before any field
        // is touched. The handles are cloned out only to derive the surface
        // key; the ids are what the frame path keeps.
        let color = color_view.map(|id| {
            self.graph
                .get_texture_view(id)
                .expect("color_view is a texture view in the graph")
                .clone()
        });
        let depth = depth_view.map(|id| {
            self.graph
                .get_texture_view(id)
                .expect("depth_view is a texture view in the graph")
                .clone()
        });
        let msaa = msaa_view.map(|id| {
            self.graph
                .get_texture_view(id)
                .expect("msaa_view is a texture view in the graph")
                .clone()
        });
        // The graph records the format each view was created with, which wgpu
        // itself cannot report. A view that reinterprets its texture — an sRGB
        // view over a non-sRGB swap-chain image — is what a pipeline has to
        // match, so it wins over the texture's own format.
        let attachments = RenderAttachments::from_views(color, depth, msaa);
        let attachments = match color_view.and_then(|id| self.graph.get_texture_view_format(id)) {
            Some(format) => attachments.with_color_format(format),
            None => attachments,
        };
        self.surface = Some(attachments.surface_key());
        self.color_view = color_view;
        self.depth_view = depth_view;
        self.msaa_view = msaa_view;
    }

    /// Upload vertex and index data and return a [GpuMesh] handle.
    ///
    /// The renderer assumes no vertex layout: `vertex_buffers` lists exactly
    /// the buffers a draw binds, each tagged with the slot the pipeline's
    /// vertex state declares, so a mesh may carry any combination of
    /// attributes in any format. The pipeline specializes on the layout.
    ///
    /// The mesh's [`Aabb`](crate::Aabb) is recorded in the renderer's
    /// mesh-metadata array, which the next [frame](Renderer::render) uploads.
    ///
    /// [`Renderer::allocate_unlit_mesh`] is the helper that builds the
    /// compressed layout the built-in unlit shader expects.
    ///
    /// # Panics
    ///
    /// If `count` is zero for a non-empty draw, or if the index format does
    /// not match the packed data.
    pub fn allocate_mesh(&mut self, desc: MeshDesc) -> GpuMesh {
        let metadata = MeshMetadata {
            aabb_center: desc.aabb.center,
            aabb_half_extents: desc.aabb.half_extents,
            ..Default::default()
        };
        self.allocate_mesh_with_metadata(desc, metadata)
    }

    /// Upload a mesh together with the full metadata entry it owns.
    ///
    /// The entry is appended to the CPU-side array and reaches the GPU on the
    /// next [frame](Renderer::render); the returned handle names its index.
    fn allocate_mesh_with_metadata(&mut self, desc: MeshDesc, metadata: MeshMetadata) -> GpuMesh {
        let MeshDesc {
            vertex_buffers,
            index_buffer,
            count,
            indexed,
            aabb,
            bind_group,
            mesh_info_buffer,
        } = desc;

        // The parts, capped like the description they come from: a mesh cannot
        // have more vertex buffers than a pass can bind.
        let mut buffers = ArrayVec::<ResourceId, MAX_VERTEX_BUFFERS>::new();
        let mut vertex_slots = ArrayVec::<(u32, ResourceId), MAX_VERTEX_BUFFERS>::new();
        let mut vertex_layout =
            ArrayVec::<(u32, VertexBufferLayoutDesc), MAX_VERTEX_BUFFERS>::new();
        for desc in vertex_buffers {
            // A weak node: the mesh's virtual root is built from it, so the
            // buffer lives exactly as long as the root does.
            let id = self
                .graph
                .insert_weak(Resource::Buffer(desc.buffer), &[])
                .expect("a vertex buffer has no dependencies");
            vertex_slots.push((desc.slot, id));
            // The layout is owned by the mesh so a family can key on it
            // without reading the description again.
            vertex_layout.push((
                desc.slot,
                VertexBufferLayoutDesc {
                    array_stride: desc.array_stride,
                    step_mode: desc.step_mode,
                    attributes: desc.attributes,
                },
            ));
            buffers.push(id);
        }

        let index_buffer = index_buffer.map(|(buffer, format)| {
            // Weak for the same reason as the vertex buffers.
            let id = self
                .graph
                .insert_weak(Resource::Buffer(buffer), &[])
                .expect("an index buffer has no dependencies");
            (id, format)
        });

        // The uniform the mesh's group reads is a weak node that nothing is
        // built from: the group depends on it, not the other way round. It is
        // the mesh's virtual root that keeps it alive, like every other part.
        let mesh_info_id = mesh_info_buffer.map(|buffer| {
            self.graph
                .insert_weak(Resource::Buffer(buffer), &[])
                .expect("a mesh-info buffer has no dependencies")
        });

        let bind_group_id = bind_group.map(|bind_group| {
            // Only the uniform the group reads, so replacing or removing it
            // reaches the group. The mesh's vertex and index buffers are
            // deliberately absent: a draw binds them directly, the group
            // reads none of them, and a pooled buffer changes when the pool
            // grows — a dependency would rebuild every group of every mesh
            // that shares the pool for nothing.
            let mut dependencies = ArrayVec::<ResourceId, MAX_MESH_PARTS>::new();
            dependencies.extend(mesh_info_id);
            self.graph
                .insert_weak(Resource::BindGroup(bind_group), &dependencies)
                .expect("a mesh bind group's dependencies are in the graph")
        });

        // The entry is owned whether or not the pipeline reads it: a draw that
        // binds no metadata group simply leaves the index unused. A slot a
        // removed mesh held is reused, so the array stays as dense as the
        // meshes that are still alive.
        let metadata_index = match self.free_metadata.pop() {
            Some(index) => {
                self.metadata[index as usize] = metadata;
                index
            }
            None => {
                let index = self.metadata.len() as u32;
                self.metadata.push(metadata);
                index
            }
        };
        self.metadata_dirty = true;

        // The uniform names the entry the mesh just took, so it is written
        // once the index above is known.
        if let Some(id) = mesh_info_id {
            self.queue.write_buffer(
                self.graph.get_buffer(id).expect("mesh-info buffer exists"),
                0,
                MeshInfo::new(metadata_index).as_bytes(),
            );
        }

        // Every part of the mesh is registered weak and dependency-free, so
        // the strong virtual root below is the one node that keeps them all
        // alive: removing it orphans them for the cleanup in `remove_mesh`
        // to collect. The root is built from the parts, which is also what
        // makes a replaced part mark it dirty.
        let mut parts = ArrayVec::<ResourceId, MAX_MESH_PARTS>::new();
        parts.extend(buffers.iter().copied());
        parts.extend(index_buffer.map(|(id, _format)| id));
        parts.extend(bind_group_id);
        parts.extend(mesh_info_id);
        let root = self
            .graph
            .insert_strong(Resource::Virtual, &parts)
            .expect("a mesh's parts are in the graph");

        GpuMesh {
            root,
            vertex_buffers: vertex_slots,
            vertex_layout,
            index_buffer,
            count,
            first: 0,
            base_vertex: 0,
            indexed,
            aabb,
            metadata_index,
            bind_group_id,
            vertex_allocation: None,
            index_allocation: None,
        }
    }

    /// Upload raw mesh channels in the layout the built-in unlit shader
    /// expects, and return a [GpuMesh] handle.
    ///
    /// Positions and UVs are compressed to the compact vertex formats the
    /// shader decodes, packed into a position buffer and an interleaved
    /// UV-and-colour buffer, and bound with the mesh-metadata bind group the
    /// shader reads its decode parameters from. Colours are already stored
    /// in the width they are uploaded at, so they are copied through
    /// unchanged.
    ///
    /// This is a convenience over [`Renderer::allocate_mesh`]: it builds the
    /// same [`MeshDesc`] a caller could build by hand, and shares the
    /// compression in [wgpu_unlit_render::mesh] with anyone else who wants
    /// it.
    ///
    /// Which channels are packed is the key's own vertex layout: a slice for a
    /// channel the variant does not declare is left out, so the buffer always
    /// matches what the shader reads.
    ///
    /// # Panics
    ///
    /// If the input slices are empty or of mismatched length (see the
    /// compressors in [wgpu_unlit_render::mesh]), or if the key's options read
    /// no compressed channel and so declare no mesh-metadata group.
    pub fn allocate_unlit_mesh(
        &mut self,
        key: &UnlitPipelineKey,
        positions: &[[f32; 3]],
        uvs: Option<&[[f32; 2]]>,
        colors: Option<&[[u8; 4]]>,
        indices: Option<&[u32]>,
    ) -> GpuMesh {
        // The key's layouts are derived on the fly: the renderer keeps no
        // per-family state, and the pure layout builder is the same one the
        // family's factory uses, so the two cannot drift.
        let options = &key.options;
        let layouts = UnlitPipeline::bind_group_layouts(&self.device, options);
        let mesh_layout = layouts
            .mesh
            .clone()
            .expect("the unlit variant reads mesh metadata");
        // The stream is the key's, not the caller's: the UV-and-color buffer
        // has to be packed the way the shader reading it declares its vertex
        // layout, so a channel the key does not read is left out.
        let uv_color_stream = options.uv_color_stream();

        // Compress vertex streams.
        let mut meta = MeshMetadata::default();
        let packed_positions: Vec<_> = compress_positions(positions, &mut meta).collect();
        let vertex_count = packed_positions.len();

        // UV and colour vertex data, interleaved in the order the shader
        // declares: the channel a slice is given for is the channel packed.
        use wgpu::WriteOnly;
        let uv_color_len = uv_color_stream.byte_len(vertex_count);
        let mut uv_color_data = vec![0u8; uv_color_len];
        if uv_color_len > 0 {
            uv_color_stream.write(
                uvs.unwrap_or(&[]),
                colors.unwrap_or(&[]),
                &mut meta,
                WriteOnly::from_mut(uv_color_data.as_mut_slice()),
            );
        }

        // The mesh-info uniform and the bind group the shader reads it
        // through. The index is unknown until the mesh is allocated below, and
        // the buffer is written then.
        let mesh_info_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::info"),
            size: size_of::<MeshInfo>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mesh_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::mesh::bind_group"),
            layout: &mesh_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: MESH_INFO_BINDING,
                resource: mesh_info_buf.as_entire_binding(),
            }],
        });

        // Index buffer (optional): `Uint16` when every index fits, otherwise
        // `Uint32` — the same choice the compressor makes. The indices go into
        // the index pool, so every indexed mesh draws out of one shared buffer
        // and names its own slice through `GpuMesh::first`.
        let (index_buffer, count, indexed, index_allocation, first_index) = match indices {
            Some(indices) if !indices.is_empty() => {
                let index_count = indices.len() as u32;
                let format = if compress_indices(indices).is_ok() {
                    wgpu::IndexFormat::Uint16
                } else {
                    wgpu::IndexFormat::Uint32
                };
                let data: Vec<u8> = match format {
                    wgpu::IndexFormat::Uint16 => compress_indices(indices)
                        .expect("checked above")
                        .collect::<Vec<u16>>()
                        .as_bytes()
                        .to_vec(),
                    _ => indices.as_bytes().to_vec(),
                };
                let padded_len = data
                    .len()
                    .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize);
                let mut padded = data;
                padded.resize(padded_len, 0);
                let range = self
                    .index_pool
                    .allocate(&self.device, &self.queue, padded_len as u32)
                    .expect("the index pool grows with the mesh");
                Self::sync_pool_node(&self.index_pool, self.index_pool_id, &mut self.graph);
                self.queue.write_buffer(
                    self.graph
                        .get_buffer(self.index_pool_id)
                        .expect("the index pool node exists"),
                    u64::from(range.offset()),
                    &padded,
                );
                let first = range.offset() / index_format_size(format);
                (
                    Some((self.index_pool_id, format)),
                    index_count,
                    true,
                    Some(range.allocation()),
                    first,
                )
            }
            _ => (None, vertex_count as u32, false, None, 0),
        };

        // The vertex layout the key's options declare, slot for slot. An empty
        // stream still gets a buffer entry — the pipeline simply declares no
        // attributes for that slot — so the mesh's layout matches the key's.
        let vertex_layouts = UnlitPipeline::vertex_buffer_layouts(options);
        let layout_of = |slot: u32| -> VertexBufferLayoutDesc {
            vertex_layouts
                .get(slot as usize)
                .and_then(|layout| layout.as_ref())
                .map(VertexBufferLayoutDesc::from_wgpu)
                .unwrap_or(VertexBufferLayoutDesc {
                    array_stride: 0,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: VertexAttributes::new(),
                })
        };
        let position_layout = layout_of(POSITION_SLOT);
        let uv_color_layout = layout_of(UV_COLOR_SLOT);
        let instance_layout = layout_of(INSTANCE_SLOT);

        // The mesh's streams are the vertex pool's: one element allocation
        // covers every stream of the mesh at the same element index, which is
        // what lets a draw address them all with one `firstVertex` or
        // `baseVertex`. A stream the variant declares nothing for — an empty
        // UV-and-colour stream, say — gets no buffer and no part in it.
        let stream_layouts = [position_layout.clone(), uv_color_layout.clone()];
        let vertices = self
            .vertex_pool
            .allocate(
                &self.device,
                &self.queue,
                &stream_layouts,
                vertex_count as u32,
            )
            .expect("the vertex pool grows with the mesh");
        let vertex_offset = vertices.offset();
        for (_slot, (layout, data)) in [
            (
                POSITION_SLOT,
                (&position_layout, packed_positions.as_bytes()),
            ),
            (UV_COLOR_SLOT, (&uv_color_layout, uv_color_data.as_bytes())),
        ] {
            if layout.array_stride == 0 {
                continue;
            }
            let id = self.vertex_node(layout);
            self.sync_vertex_node(id, layout);
            let buffer = self.graph.get_buffer(id).expect("the stream's node exists");
            self.queue.write_buffer(
                buffer,
                VertexStreamPool::byte_offset(layout, vertex_offset),
                data,
            );
        }
        assert!(
            vertex_offset <= i32::MAX as u32,
            "a vertex offset has to fit the i32 a draw's base vertex is"
        );

        // The mesh owns the metadata entry `meta` — the same AABB and UV
        // decode parameters the compression just derived.
        let aabb = Aabb::new(meta.aabb_center, meta.aabb_half_extents);
        let mut mesh = self.allocate_mesh_with_metadata(
            MeshDesc {
                // The streams are the vertex pool's, not the mesh's own: the
                // mesh names the pool's node for each slot and its own range,
                // so no buffer is registered under the mesh for them.
                vertex_buffers: ArrayVec::new(),
                // The indices are the index pool's, not the mesh's own: the
                // mesh names the pool's node and its own range, so no buffer
                // is registered under the mesh for them.
                index_buffer: None,
                count,
                indexed,
                aabb,
                bind_group: Some(mesh_bind_group),
                // The group reads the metadata index through this uniform, so
                // the group depends on it: the uniform is inserted weak, and
                // removing the mesh frees the group and then collects the
                // orphaned uniform.
                mesh_info_buffer: Some(mesh_info_buf),
            },
            meta,
        );

        // The renderer binds the per-instance buffer at [INSTANCE_SLOT] for
        // every draw, so the mesh's layout declares that slot even though the
        // buffer itself is not uploaded here. Without it the draw's key would
        // imply no [UnlitFlags::VERTEX_INSTANCE] and the pipeline would ignore
        // the instance transform.
        mesh.vertex_layout.push((INSTANCE_SLOT, instance_layout));

        // The mesh's slices of the pools it shares: the draw names its ranges
        // by `first` and `base_vertex`, and the allocations are handed back on
        // removal.
        // The layout is the key's own, slot by slot: the family keys on it, so
        // it is owned by the mesh even though the buffers behind it are the
        // pool's. A stream the variant declares nothing for is left out, as it
        // is for a mesh that owns its buffers.
        for (slot, layout) in [
            (POSITION_SLOT, &position_layout),
            (UV_COLOR_SLOT, &uv_color_layout),
        ] {
            if layout.array_stride > 0 {
                mesh.vertex_layout.push((slot, layout.clone()));
                mesh.vertex_buffers.push((slot, self.vertex_node(layout)));
            }
        }
        mesh.index_buffer = index_buffer;
        mesh.first = first_index;
        mesh.base_vertex = vertex_offset;
        mesh.index_allocation = index_allocation;
        mesh.vertex_allocation = Some(vertices.allocation());
        mesh
    }

    /// Insert `texture` into the resource graph and return a pair of ids: the
    /// texture itself and a default view of it.
    ///
    /// The view is recorded as depending on the texture, so replacing the
    /// texture marks every material built from the view dirty. The caller
    /// keeps ownership of the texture only until this call; afterwards the
    /// graph holds it.
    pub fn register_texture_and_default_view(
        &mut self,
        texture: wgpu::Texture,
    ) -> (ResourceId, ResourceId) {
        let texture_id = self
            .graph
            .insert_strong(Resource::Texture(texture), &[])
            .expect("a texture has no dependencies");
        let view = self
            .graph
            .get_texture(texture_id)
            .expect("texture exists")
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_id = self
            .graph
            .insert_strong(view, &[texture_id])
            .expect("the view depends on its texture");
        (texture_id, view_id)
    }

    /// Create a sampler with `descriptor` — or the default one when `None` —
    /// insert it into the resource graph and return its id.
    pub fn register_sampler(
        &mut self,
        descriptor: Option<wgpu::SamplerDescriptor<'_>>,
    ) -> ResourceId {
        let sampler = self.device.create_sampler(&descriptor.unwrap_or_default());
        self.graph
            .insert_strong(Resource::Sampler(sampler), &[])
            .expect("a sampler has no dependencies")
    }

    /// Allocate the unlit material bind group from an existing base-colour
    /// texture view and sampler, and return its [GpuMaterial] handle.
    ///
    /// Only the bind group is built. `view_id` and `sampler_id` name
    /// resources the caller has already put in the graph, and they become the
    /// group's dependencies, so replacing either marks it dirty. Returns
    /// `None` when the key's options read no base-color texture, so the
    /// variant binds no material group to put them in.
    ///
    /// # Panics
    ///
    /// If `view_id` or `sampler_id` is not a texture view or sampler in the
    /// graph.
    pub fn allocate_unlit_material(
        &mut self,
        key: &UnlitPipelineKey,
        view_id: ResourceId,
        sampler_id: ResourceId,
    ) -> Option<GpuMaterial> {
        if !key.options.flags.contains(UnlitFlags::BASE_COLOR_TEXTURE) {
            return None;
        }
        let layout = UnlitPipeline::bind_group_layouts(&self.device, &key.options)
            .material
            .clone()
            .expect("the base-color variant declares a material group");

        // Clone the handles out so the entries borrow nothing from the graph
        // while `allocate_material` mutates it.
        let view = self
            .graph
            .get_texture_view(view_id)
            .expect("the view is in the graph")
            .clone();
        let sampler = self
            .graph
            .get_sampler(sampler_id)
            .expect("the sampler is in the graph")
            .clone();
        let entries = [
            wgpu::BindGroupEntry {
                binding: BASE_COLOR_TEXTURE_BINDING,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: BASE_COLOR_SAMPLER_BINDING,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ];
        Some(self.allocate_material(&layout, &entries, &[view_id, sampler_id]))
    }

    /// Build a material bind group from `entries` against `layout` and return
    /// its [GpuMaterial] handle.
    ///
    /// Only the bind group is created here: the resources it reads are the
    /// caller's, already in the resource graph and named in `dependencies` so
    /// replacing one marks the group dirty. `layout` is the material layout of
    /// the pipeline the material is for —
    /// [`UnlitPipeline::bind_group_layouts`](wgpu_unlit_render::pipeline::UnlitPipeline::bind_group_layouts)
    /// for the built-in shader, or the one a custom pipeline registered.
    ///
    /// # Panics
    ///
    /// If a resource named in `entries` is not in the graph.
    pub fn allocate_material(
        &mut self,
        layout: &wgpu::BindGroupLayout,
        entries: &[wgpu::BindGroupEntry<'_>],
        dependencies: &[ResourceId],
    ) -> GpuMaterial {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::material::bind_group"),
            layout,
            entries,
        });
        let bind_group_id = self
            .graph
            .insert_strong(Resource::BindGroup(bind_group), dependencies)
            .expect("a material bind group depends on graph resources");

        GpuMaterial { bind_group_id }
    }

    /// Free `mesh` and every resource it owns, and drop its mesh-metadata
    /// entry.
    ///
    /// Every resource of the mesh is registered under its virtual root, so
    /// removing that one node and collecting the parts it orphans frees the
    /// whole mesh: its bind group and the per-mesh uniform that only fed that
    /// bind group. A mesh uploaded through
    /// [`allocate_unlit_mesh`](Self::allocate_unlit_mesh) keeps its vertices
    /// and indices in pools the renderer shares between meshes; those
    /// allocations are handed back here, and the pool buffers outlive the mesh.
    /// A mesh uploaded with [`allocate_mesh`](Self::allocate_mesh) owns its
    /// buffers, and they die with it.
    ///
    /// Its metadata slot is freed and reused by a mesh allocated later, so
    /// removing meshes does not grow the array a long-lived renderer uploads.
    /// The next [frame](Renderer::render) uploads the array the shader reads.
    /// Removing a mesh does not shrink the metadata buffer: it grows to the
    /// largest array it has ever held and stays there.
    ///
    /// Removing a mesh while the world still holds its handle is a programming
    /// error the caller has to avoid: nothing detects the stale handle.
    pub fn remove_mesh(&mut self, mesh: GpuMesh) {
        self.graph.remove_drop(mesh.root);
        // The root was the only node built from the parts, so with it gone
        // every part is an orphan.
        self.graph.cleanup_drop();

        if let Some(vertex) = mesh.vertex_allocation {
            self.vertex_pool.release(vertex);
        }
        if let Some(index) = mesh.index_allocation {
            self.index_pool.release(index);
        }

        let emptied = self.metadata.get_mut(mesh.metadata_index as usize);
        if let Some(entry) = emptied {
            *entry = MeshMetadata::default();
            self.free_metadata.push(mesh.metadata_index);
            self.metadata_dirty = true;
        }
    }

    /// Free the bind group `material` names, together with everything built
    /// from it.
    ///
    /// The material's own resources — the texture view and sampler it was
    /// built from — are the caller's and stay in the graph: remove them
    /// separately if nothing else reads them. The [`GpuMaterial`] handle must
    /// not be used afterwards.
    pub fn remove_material(&mut self, material: GpuMaterial) {
        self.graph.remove_drop(material.bind_group_id);
        // A resource that only fed this material's bind group is an orphan now.
        self.graph.cleanup_drop();
    }

    /// Render one frame from the ECS `world`.
    ///
    /// The frame renders into the target bound with [`Self::set_render_target`];
    /// there is no implicit target, so a renderer that has not been bound one
    /// panics here.
    ///
    /// The frame is drawn with the first [Camera] in `world` and opened with
    /// the first [RenderLoadOps] there, or the defaults when none carries
    /// one. A world with no camera draws nothing, but still opens and closes
    /// its pass, so the frame's clears are applied.
    pub fn render(&mut self, world: &LocalWorld) {
        // Find the frame's load ops and its camera, copying the camera out of
        // its cell so the borrow does not block the world accesses below.
        let load_ops = frame_load_ops(world);
        let camera = world.query::<&Camera>().next().map(|(_, c)| Camera {
            clip_from_world: c.clip_from_world,
            position: c.position,
        });

        // The frame records into one encoder, the staged uploads included, so
        // whatever a frame stages reaches the GPU in its own submission —
        // including on the paths below that draw nothing.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("unlit3d::encoder"),
            });

        // A frame that draws still has to publish a changed metadata array, but
        // one that does not draw can leave it for the next frame.
        self.upload_metadata(&mut encoder);

        let Some(camera) = camera else {
            self.clear_pass(&mut encoder, load_ops);
            self.queue.submit([encoder.finish()]);
            return;
        };

        // Update global uniforms.
        self.globals.time += self.globals.delta_time;
        self.globals.frame_count += 1;
        self.upload_globals(&mut encoder);

        let view = View::new(camera.clip_from_world, camera.position);
        self.upload_camera(&mut encoder, &view);

        // Resolve the surface the bound attachments describe, and key every
        // pipeline against it. The surface is cached by `set_render_target`.
        let surface = self
            .surface
            .expect("set_render_target binds the target before rendering");

        // Collect, cull and sort the visible set in one pass. The cache keeps
        // its allocation between frames, so a steady scene allocates nothing.
        self.collect_and_sort_visible(world, &camera, surface);
        if self.visible_cache.is_empty() {
            self.clear_pass(&mut encoder, load_ops);
            self.queue.submit([encoder.finish()]);
            return;
        }
        let instance_count = self.visible_cache.len() as u32;

        // Pack instance data into the reused scratch buffer and upload it.
        self.ensure_instance_buffer(instance_count);
        self.upload_instances(&mut encoder);

        // The per-entry handles are cloned out of the graph into a reused list
        // first, so assembling the draws does not touch the graph and the
        // assembled scene owns everything it names.
        let mut handles = std::mem::take(&mut self.entry_handle_cache);
        handles.clear();
        {
            let graph_ref = &self.graph;
            for entry in &self.visible_cache {
                let mesh = world
                    .get::<GpuMesh>(entry.mesh.entity)
                    .expect("visible entity has GpuMesh");

                let mesh_bg = mesh.bind_group_id.map(|id| {
                    graph_ref
                        .get_bind_group(id)
                        .expect("mesh bind group exists")
                        .clone()
                });

                let material_bg = world.get::<GpuMaterial>(entry.mesh.entity).map(|material| {
                    graph_ref
                        .get_bind_group(material.bind_group_id)
                        .expect("material bind group exists")
                        .clone()
                });

                let mut vertex_buffers = ArrayVec::new();
                for &(slot, buffer) in &mesh.vertex_buffers {
                    let buffer = graph_ref
                        .get_buffer(buffer)
                        .expect("mesh vertex buffer exists")
                        .clone();
                    // A mesh binds its vertex buffers whole; the draw's range
                    // is what picks the mesh's slice out of the pool.
                    vertex_buffers.push((slot, buffer.clone(), 0..buffer.size()));
                }

                let index_buffer = mesh.index_buffer.map(|(buffer, format)| {
                    (
                        graph_ref
                            .get_buffer(buffer)
                            .expect("mesh index buffer exists")
                            .clone(),
                        format,
                    )
                });

                // The mesh is named once here and nowhere else: what a draw
                // reads is carried alongside its handles, so assembling the
                // scene needs no lookup per draw.
                handles.push(EntryHandles {
                    mesh_bg,
                    material_bg,
                    vertex_buffers,
                    index_buffer,
                    shape: DrawShape {
                        indexed: mesh.indexed,
                        count: mesh.count,
                        first: mesh.first,
                        base_vertex: mesh.base_vertex,
                    },
                });
            }
        }

        // Every registered pipeline's handle and global bind group, indexed
        // the same way [`Renderer::pipelines`] is. A pipeline that binds no
        // global group carries `None`. The list is reused between frames, so
        // a steady scene allocates nothing.
        let mut pipeline_handles = std::mem::take(&mut self.pipeline_handle_cache);
        pipeline_handles.clear();
        pipeline_handles.extend(self.pipelines.iter().map(|registered| PipelineHandles {
            pipeline: registered.pipeline.clone(),
            global: registered.global.as_ref().map(|global| {
                self.graph
                    .get_bind_group(global.id)
                    .expect("global group exists")
                    .clone()
            }),
        }));

        let instance_buf = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();

        // Reuse the draw list's allocation across frames.
        let mut scene = std::mem::take(&mut self.scene_cache);
        scene.clear();
        assemble_scene(
            &mut scene,
            &self.visible_cache,
            &pipeline_handles,
            &handles,
            &instance_buf,
        );

        // Record the pass into this frame's encoder: the staged uploads above
        // are already in it, and one submission carries both.
        let attachments = self.attachments();
        {
            let mut pass = attachments.begin_pass(
                &mut encoder,
                load_ops.color,
                load_ops.depth,
                load_ops.stencil,
            );
            scene.record(&mut pass);
        }

        // Keep the draw list's allocation for the next frame.
        self.scene_cache = scene;
        self.pipeline_handle_cache = pipeline_handles;
        self.entry_handle_cache = handles;

        self.queue.submit([encoder.finish()]);
    }
    // -- internal helpers ----------------------------------------------------

    /// Register a pipeline family for the key type `K`.
    ///
    /// This is the only way a pipeline enters the renderer, for the built-in
    /// unlit shader and a caller's own alike. The key type is the family's
    /// identity: entities draw with it when they carry a [GpuPipeline] of that
    /// type, and registering a second family for the same key type is a
    /// programming error.
    ///
    /// A family compiles nothing on registration: its concrete pipelines are
    /// built lazily, the first time a draw resolves a variant key, and are
    /// appended to [`Renderer::pipelines`] in resolution order.
    ///
    /// [`Renderer::register_unlit_family`] is the built-in unlit family; a
    /// pipeline with nothing to specialize on is registered with the
    /// [`TrivialSpecializer`](crate::TrivialSpecializer) and
    /// [`RenderPipelineFactory`](crate::RenderPipelineFactory) helpers.
    ///
    /// # Panics
    ///
    /// If a family is already registered for `K`.
    pub fn register_family<K, T, S, F>(&mut self, specializer: S, factory: F)
    where
        K: PipelineKey<Pipeline = T> + 'static,
        T: Specializable + 'static,
        S: Specializer<T> + 'static,
        S::Key: From<(K, DrawKey)>,
        F: PipelineFactory<T> + 'static,
    {
        let key = TypeId::of::<K>();
        assert!(
            !self.families.contains_key(&key),
            "a pipeline family is already registered for this key type"
        );
        self.families.insert(
            key,
            Box::new(Family::<T, S, F, K>::new(
                &self.device,
                specializer,
                factory,
            )),
        );
    }

    /// Register the built-in unlit shader as a family.
    ///
    /// The family specializes each entity's options on the frame's render
    /// target and the mesh's vertex layout: two draws that agree on both share
    /// one compiled pipeline, and a draw whose mesh layout implies different
    /// channels compiles its own. The options are the entity's: a mesh or
    /// material is built against the [UnlitPipelineKey] it will be drawn with,
    /// through [`Renderer::allocate_unlit_mesh`] and
    /// [`Renderer::allocate_unlit_material`].
    ///
    /// This compiles nothing; like any family, its first concrete pipeline is
    /// built when an entity that uses it is first drawn.
    ///
    /// # Panics
    ///
    /// If the unlit family is already registered.
    pub fn register_unlit_family(&mut self) {
        self.register_family::<UnlitPipelineKey, _, _, _>(UnlitDrawSpecializer, UnlitFactory);
    }

    /// The renderer's global buffers, cloned out of the resource graph.
    fn render_resources(&self) -> RenderResources {
        RenderResources {
            camera: self
                .graph
                .get_buffer(self.camera_buf)
                .expect("camera buf")
                .clone(),
            globals: self
                .graph
                .get_buffer(self.globals_buf)
                .expect("globals buf")
                .clone(),
            metadata: self
                .graph
                .get_buffer(self.metadata_buf)
                .expect("meta buf")
                .clone(),
        }
    }

    /// Rebuild the global bind group of every registered pipeline whose
    /// dependencies changed.
    ///
    /// Each pipeline supplies its own rebuild closure, so a replacement of
    /// the camera, globals or metadata buffer refreshes the built-in unlit
    /// pipeline and any custom one that binds those buffers alike.
    fn rebuild_dirty_global_groups(&mut self) {
        let resources = self.render_resources();
        let rebuilt: Vec<_> = self
            .pipelines
            .iter()
            .filter_map(|registered| {
                let global = registered.global.as_ref()?;
                self.graph
                    .is_dirty(global.id)
                    .then(|| (global.id, Arc::clone(&global.rebuild)))
            })
            .collect();

        for (id, rebuild) in rebuilt {
            let bind_group = rebuild(&resources);
            self.graph
                .replace(id, Resource::BindGroup(bind_group))
                .expect("global group exists");
            self.graph.mark_clean(id);
        }
    }

    /// Upload the camera uniform, staging the bytes through the frame's
    /// encoder.
    fn upload_camera(&mut self, encoder: &mut wgpu::CommandEncoder, view: &View) {
        let buffer = self
            .graph
            .get_buffer(self.camera_buf)
            .expect("camera buffer exists")
            .clone();
        self.camera_staging
            .write(&self.device, encoder, &buffer, 0, view.as_bytes());
    }

    /// Upload the frame-globals uniform, staging the bytes through the frame's
    /// encoder.
    fn upload_globals(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let buffer = self
            .graph
            .get_buffer(self.globals_buf)
            .expect("globals buffer exists")
            .clone();
        self.globals_staging
            .write(&self.device, encoder, &buffer, 0, self.globals.as_bytes());
    }

    /// Pack and upload the frame's instance data, staging the bytes through
    /// the frame's encoder.
    fn upload_instances(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.packed_instances_cache.clear();
        self.packed_instances_cache
            .extend(self.visible_cache.iter().map(|entry| entry.mesh.instance));
        let buffer = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();
        self.instance_staging.write(
            &self.device,
            encoder,
            &buffer,
            0,
            self.packed_instances_cache.as_bytes(),
        );
    }

    /// Grow the metadata buffer if the array outgrew it, and upload the array
    /// through the frame's encoder if it changed.
    ///
    /// Allocating or removing a mesh marks the array dirty, so the upload lands
    /// in the next frame together with the draws that read it. The storage
    /// buffer is recreated only when the array outgrows it, so a steady scene
    /// rewrites in place and rebuilds no bind group.
    fn upload_metadata(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if !self.metadata_dirty {
            return;
        }
        self.metadata_dirty = false;

        let needed = self.metadata.len().max(1) as u32;
        if needed > self.metadata_capacity {
            // Grow geometrically so repeated allocations amortize, and
            // recreate rather than resize: a buffer has a fixed size.
            let capacity = needed.max(self.metadata_capacity * 2);
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::mesh_metadata"),
                size: capacity as u64 * size_of::<MeshMetadata>() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.graph
                .replace(self.metadata_buf, Resource::Buffer(buf))
                .expect("metadata buffer exists");
            self.metadata_capacity = capacity;
            // A replaced buffer invalidates every global group bound to it.
            self.rebuild_dirty_global_groups();
        }
        let buffer = self
            .graph
            .get_buffer(self.metadata_buf)
            .expect("metadata buffer exists")
            .clone();
        self.metadata_staging
            .write(&self.device, encoder, &buffer, 0, self.metadata.as_bytes());
    }

    /// The graph node of the vertex pool's buffer for `layout`, creating it if
    /// the pool has never been asked for the layout before.
    ///
    /// The node is strong: the renderer owns the pool, no mesh does. See
    /// [`Self::sync_pool_node`] for why nothing may depend on it.
    fn vertex_node(&mut self, layout: &VertexBufferLayoutDesc) -> ResourceId {
        if let Some(&id) = self.vertex_pool_ids.get(layout) {
            return id;
        }
        let id = self
            .graph
            .insert_strong(Resource::Virtual, &[])
            .expect("a new stream node has no dependencies");
        self.vertex_pool_ids.insert(layout.clone(), id);
        id
    }

    /// Point the stream node `id` at the vertex pool's buffer for `layout`.
    ///
    /// A stream node starts as a virtual stand-in, so its first sync swaps in
    /// the real buffer; later syncs only happen when the pool grew.
    fn sync_vertex_node(&mut self, id: ResourceId, layout: &VertexBufferLayoutDesc) {
        let buffer = self
            .vertex_pool
            .buffer(layout)
            .expect("the layout has a buffer");
        if self.graph.get_buffer(id) != Some(buffer) {
            self.graph
                .replace(id, Resource::Buffer(buffer.clone()))
                .expect("a stream node exists");
        }
    }

    /// Point the graph node `id` at the buffer `pool` currently hands out.
    ///
    /// A pool grows by creating a new buffer, and a mesh keeps the pool's node
    /// id rather than its buffer, so the node has to follow the buffer. The
    /// check is the pool's own handle compared against what the node holds: no
    /// counter to keep in step, and repeating the call is free.
    ///
    /// Nothing depends on a pool node — a mesh's bind group reads only its
    /// mesh-info uniform — so replacing one marks nothing else dirty. A
    /// dependency added onto a pool buffer makes every grow rebuild it, which
    /// is why there must not be one.
    fn sync_pool_node(pool: &BufferPool, id: ResourceId, graph: &mut ResourceGraph) {
        if graph.get_buffer(id) != Some(pool.buffer()) {
            graph
                .replace(id, Resource::Buffer(pool.buffer().clone()))
                .expect("a pool node exists");
        }
    }

    /// Grow the per-instance buffer geometrically when needed.
    fn ensure_instance_buffer(&mut self, count: u32) {
        if count <= self.instance_capacity {
            return;
        }
        let new_cap = count.max(self.instance_capacity * 2).max(1);
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::instance"),
            size: (new_cap as u64) * size_of::<MeshInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.instance_buffer = Some(buf);
        self.instance_capacity = new_cap;
    }

    /// Collect entities that pass frustum culling, then sort them for drawing.
    ///
    /// The pass itself lives in [crate::scene]; this only unpacks the
    /// renderer's caches and the closure that registers a newly resolved
    /// pipeline, so a family never borrows the whole renderer.
    ///
    /// `surface` is the render target this frame draws into; it is threaded
    /// into every entity's pipeline resolution, so each entry's `pipeline_id`
    /// is a concrete pipeline valid for that target.
    fn collect_and_sort_visible(
        &mut self,
        world: &LocalWorld,
        camera: &Camera,
        surface: SurfaceKey,
    ) {
        let resources = self.render_resources();
        let device = self.device.clone();

        // Split the borrows: the families mutate the pipeline list and the
        // resource graph through the register closure, while the caches are
        // disjoint fields.
        let (families, meshes, visible) = (
            &mut self.families,
            &mut self.visible_meshes_cache,
            &mut self.visible_cache,
        );
        let (pipelines, graph) = (&mut self.pipelines, &mut self.graph);
        let (camera_buf, globals_buf, metadata_buf) =
            (self.camera_buf, self.globals_buf, self.metadata_buf);
        let mut register = |desc| {
            register_concrete(
                pipelines,
                graph,
                camera_buf,
                globals_buf,
                metadata_buf,
                desc,
            )
        };

        collect_and_sort_visible(
            SceneFrame {
                families,
                meshes,
                visible,
                register: &mut register,
            },
            world,
            camera,
            surface,
            &device,
            &resources,
        );
    }

    /// Build the one-frame attachment set from the views bound with
    /// [`Self::set_render_target`].
    ///
    /// The views are cloned out of the graph, so the returned set borrows
    /// nothing from the renderer. No texture is allocated here: the caller
    /// owns every attachment through the graph.
    fn attachments(&self) -> RenderAttachments {
        let color = self.color_view.map(|id| {
            self.graph
                .get_texture_view(id)
                .expect("bound color view")
                .clone()
        });
        let depth = self.depth_view.map(|id| {
            self.graph
                .get_texture_view(id)
                .expect("bound depth view")
                .clone()
        });
        let msaa = self.msaa_view.map(|id| {
            self.graph
                .get_texture_view(id)
                .expect("bound msaa view")
                .clone()
        });
        RenderAttachments::from_views(color, depth, msaa)
    }

    /// Open and close a pass over the attachments without drawing anything,
    /// with `load_ops`, so a frame with nothing to draw still applies the
    /// caller's clears.
    fn clear_pass(&self, encoder: &mut wgpu::CommandEncoder, load_ops: RenderLoadOps) {
        let attachments = self.attachments();
        let mut _pass =
            attachments.begin_pass(encoder, load_ops.color, load_ops.depth, load_ops.stencil);
    }
}

/// The load ops a frame is opened with: the first [RenderLoadOps] in `world`,
/// or the defaults when no entity carries one.
///
/// Load ops are their own component, so they need no particular entity: any
/// one may carry them, and the renderer reads only the first.
fn frame_load_ops(world: &LocalWorld) -> RenderLoadOps {
    world
        .query::<&RenderLoadOps>()
        .next()
        .map_or_else(RenderLoadOps::default, |(_, ops)| *ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Transform, UnlitPipeline, ZSortedDrawing};
    use unlit_ecs::Entity;

    fn test_perspective() -> glam::Mat4 {
        glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0)
    }

    #[test]
    fn mesh_instance_from_transform() {
        let t = Transform {
            translation: glam::Vec3::new(1.0, 2.0, 3.0),
            rotation: glam::Quat::IDENTITY,
            scale: glam::Vec3::ONE,
        };
        let instance = MeshInstance::new(t.compute_matrix(), glam::Vec4::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(instance.model[0].w, 1.0);
        assert_eq!(instance.model[1].w, 2.0);
        assert_eq!(instance.model[2].w, 3.0);
    }

    // -- pipeline registration ---------------------------------------------

    /// The width and height every test renderer is built with.
    const TEST_SIZE: u32 = 64;

    /// A renderer on wgpu's noop backend with the built-in unlit family
    /// registered, plus a standard key to draw with.
    fn noop_renderer() -> (Renderer, UnlitPipelineKey) {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue);
        renderer.register_unlit_family();
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&renderer.device));
        // Bind a default color + depth target so the renderer is ready to draw.
        bind_test_target(&mut renderer, TEST_SIZE, TEST_SIZE);
        (renderer, key)
    }

    /// Register a `width` x `height` color texture and a matching depth texture
    /// in `renderer`'s graph and bind them as its render target.
    ///
    /// Returns the view id of the color texture, so the test can copy the
    /// result back through the graph's texture resource.
    fn bind_test_target(renderer: &mut Renderer, width: u32, height: u32) -> ResourceId {
        use wgpu_unlit_render::render_attachments::create_render_target;
        let ft = create_render_target(
            &renderer.device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            width,
            height,
            1,
        );
        let color_view = renderer.register_texture_and_default_view(ft.color).1;
        let depth_view = renderer
            .graph
            .insert_strong(
                ft.depth
                    .create_view(&wgpu::TextureViewDescriptor::default()),
                &[],
            )
            .expect("depth view has no dependencies");
        renderer.set_render_target(Some(color_view), Some(depth_view), None);
        color_view
    }

    /// Register a 2D texture with `renderer` and return a view id and a
    /// sampler id, ready for [`Renderer::allocate_unlit_material`].
    fn test_material_resources(renderer: &mut Renderer) -> (ResourceId, ResourceId) {
        let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
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
        let view = renderer.register_texture_and_default_view(texture).1;
        let sampler = renderer.register_sampler(None);
        (view, sampler)
    }

    /// A variant of the standard one that reads no UV — so no base-color
    /// texture either — and therefore packs its vertices differently.
    fn uv_less_options(device: &wgpu::Device) -> UnlitOptions {
        use wgpu_unlit_render::pipeline::UnlitFlags;
        let mut options = UnlitOptions::standard(device);
        options.flags &= !(UnlitFlags::VERTEX_UV | UnlitFlags::BASE_COLOR_TEXTURE);
        options
    }

    #[test]
    fn registering_a_family_compiles_nothing() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue);

        renderer.register_unlit_family();

        // Registration records the family; its first concrete pipeline is
        // built only when a draw resolves a variant.
        assert!(renderer.pipelines.is_empty());
        assert_eq!(renderer.families.len(), 1);
    }

    // -- frame load ops ------------------------------------------------------

    #[test]
    fn a_world_without_load_ops_clears_with_the_defaults() {
        let world = LocalWorld::new();

        assert_eq!(frame_load_ops(&world), RenderLoadOps::default());
    }

    #[test]
    fn load_ops_are_read_from_any_entity() {
        let mut world = LocalWorld::new();
        // The entity carries load ops and nothing else: the component needs
        // no camera, no transform and no mesh to take effect.
        world.spawn((RenderLoadOps {
            color: wgpu::LoadOp::Load,
            ..Default::default()
        },));

        assert_eq!(frame_load_ops(&world).color, wgpu::LoadOp::Load);
    }

    #[test]
    fn the_first_load_ops_entity_wins() {
        let mut world = LocalWorld::new();
        let first = world.spawn((RenderLoadOps {
            depth: wgpu::LoadOp::Load,
            ..Default::default()
        },));
        world.spawn((RenderLoadOps {
            depth: wgpu::LoadOp::Clear(1.0),
            ..Default::default()
        },));

        assert_eq!(frame_load_ops(&world).depth, wgpu::LoadOp::Load);
        assert!(world.get::<RenderLoadOps>(first).is_some());
    }

    #[test]
    fn a_renderer_starts_with_no_pipelines() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let renderer = Renderer::new(device, queue);

        // Nothing is privileged: pipelines arrive only through registration.
        assert!(renderer.pipelines.is_empty());
    }

    #[test]
    fn a_unlit_mesh_is_built_from_its_keys_options() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue);
        renderer.register_unlit_family();

        let standard = UnlitPipelineKey::new(UnlitOptions::standard(&renderer.device));
        let uv_less = UnlitPipelineKey::new(uv_less_options(&renderer.device));

        // Each mesh packs the channels its own key reads, not the last
        // registered family's.
        let standard_mesh = tri_mesh(&mut renderer, &standard);
        let uv_less_mesh = tri_mesh(&mut renderer, &uv_less);
        assert!(
            unlit_flags_for_layout(&standard_mesh.vertex_layout).contains(UnlitFlags::VERTEX_UV)
        );
        assert!(
            !unlit_flags_for_layout(&uv_less_mesh.vertex_layout).contains(UnlitFlags::VERTEX_UV)
        );

        // A material is built against the key's layout: the standard variant
        // samples a base-color texture, the UV-less one does not.
        let (view, sampler) = test_material_resources(&mut renderer);
        assert!(
            renderer
                .allocate_unlit_material(&standard, view, sampler)
                .is_some()
        );
        assert!(
            renderer
                .allocate_unlit_material(&uv_less, view, sampler)
                .is_none()
        );
    }

    #[test]
    fn every_registered_pipeline_gets_its_own_global_group() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let a = renderer.surface.expect("target bound");
        // A second target, so the family resolves two variants.
        let b = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            depth_stencil_format: a.depth_stencil_format,
            sample_count: a.sample_count,
        };
        resolve_draw(&mut renderer, &key, a, &mesh);
        resolve_draw(&mut renderer, &key, b, &mesh);
        assert_eq!(renderer.pipelines.len(), 2);

        let ids: Vec<_> = renderer
            .pipelines
            .iter()
            .map(|registered| registered.global.as_ref().expect("has a global group").id)
            .collect();
        for (index, &id) in ids.iter().enumerate() {
            assert!(
                renderer.graph.get_bind_group(id).is_some(),
                "pipeline {index}'s global group is in the graph"
            );
            // No two pipelines share one: each binds its own layout.
            assert!(!ids[..index].contains(&id));
        }
    }

    #[test]
    fn a_variant_is_compiled_when_a_draw_resolves_it() {
        let (mut renderer, key) = noop_renderer();

        // A family has no concrete pipeline until a draw asks for a variant.
        let mesh = tri_mesh(&mut renderer, &key);
        let surface = renderer.surface.expect("target bound");
        resolve_draw(&mut renderer, &key, surface, &mesh);

        assert_eq!(renderer.pipelines.len(), 1);
    }

    #[test]
    fn a_material_is_allocated_from_caller_owned_resources() {
        let (mut renderer, key) = noop_renderer();
        let (view, sampler) = test_material_resources(&mut renderer);

        let material = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        assert!(
            renderer
                .graph
                .get_bind_group(material.bind_group_id)
                .is_some()
        );
    }

    #[test]
    fn a_variant_without_a_base_color_texture_allocates_no_material() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue);
        renderer.register_unlit_family();
        let key = UnlitPipelineKey::new(uv_less_options(&renderer.device));
        let (view, sampler) = test_material_resources(&mut renderer);

        assert!(
            renderer
                .allocate_unlit_material(&key, view, sampler)
                .is_none()
        );
    }

    // -- removal -----------------------------------------------------------

    #[test]
    fn removing_a_mesh_frees_every_resource_built_from_it() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);

        // The root is the mesh's lifetime entry point; its parts are the bind
        // group and the mesh-info uniform that feeds it. The mesh's vertices
        // and indices live in pools, so they are strong nodes the renderer
        // owns and survive the mesh — what the mesh loses is its share of
        // them, which `remove_mesh` hands back.
        let before = renderer.graph.len();
        let index_pool_free = renderer.index_pool.free_space();
        assert!(matches!(
            renderer.graph.get(mesh.root),
            Some(Resource::Virtual)
        ));
        let bind_group = mesh.bind_group_id.expect("the mesh has a group");
        let root = mesh.root;

        renderer.remove_mesh(mesh);

        assert!(renderer.graph.get(root).is_none(), "root removed");
        assert!(
            renderer.graph.get(bind_group).is_none(),
            "the bind group built from them removed"
        );
        assert!(
            renderer.graph.len() < before,
            "the graph shrank: {} -> {}",
            before,
            renderer.graph.len()
        );
        assert!(
            renderer.index_pool.free_space() > index_pool_free,
            "the mesh's index range is free again"
        );
    }

    #[test]
    fn a_mesh_registers_its_pooled_parts_nowhere_under_its_root() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);

        // The pools are the renderer's, not the mesh's: a mesh that goes away
        // must not take the shared buffers with it, so they sit outside the
        // root and the root holds only what is the mesh's own.
        let dependencies: Vec<_> = renderer.graph.dependencies(mesh.root).collect();
        let bind_group = mesh.bind_group_id.expect("the mesh has a group");
        assert!(
            dependencies.contains(&bind_group),
            "the bind group is under the root"
        );
        assert!(
            !dependencies.contains(&renderer.index_pool_id),
            "the index pool is not under the root"
        );
        assert!(
            renderer.graph.get(renderer.index_pool_id).is_some(),
            "the index pool survives"
        );
    }

    #[test]
    fn a_removed_mesh_frees_its_metadata_slot() {
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let second = tri_mesh(&mut renderer, &key);
        let first_index = first.metadata_index;
        assert_ne!(first_index, second.metadata_index);

        renderer.remove_mesh(first);
        let reused = tri_mesh(&mut renderer, &key);

        assert_eq!(
            reused.metadata_index, first_index,
            "the slot the removed mesh held is handed out again"
        );
        assert_ne!(reused.metadata_index, second.metadata_index);
    }

    #[test]
    fn removing_a_material_keeps_the_resources_it_reads() {
        let (mut renderer, key) = noop_renderer();
        let (view, sampler) = test_material_resources(&mut renderer);
        let material = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");

        renderer.remove_material(material.clone());

        assert!(
            renderer.graph.get(material.bind_group_id).is_none(),
            "the bind group is gone"
        );
        // The view and sampler are the caller's, so removing the material
        // that reads them leaves them alone.
        assert!(renderer.graph.get(view).is_some(), "the view stays");
        assert!(renderer.graph.get(sampler).is_some(), "the sampler stays");
    }

    // -- draw ordering ------------------------------------------------------

    /// A camera at `eye` looking at the origin, with a frustum wide enough
    /// that nothing in these tests is culled.
    fn test_camera(eye: glam::Vec3) -> Camera {
        let view = glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y);
        Camera {
            clip_from_world: test_perspective() * view,
            position: eye,
        }
    }

    /// The entities of [`Renderer::visible_cache`] in draw order.
    fn drawn(renderer: &Renderer) -> Vec<Entity> {
        renderer
            .visible_cache
            .iter()
            .map(|entry| entry.mesh.entity)
            .collect()
    }

    /// A triangle mesh allocated on `renderer` for `key`.
    ///
    /// The standard key declares UV and vertex-colour channels, so the three
    /// slices must be present and of equal length.
    fn tri_mesh(renderer: &mut Renderer, key: &UnlitPipelineKey) -> GpuMesh {
        renderer.allocate_unlit_mesh(
            key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        )
    }

    #[test]
    fn indexed_meshes_share_one_index_buffer() {
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let second = tri_mesh(&mut renderer, &key);

        // Both name the pool's node, and each names its own slice of it.
        let pool = renderer.index_pool_id;
        assert_eq!(first.index_buffer.map(|(id, _)| id), Some(pool));
        assert_eq!(second.index_buffer.map(|(id, _)| id), Some(pool));
        assert_ne!(first.first, second.first, "the slices do not overlap");

        // The ranges tile the pool in allocation order.
        let first_range = renderer.index_pool.allocation_size(
            first
                .index_allocation
                .expect("the mesh holds an allocation"),
        );
        assert_eq!(
            first.first + first_range / index_format_size(wgpu::IndexFormat::Uint16),
            second.first,
            "the second mesh starts where the first ends"
        );
    }

    #[test]
    fn a_removed_mesh_frees_its_index_range_for_the_next_mesh() {
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let first_offset = first.first;
        renderer.remove_mesh(first);

        let again = tri_mesh(&mut renderer, &key);
        assert_eq!(
            again.first, first_offset,
            "the freed range is handed out again"
        );
    }

    #[test]
    fn an_index_pool_that_grows_keeps_every_meshes_slice() {
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let first_node = first.index_buffer.expect("the mesh is indexed").0;
        let first_offset = first.first;

        // Enough meshes to push the pool past its starting size.
        let mut last = None;
        for _ in 0..40 {
            last = Some(tri_mesh(&mut renderer, &key));
        }
        let last = last.expect("the loop runs");

        // The node still names the pool, and the graph holds the buffer the
        // pool currently does: a mesh needs no update after a grow.
        assert_eq!(last.index_buffer.map(|(id, _)| id), Some(first_node));
        assert_eq!(
            renderer.graph.get_buffer(first_node),
            Some(renderer.index_pool.buffer()),
            "the node follows the pool's buffer"
        );
        assert_eq!(
            first.first, first_offset,
            "the first mesh's slice did not move"
        );
        assert!(
            renderer.index_pool.size() > first_offset as u64,
            "the pool grew"
        );
    }

    #[test]
    fn a_non_indexed_mesh_holds_no_index_allocation() {
        let (mut renderer, key) = noop_renderer();
        let mesh = renderer.allocate_unlit_mesh(
            &key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            None,
        );

        assert!(!mesh.indexed);
        assert!(mesh.index_buffer.is_none());
        assert!(mesh.index_allocation.is_none());
        assert_eq!(mesh.first, 0);
    }

    #[test]
    fn meshes_sharing_a_layout_share_one_buffer_per_stream() {
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let second = tri_mesh(&mut renderer, &key);

        // Every stream of a mesh names its pool's node, and both meshes name
        // the same node per slot.
        for ((_, first_id), (_, second_id)) in first
            .vertex_buffers
            .iter()
            .zip(second.vertex_buffers.iter())
        {
            assert_eq!(first_id, second_id, "the streams share a buffer");
            assert!(
                renderer.graph.get_buffer(*first_id).is_some(),
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
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let vertex_offset = first.base_vertex;
        renderer.remove_mesh(first);

        let again = tri_mesh(&mut renderer, &key);
        assert_eq!(
            again.base_vertex, vertex_offset,
            "the freed element range is handed out again"
        );
    }

    #[test]
    fn a_vertex_pool_that_grows_keeps_every_meshes_element_index() {
        let (mut renderer, key) = noop_renderer();
        let first = tri_mesh(&mut renderer, &key);
        let nodes: Vec<_> = first.vertex_buffers.iter().map(|(_, id)| *id).collect();
        let base_vertex = first.base_vertex;

        // Enough meshes to push the pool past its starting capacity.
        let mut last = None;
        for _ in 0..80 {
            last = Some(tri_mesh(&mut renderer, &key));
        }
        let last = last.expect("the loop runs");

        // The nodes still name the streams, and each holds the buffer the pool
        // currently does for its layout: a mesh needs no update after a grow.
        // Only the pool-backed slots count: the per-instance slot is bound by
        // the renderer from its own buffer, not the pool's.
        let stream_slots = last
            .vertex_layout
            .iter()
            .filter(|(slot, _)| *slot != INSTANCE_SLOT);
        for (node, (_, layout)) in nodes.iter().zip(stream_slots) {
            let pool_buffer = renderer
                .vertex_pool
                .buffer(layout)
                .expect("the layout has a buffer");
            assert_eq!(
                renderer.graph.get_buffer(*node),
                Some(pool_buffer),
                "the node follows the pool's buffer"
            );
        }
        assert_eq!(
            first.base_vertex, base_vertex,
            "the first mesh's element index did not move"
        );
        assert!(
            renderer.vertex_pool.element_capacity() > base_vertex,
            "the pool grew"
        );
    }

    #[test]
    fn entities_without_a_sort_marker_are_drawn_first() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();

        // The z-sorted entity is the nearer of the two, so depth alone would
        // put it first: it is drawn second because it carries the marker.
        let z_sorted = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            ZSortedDrawing,
        ));
        let opaque = world.spawn((Transform::default(), mesh.clone(), UnlitPipeline::new(key)));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.surface.expect("target bound"));

        assert_eq!(drawn(&renderer), vec![opaque, z_sorted]);
        assert!(renderer.visible_cache[0].depth > renderer.visible_cache[1].depth);
    }

    #[test]
    fn z_sorted_entities_are_drawn_back_to_front() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();

        let near = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            ZSortedDrawing,
        ));
        let middle = world.spawn((
            Transform {
                translation: glam::Vec3::ZERO,
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            ZSortedDrawing,
        ));
        let far = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, -4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            ZSortedDrawing,
        ));
        // Spawned out of order on purpose: registration order is not draw
        // order for z-sorted entities.
        assert_ne!(drawn(&renderer), vec![far, middle, near]);

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.surface.expect("target bound"));

        assert_eq!(drawn(&renderer), vec![far, middle, near]);
    }

    #[test]
    fn entities_are_drawn_in_pipeline_id_order() {
        let (mut renderer, key) = noop_renderer();
        // A second key whose specialized options differ, so it resolves to its
        // own concrete pipeline.
        let uv_less_key = UnlitPipelineKey::new(uv_less_options(&renderer.device));

        // Warm both variants, first key first, so their pipeline ids follow
        // the order they were resolved in.
        let standard_mesh = tri_mesh(&mut renderer, &key);
        let uv_less_mesh = tri_mesh(&mut renderer, &uv_less_key);
        let surface = renderer.surface.expect("target bound");
        let first_id = resolve_draw(&mut renderer, &key, surface, &standard_mesh);
        let second_id = resolve_draw(&mut renderer, &uv_less_key, surface, &uv_less_mesh);
        assert!(first_id < second_id);

        let mut world = LocalWorld::new();
        // Spawn the later-drawn entity first: draw order is the pipeline id,
        // not the spawn order.
        let second = world.spawn((
            Transform::default(),
            uv_less_mesh,
            UnlitPipeline::new(uv_less_key),
        ));
        let first = world.spawn((Transform::default(), standard_mesh, UnlitPipeline::new(key)));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, surface);

        assert_eq!(drawn(&renderer), vec![first, second]);
    }

    #[test]
    fn opaque_draws_sharing_a_material_stay_adjacent() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let (view, sampler) = test_material_resources(&mut renderer);
        let shared = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        let (view, sampler) = test_material_resources(&mut renderer);
        let other = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        assert_ne!(shared.sort_key(), other.sort_key());

        let mut world = LocalWorld::new();
        let a = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            shared.clone(),
        ));
        // The odd one out: sharing no material with the other two, so it is
        // the draw that has to sit on the other side of the pair.
        let _other_material = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            other.clone(),
        ));
        let c = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            shared.clone(),
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.surface.expect("target bound"));

        // The two draws sharing a material are neighbours, so the renderer
        // binds its bind group once for the pair.
        let order = drawn(&renderer);
        assert_eq!(order.len(), 3);
        let (pos_a, pos_c) = (
            order.iter().position(|&e| e == a).expect("a is drawn"),
            order.iter().position(|&e| e == c).expect("c is drawn"),
        );
        assert_eq!(pos_a.abs_diff(pos_c), 1);
    }

    #[test]
    fn sorting_survives_a_second_frame_on_the_reused_cache() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();
        let opaque = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
        ));
        let z_sorted = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key),
            ZSortedDrawing,
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.surface.expect("target bound"));
        assert_eq!(drawn(&renderer), vec![opaque, z_sorted]);

        // The cache is cleared and refilled, not reallocated.
        let capacity = renderer.visible_cache.capacity();
        renderer.collect_and_sort_visible(&world, &camera, renderer.surface.expect("target bound"));
        assert_eq!(drawn(&renderer), vec![opaque, z_sorted]);
        assert_eq!(renderer.visible_cache.capacity(), capacity);
    }

    // -- specialization cache ------------------------------------------------

    /// A raw-layout key for `mesh` on `surface`.
    fn draw_key(surface: SurfaceKey, mesh: &GpuMesh) -> DrawKey {
        DrawKey::for_mesh(surface, mesh)
    }

    /// Resolve one entity carrying `key` on `surface` and return the
    /// concrete pipeline it was drawn with.
    fn resolve_draw(
        renderer: &mut Renderer,
        key: &UnlitPipelineKey,
        surface: SurfaceKey,
        mesh: &GpuMesh,
    ) -> PipelineId {
        let mut world = LocalWorld::new();
        world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
        ));
        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, surface);
        renderer
            .visible_cache
            .last()
            .expect("the entity is visible")
            .pipeline_id
    }

    /// Force a variant for `mesh` on `surface` and return its pipeline id.
    fn resolve(
        renderer: &mut Renderer,
        key: &UnlitPipelineKey,
        surface: SurfaceKey,
        mesh: &GpuMesh,
    ) -> PipelineId {
        resolve_draw(renderer, key, surface, mesh)
    }

    #[test]
    fn a_render_target_change_respecializes_the_family() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);

        let internal = renderer.surface.expect("target bound");
        let first = resolve(&mut renderer, &key, internal, &mesh);
        // The same key twice reuses one compiled pipeline.
        assert_eq!(resolve(&mut renderer, &key, internal, &mesh), first);

        // A different color format is a different surface, so the family
        // compiles a second variant for it.
        let other = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            ..internal
        };
        let second = resolve(&mut renderer, &key, other, &mesh);
        assert_ne!(second, first);
        assert_eq!(renderer.pipelines.len(), 2);
    }

    #[test]
    fn a_mesh_layout_change_respecializes_the_family() {
        let (mut renderer, key) = noop_renderer();
        let surface = renderer.surface.expect("target bound");
        let standard = tri_mesh(&mut renderer, &key);
        let first = resolve(&mut renderer, &key, surface, &standard);

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
        other.vertex_layout = ArrayVec::try_from(layout.as_slice()).expect("the layout still fits");
        assert_ne!(draw_key(surface, &other), draw_key(surface, &standard));
        let second = resolve(&mut renderer, &key, surface, &other);

        assert_ne!(second, first);
        assert_eq!(renderer.pipelines.len(), 2);
    }

    #[test]
    fn meshes_that_share_flags_share_one_variant() {
        let (mut renderer, key) = noop_renderer();
        let surface = renderer.surface.expect("target bound");
        let base = tri_mesh(&mut renderer, &key);
        let first = resolve(&mut renderer, &key, surface, &base);

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
        twin.vertex_layout = ArrayVec::try_from(layout.as_slice()).expect("the layout still fits");
        assert_ne!(draw_key(surface, &twin), draw_key(surface, &base));

        let second = resolve(&mut renderer, &key, surface, &twin);
        assert_eq!(second, first);
        assert_eq!(renderer.pipelines.len(), 1);
    }

    #[test]
    fn an_entity_without_a_pipeline_is_not_drawn() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();
        // No GpuPipeline: the query filters it out.
        let _unpipelined = world.spawn((Transform::default(), mesh.clone()));
        let drawn_entity = world.spawn((Transform::default(), mesh, UnlitPipeline::new(key)));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.surface.expect("target bound"));

        assert_eq!(drawn(&renderer), vec![drawn_entity]);
    }

    #[test]
    fn registering_compiles_no_pipeline() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue);
        renderer.register_unlit_family();

        assert!(renderer.pipelines.is_empty());
        assert_eq!(renderer.families.len(), 1);
    }

    // -- metadata buffer -----------------------------------------------------

    /// A command encoder to record a test's uploads into.
    fn test_encoder(renderer: &Renderer) -> wgpu::CommandEncoder {
        renderer
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("test::encoder"),
            })
    }

    #[test]
    fn the_metadata_buffer_grows_only_when_the_array_outgrows_it() {
        let (mut renderer, key) = noop_renderer();
        // The renderer starts with room for one entry and grows by doubling,
        // so only the entries past the capacity it holds recreate the buffer.
        assert_eq!(renderer.metadata_capacity, 1);

        tri_mesh(&mut renderer, &key);
        renderer.upload_metadata(&mut test_encoder(&renderer));
        assert_eq!(renderer.metadata_capacity, 1, "one entry fills the room");

        tri_mesh(&mut renderer, &key);
        renderer.upload_metadata(&mut test_encoder(&renderer));
        assert_eq!(
            renderer.metadata_capacity, 2,
            "a second entry outgrows room for one"
        );

        // A third entry does not fit in two, so the buffer doubles again.
        tri_mesh(&mut renderer, &key);
        renderer.upload_metadata(&mut test_encoder(&renderer));
        assert_eq!(renderer.metadata_capacity, 4);

        // A fourth entry does fit in four, so the buffer is left alone.
        let before = renderer.graph.get_buffer(renderer.metadata_buf).cloned();
        tri_mesh(&mut renderer, &key);
        renderer.upload_metadata(&mut test_encoder(&renderer));
        assert_eq!(renderer.metadata_capacity, 4, "four entries fit");
        assert_eq!(
            renderer.graph.get_buffer(renderer.metadata_buf),
            before.as_ref(),
            "no growth means the same buffer, so no global group is rebuilt"
        );
    }

    #[test]
    fn a_removed_mesh_marks_the_metadata_array_for_upload() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        renderer.upload_metadata(&mut test_encoder(&renderer));
        assert!(!renderer.metadata_dirty, "the mesh's own upload cleared it");

        // Removing the mesh clears its entry in the array, so the array
        // reaches the GPU again on the next frame.
        renderer.remove_mesh(mesh);
        assert!(renderer.metadata_dirty, "removing a mesh marks the array");
        renderer.upload_metadata(&mut test_encoder(&renderer));
        assert!(!renderer.metadata_dirty, "the upload cleared the mark");
    }

    // -- a whole frame -------------------------------------------------------

    #[test]
    fn a_frame_without_a_camera_only_clears() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();
        // A drawable entity, but nothing the renderer can view it from.
        world.spawn((Transform::default(), mesh, UnlitPipeline::new(key)));

        // The frame takes the no-camera path and still submits a pass, which
        // is what applies the caller's clears.
        renderer.render(&world);
        assert!(renderer.visible_cache.is_empty(), "nothing was drawn");
    }

    #[test]
    fn rendering_a_world_twice_reuses_every_per_frame_cache() {
        let (mut renderer, _key) = noop_renderer();
        // A variant that binds no material group, so the frame needs no
        // material to be a complete draw.
        let key = UnlitPipelineKey::new(uv_less_options(&renderer.device));
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();
        world.spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(key),
            ZSortedDrawing,
        ));

        renderer.render(&world);
        let drawn_first = drawn(&renderer);
        assert_eq!(drawn_first.len(), 1, "the entity was drawn");
        let capacities = (
            renderer.visible_cache.capacity(),
            renderer.entry_handle_cache.capacity(),
            renderer.pipeline_handle_cache.capacity(),
            renderer.scene_cache.draws.capacity(),
        );

        renderer.render(&world);
        assert_eq!(drawn(&renderer), drawn_first, "the same draws, in order");
        assert_eq!(
            (
                renderer.visible_cache.capacity(),
                renderer.entry_handle_cache.capacity(),
                renderer.pipeline_handle_cache.capacity(),
                renderer.scene_cache.draws.capacity(),
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
        let (mut renderer, _key) = noop_renderer();
        let key = UnlitPipelineKey::new(uv_less_options(&renderer.device));
        // Both meshes are indexed and share the pool's index buffer, so what
        // tells their draws apart is the slice each one names.
        let first = tri_mesh(&mut renderer, &key);
        let second = tri_mesh(&mut renderer, &key);
        assert!(first.indexed && second.indexed, "both meshes are indexed");
        assert_ne!(first.first, second.first, "the slices do not overlap");

        let mut world = LocalWorld::new();
        world.spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        world.spawn((Transform::default(), first, UnlitPipeline::new(key.clone())));
        world.spawn((Transform::default(), second, UnlitPipeline::new(key)));

        renderer.render(&world);
        let handles = &renderer.entry_handle_cache;
        assert_eq!(handles.len(), 2, "one handle set per drawn entity");
        assert_eq!(handles[0].shape.first, 0, "the first mesh starts at zero");
        assert_ne!(
            handles[0].shape.first, handles[1].shape.first,
            "each draw keeps its own slice of the pool"
        );
        assert_eq!(drawn(&renderer).len(), 2, "both entities were drawn");
    }
}
