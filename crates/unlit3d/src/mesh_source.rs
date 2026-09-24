//! The built-in mesh source: the 3D half of a frame.
//!
//! [`MeshSource`] owns everything the mesh path needs: the resource-graph nodes
//! a mesh is uploaded into, the pipeline families an entity's key resolves
//! through, the per-frame caches the draw list is built from, and the
//! [`Scene`] those draws are recorded from. It is an ordinary
//! [`FrameSource`] — the frame driver knows nothing about meshes, so a
//! caller's own source runs beside it with no less privilege.
//!
//! The GPU state it draws with lives in the world, addressed by the
//! [`RenderContext`] it was created with: every setup-time method takes
//! `&LocalWorld` and fetches the device, the queue and the resource graph
//! itself, so the source never holds a borrow of them between calls.

use arrayvec::ArrayVec;
use core::any::TypeId;
use core::ops::DerefMut;
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
use wgpu_unlit_render::resources::{Resource, ResourceGraph, ResourceId};
use wgpu_unlit_render::scene::{MAX_VERTEX_BUFFERS, Scene};
use wgpu_unlit_render::specialize::{
    Specializable, Specializer, SpecializerKey, SurfaceKey, VertexAttributes,
    VertexBufferLayoutDesc,
};
use wgpu_unlit_render::staging::StagingBuffer;
use wgpu_unlit_render::vertex_pool::VertexStreamPool;
use zerocopy::IntoBytes;

use crate::bounds::Aabb;
use crate::components::{Camera, GpuMaterial, GpuMesh};
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
use crate::source::{FrameOrder, FrameSource, RenderContext, frame_target};

/// The most resources one mesh can be built from: every vertex buffer a pass
/// can bind, plus the index buffer, the mesh bind group and the mesh-info
/// uniform.
const MAX_MESH_PARTS: usize = MAX_VERTEX_BUFFERS + 3;

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

/// Build the unlit shader's global bind group from the source's buffers.
///
/// A free function rather than a method so the rebuild closure the pipeline is
/// registered with can capture it without borrowing the source.
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
/// holds the source mutably, so the closure it registers through can only
/// borrow the disjoint fields this needs — the pipeline list, the resource
/// graph and the ids of the source's global buffers.
fn register_concrete(
    pipelines: &mut Vec<RegisteredPipeline>,
    graph: &mut ResourceGraph,
    camera_buf: ResourceId,
    globals_buf: ResourceId,
    metadata_buf: ResourceId,
    desc: PipelineDesc,
) -> PipelineId {
    // The material and mesh layouts describe a pipeline's binding interface,
    // but they do not outlive registration: the source builds those groups
    // from the family it registered, and wgpu already holds the pipeline's own
    // layout internally.
    let PipelineDesc {
        pipeline, global, ..
    } = desc;

    // A globally bound pipeline reads the source's own camera, globals and
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
            .expect("the source's buffers exist");
        RegisteredGlobal { id, rebuild }
    });

    pipelines.push(RegisteredPipeline { pipeline, global });
    // The length before the push is the index the pipeline landed on.
    PipelineId::new((pipelines.len() - 1) as u32)
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

/// The per-entity options the built-in unlit family draws with.
///
/// Two entities drawing the same family may start from different options, so
/// the options live on the entity's key rather than on the source. The key's
/// type is how the source finds the family it draws with.
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

/// Describes a specialized [UnlitPipeline] the way the source registers it.
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

/// A pipeline registered with the source.
struct RegisteredPipeline {
    /// The compiled pipeline.
    pipeline: wgpu::RenderPipeline,
    /// The bind group bound at the global index (0), together with the id it
    /// lives under in the resource graph and how to rebuild it. `None` for a
    /// pipeline that binds nothing there.
    global: Option<RegisteredGlobal>,
}

/// The built-in mesh renderer, as one frame source.
///
/// It owns the mesh pools, the pipeline families, the per-frame caches and the
/// [`Scene`] the frame's draws are assembled into, and it fetches the device,
/// the queue and the resource graph from the world through the
/// [`RenderContext`] it was created with. Register the families it draws with
/// — [`MeshSource::register_unlit_family`] for the built-in unlit shader — and
/// mount it with [`spawn_source`](crate::source::spawn_source).
pub struct MeshSource {
    /// The world addresses of the frame's GPU state, kept so every entry point
    /// fetches the device, the queue and the graph for itself instead of
    /// holding a borrow of them between calls.
    context: RenderContext,

    /// Resource id of the camera uniform buffer.
    camera_buf: ResourceId,
    /// Resource id of the frame-globals uniform buffer.
    globals_buf: ResourceId,
    /// Resource id of the mesh-metadata storage buffer.
    metadata_buf: ResourceId,
    /// Per-frame globals (advanced once per built scene).
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
    /// [build](FrameSource::build_scene) uploads the array then, so an upload
    /// always lands in the same encoder as the draws that read it.
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
    /// [`MeshSource::allocate_unlit_mesh`] keeps its indices in, as one large
    /// buffer shared by every indexed mesh.
    ///
    /// A mesh names it through [`GpuMesh::index_buffer`], which is why the
    /// buffer is a node in the resource graph and has to be replaced there
    /// when the pool grows; see [`MeshSource::sync_pool_node`].
    index_pool: BufferPool,
    /// The graph node of [`MeshSource::index_pool`]'s buffer.
    index_pool_id: ResourceId,
    /// The pool every mesh uploaded through
    /// [`MeshSource::allocate_unlit_mesh`] keeps its vertices in: one large
    /// buffer per vertex layout, so meshes that share a layout share a buffer,
    /// and one element allocation covers every stream of a mesh at the same
    /// element index.
    vertex_pool: VertexStreamPool,
    /// The graph node of each of [`MeshSource::vertex_pool`]'s buffers, by the
    /// layout the buffer is shaped for.
    vertex_pool_ids: HashMap<VertexBufferLayoutDesc, ResourceId>,

    /// Every concrete pipeline registered with this source, in registration
    /// order.
    ///
    /// A pipeline is appended the first time a family resolves a key that
    /// needs it, so an entity's concrete pipeline exists once its family has
    /// resolved the draw's surface and mesh layout.
    pipelines: Vec<RegisteredPipeline>,

    /// Every pipeline family registered with this source, keyed by the
    /// [TypeId] of its key type.
    ///
    /// Registration compiles nothing: a family appends to
    /// [`MeshSource::pipelines`] lazily, the first time one of its variant
    /// keys is resolved.
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
    /// The scene this source built for the frame, whose allocation survives
    /// between frames.
    scene: Scene,
}

impl MeshSource {
    /// Create the source and the GPU resources the mesh path shares.
    ///
    /// The camera and globals uniforms, the mesh-metadata storage buffer and
    /// the index and vertex pools every uploaded mesh draws out of are inserted
    /// into the context's resource graph. The source starts with no families:
    /// register the ones you draw with through
    /// [`MeshSource::register_family`] — for the built-in unlit shader,
    /// [`MeshSource::register_unlit_family`] does it for you. Registration
    /// compiles nothing; a family's first concrete pipeline is built the first
    /// time an entity that uses it is drawn.
    ///
    /// # Panics
    ///
    /// If `ctx` does not name the world's device, queue and resource graph.
    pub fn new(world: &LocalWorld, ctx: RenderContext) -> Self {
        let device = world
            .get::<wgpu::Device>(ctx.device)
            .expect("the context names the world's device")
            .clone();
        let queue = world
            .get::<wgpu::Queue>(ctx.queue)
            .expect("the context names the world's queue")
            .clone();
        let mut graph = world
            .get_mut::<ResourceGraph>(ctx.graph)
            .expect("the context names the world's resource graph");

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
        // source owns it, no mesh does — and starts out the size of the first
        // mesh's worth of data.
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
            context: ctx,
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
            scene: Scene::new(),
        }
    }

    /// The world addresses this source draws through.
    ///
    /// Setup-time code that needs the resource graph reaches it with
    /// `world.get_mut::<ResourceGraph>(source.context().graph)`, which is an
    /// ordinary ECS access: the graph and the source live in different cells.
    pub fn context(&self) -> RenderContext {
        self.context
    }

    /// The device the frame is drawn with, cloned out of `world`.
    ///
    /// # Panics
    ///
    /// If the context's device resource is gone.
    pub fn device(&self, world: &LocalWorld) -> wgpu::Device {
        world
            .get::<wgpu::Device>(self.context.device)
            .expect("the context's device resource exists")
            .clone()
    }

    /// The queue the frame is submitted to, cloned out of `world`.
    ///
    /// # Panics
    ///
    /// If the context's queue resource is gone.
    pub fn queue(&self, world: &LocalWorld) -> wgpu::Queue {
        world
            .get::<wgpu::Queue>(self.context.queue)
            .expect("the context's queue resource exists")
            .clone()
    }

    /// The resource graph `ctx` addresses in `world`.
    ///
    /// An associated function rather than a method so the borrow it takes is
    /// visibly disjoint from the `&mut self` fields a caller may be splitting
    /// alongside it.
    ///
    /// # Panics
    ///
    /// If the context's graph resource is gone.
    pub fn graph<'w>(
        world: &'w LocalWorld,
        ctx: RenderContext,
    ) -> impl DerefMut<Target = ResourceGraph> + 'w {
        world
            .get_mut::<ResourceGraph>(ctx.graph)
            .expect("the context's resource graph exists")
    }

    // -- pipeline registration -------------------------------------------------

    /// Register a pipeline family for the key type `K`.
    ///
    /// This is the only way a pipeline enters the source, for the built-in
    /// unlit shader and a caller's own alike. The key type is the family's
    /// identity: entities draw with it when they carry a
    /// [GpuPipeline](crate::GpuPipeline) of that type, and registering a
    /// second family for the same key type is a programming error.
    ///
    /// A family compiles nothing on registration: its concrete pipelines are
    /// built lazily, the first time a draw resolves a variant key, and are
    /// appended to [`MeshSource::pipelines`] in resolution order.
    ///
    /// [`MeshSource::register_unlit_family`] is the built-in unlit family; a
    /// pipeline with nothing to specialize on is registered with the
    /// [`TrivialSpecializer`](crate::TrivialSpecializer) and
    /// [`RenderPipelineFactory`](crate::RenderPipelineFactory) helpers.
    ///
    /// # Panics
    ///
    /// If a family is already registered for `K`.
    pub fn register_family<K, T, S, F>(&mut self, world: &LocalWorld, specializer: S, factory: F)
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
                &self.device(world),
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
    /// through [`MeshSource::allocate_unlit_mesh`] and
    /// [`MeshSource::allocate_unlit_material`].
    ///
    /// This compiles nothing; like any family, its first concrete pipeline is
    /// built when an entity that uses it is first drawn.
    ///
    /// # Panics
    ///
    /// If the unlit family is already registered.
    pub fn register_unlit_family(&mut self, world: &LocalWorld) {
        self.register_family::<UnlitPipelineKey, _, _, _>(
            world,
            UnlitDrawSpecializer,
            UnlitFactory,
        );
    }

    // -- mesh allocation -------------------------------------------------------

    /// Upload vertex and index data and return a [GpuMesh] handle.
    ///
    /// The source assumes no vertex layout: `vertex_buffers` lists exactly the
    /// buffers a draw binds, each tagged with the slot the pipeline's vertex
    /// state declares, so a mesh may carry any combination of attributes in
    /// any format. The pipeline specializes on the layout.
    ///
    /// The mesh's [`Aabb`](crate::Aabb) is recorded in the source's
    /// mesh-metadata array, which the next built scene uploads.
    ///
    /// [`MeshSource::allocate_unlit_mesh`] is the helper that builds the
    /// compressed layout the built-in unlit shader expects.
    ///
    /// # Panics
    ///
    /// If `count` is zero for a non-empty draw, or if the index format does
    /// not match the packed data.
    pub fn allocate_mesh(&mut self, world: &LocalWorld, desc: MeshDesc) -> GpuMesh {
        let metadata = MeshMetadata {
            aabb_center: desc.aabb.center,
            aabb_half_extents: desc.aabb.half_extents,
            ..Default::default()
        };
        self.allocate_mesh_with_metadata(world, desc, metadata)
    }

    /// Upload a mesh together with the full metadata entry it owns.
    ///
    /// The entry is appended to the CPU-side array and reaches the GPU on the
    /// next built scene; the returned handle names its index.
    fn allocate_mesh_with_metadata(
        &mut self,
        world: &LocalWorld,
        desc: MeshDesc,
        metadata: MeshMetadata,
    ) -> GpuMesh {
        let queue = self.queue(world);

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
            let id = Self::graph(world, self.context)
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
            let id = Self::graph(world, self.context)
                .insert_weak(Resource::Buffer(buffer), &[])
                .expect("an index buffer has no dependencies");
            (id, format)
        });

        // The uniform the mesh's group reads is a weak node that nothing is
        // built from: the group depends on it, not the other way round. It is
        // the mesh's virtual root that keeps it alive, like every other part.
        let mesh_info_id = mesh_info_buffer.map(|buffer| {
            Self::graph(world, self.context)
                .insert_weak(Resource::Buffer(buffer), &[])
                .expect("a mesh-info buffer has no dependencies")
        });

        let bind_group_id = bind_group.map(|bind_group| {
            // Only the uniform the group reads, so replacing or removing it
            // reaches the group. The mesh's vertex and index buffers are
            // deliberately absent: a draw binds them directly, the group reads
            // none of them, and a pooled buffer changes when the pool grows —
            // a dependency would rebuild every group of every mesh that shares
            // the pool for nothing.
            let mut dependencies = ArrayVec::<ResourceId, MAX_MESH_PARTS>::new();
            dependencies.extend(mesh_info_id);
            Self::graph(world, self.context)
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
            queue.write_buffer(
                Self::graph(world, self.context)
                    .get_buffer(id)
                    .expect("mesh-info buffer exists"),
                0,
                MeshInfo::new(metadata_index).as_bytes(),
            );
        }

        // Every part of the mesh is registered weak and dependency-free, so
        // the strong virtual root below is the one node that keeps them all
        // alive: removing it orphans them for the cleanup in `remove_mesh` to
        // collect. The root is built from the parts, which is also what makes
        // a replaced part mark it dirty.
        let mut parts = ArrayVec::<ResourceId, MAX_MESH_PARTS>::new();
        parts.extend(buffers.iter().copied());
        parts.extend(index_buffer.map(|(id, _format)| id));
        parts.extend(bind_group_id);
        parts.extend(mesh_info_id);
        let root = Self::graph(world, self.context)
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
    /// shader reads its decode parameters from. Colours are already stored in
    /// the width they are uploaded at, so they are copied through unchanged.
    ///
    /// This is a convenience over [`MeshSource::allocate_mesh`]: it builds the
    /// same [`MeshDesc`] a caller could build by hand, and shares the
    /// compression in [wgpu_unlit_render::mesh] with anyone else who wants it.
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
        world: &LocalWorld,
        key: &UnlitPipelineKey,
        positions: &[[f32; 3]],
        uvs: Option<&[[f32; 2]]>,
        colors: Option<&[[u8; 4]]>,
        indices: Option<&[u32]>,
    ) -> GpuMesh {
        let device = self.device(world);
        let queue = self.queue(world);

        // The key's layouts are derived on the fly: the source keeps no
        // per-family state, and the pure layout builder is the same one the
        // family's factory uses, so the two cannot drift.
        let options = &key.options;
        let layouts = UnlitPipeline::bind_group_layouts(&device, options);
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
        let mesh_info_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::info"),
            size: size_of::<MeshInfo>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mesh_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                    .allocate(&device, &queue, padded_len as u32)
                    .expect("the index pool grows with the mesh");
                {
                    let mut graph = Self::graph(world, self.context);
                    Self::sync_pool_node(&self.index_pool, self.index_pool_id, &mut graph);
                    queue.write_buffer(
                        graph
                            .get_buffer(self.index_pool_id)
                            .expect("the index pool node exists"),
                        u64::from(range.offset()),
                        &padded,
                    );
                }
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
            .allocate(&device, &queue, &stream_layouts, vertex_count as u32)
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
            let id = self.vertex_node(world, layout);
            self.sync_vertex_node(world, id, layout);
            let buffer = Self::graph(world, self.context)
                .get_buffer(id)
                .expect("the stream's node exists")
                .clone();
            queue.write_buffer(
                &buffer,
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
            world,
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

        // The source binds the per-instance buffer at [INSTANCE_SLOT] for
        // every draw, so the mesh's layout declares that slot even though the
        // buffer itself is not uploaded here. Without it the draw's key would
        // imply no [UnlitFlags::VERTEX_INSTANCE] and the pipeline would ignore
        // the instance transform.
        mesh.vertex_layout.push((INSTANCE_SLOT, instance_layout));

        // The mesh's slices of the pools it shares: the draw names its ranges
        // by `first` and `base_vertex`, and the allocations are handed back on
        // removal. The layout is the key's own, slot by slot: the family keys
        // on it, so it is owned by the mesh even though the buffers behind it
        // are the pool's. A stream the variant declares nothing for is left
        // out, as it is for a mesh that owns its buffers.
        for (slot, layout) in [
            (POSITION_SLOT, &position_layout),
            (UV_COLOR_SLOT, &uv_color_layout),
        ] {
            if layout.array_stride > 0 {
                mesh.vertex_layout.push((slot, layout.clone()));
                mesh.vertex_buffers
                    .push((slot, self.vertex_node(world, layout)));
            }
        }
        mesh.index_buffer = index_buffer;
        mesh.first = first_index;
        mesh.base_vertex = vertex_offset;
        mesh.index_allocation = index_allocation;
        mesh.vertex_allocation = Some(vertices.allocation());
        mesh
    }

    // -- materials -------------------------------------------------------------

    /// Insert `texture` into the resource graph and return a pair of ids: the
    /// texture itself and a default view of it.
    ///
    /// The view is recorded as depending on the texture, so replacing the
    /// texture marks every material built from the view dirty. The caller
    /// keeps ownership of the texture only until this call; afterwards the
    /// graph holds it.
    pub fn register_texture_and_default_view(
        &mut self,
        world: &LocalWorld,
        texture: wgpu::Texture,
    ) -> (ResourceId, ResourceId) {
        let mut graph = Self::graph(world, self.context);
        let texture_id = graph
            .insert_strong(Resource::Texture(texture), &[])
            .expect("a texture has no dependencies");
        let view = graph
            .get_texture(texture_id)
            .expect("texture exists")
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_id = graph
            .insert_strong(view, &[texture_id])
            .expect("the view depends on its texture");
        (texture_id, view_id)
    }

    /// Create a sampler with `descriptor` — or the default one when `None` —
    /// insert it into the resource graph and return its id.
    pub fn register_sampler(
        &mut self,
        world: &LocalWorld,
        descriptor: Option<wgpu::SamplerDescriptor<'_>>,
    ) -> ResourceId {
        let sampler = self
            .device(world)
            .create_sampler(&descriptor.unwrap_or_default());
        Self::graph(world, self.context)
            .insert_strong(Resource::Sampler(sampler), &[])
            .expect("a sampler has no dependencies")
    }

    /// Allocate the unlit material bind group from an existing base-colour
    /// texture view and sampler, and return its [GpuMaterial] handle.
    ///
    /// Only the bind group is built. `view_id` and `sampler_id` name resources
    /// the caller has already put in the graph, and they become the group's
    /// dependencies, so replacing either marks it dirty. Returns `None` when
    /// the key's options read no base-color texture, so the variant binds no
    /// material group to put them in.
    ///
    /// # Panics
    ///
    /// If `view_id` or `sampler_id` is not a texture view or sampler in the
    /// graph.
    pub fn allocate_unlit_material(
        &mut self,
        world: &LocalWorld,
        key: &UnlitPipelineKey,
        view_id: ResourceId,
        sampler_id: ResourceId,
    ) -> Option<GpuMaterial> {
        if !key.options.flags.contains(UnlitFlags::BASE_COLOR_TEXTURE) {
            return None;
        }
        let layout = UnlitPipeline::bind_group_layouts(&self.device(world), &key.options)
            .material
            .clone()
            .expect("the base-color variant declares a material group");

        // Clone the handles out so the entries borrow nothing from the graph
        // while `allocate_material` mutates it.
        let view = Self::graph(world, self.context)
            .get_texture_view(view_id)
            .expect("the view is in the graph")
            .clone();
        let sampler = Self::graph(world, self.context)
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
        Some(self.allocate_material(world, &layout, &entries, &[view_id, sampler_id]))
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
        world: &LocalWorld,
        layout: &wgpu::BindGroupLayout,
        entries: &[wgpu::BindGroupEntry<'_>],
        dependencies: &[ResourceId],
    ) -> GpuMaterial {
        let bind_group = self
            .device(world)
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("unlit3d::material::bind_group"),
                layout,
                entries,
            });
        let bind_group_id = Self::graph(world, self.context)
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
    /// [`allocate_unlit_mesh`](MeshSource::allocate_unlit_mesh) keeps its
    /// vertices and indices in pools the source shares between meshes; those
    /// allocations are handed back here, and the pool buffers outlive the mesh.
    /// A mesh uploaded with [`allocate_mesh`](MeshSource::allocate_mesh) owns
    /// its buffers, and they die with it.
    ///
    /// Its metadata slot is freed and reused by a mesh allocated later, so
    /// removing meshes does not grow the array a long-lived source uploads.
    /// The next built scene uploads the array the shader reads. Removing a mesh
    /// does not shrink the metadata buffer: it grows to the largest array it
    /// has ever held and stays there.
    ///
    /// Removing a mesh while the world still holds its handle is a programming
    /// error the caller has to avoid: nothing detects the stale handle.
    pub fn remove_mesh(&mut self, world: &LocalWorld, mesh: GpuMesh) {
        {
            let graph = Self::graph(world, self.context);
            let mut graph = graph;
            graph.remove_drop(mesh.root);
            // The root was the only node built from the parts, so with it gone
            // every part is an orphan.
            graph.cleanup_drop();
        }

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
    pub fn remove_material(&mut self, world: &LocalWorld, material: GpuMaterial) {
        let mut graph = Self::graph(world, self.context);
        graph.remove_drop(material.bind_group_id);
        // A resource that only fed this material's bind group is an orphan now.
        graph.cleanup_drop();
    }

    // -- internal helpers ------------------------------------------------------

    /// The source's global buffers, cloned out of the resource graph.
    fn render_resources(&self, world: &LocalWorld) -> RenderResources {
        let graph = Self::graph(world, self.context);
        RenderResources {
            camera: graph
                .get_buffer(self.camera_buf)
                .expect("camera buf")
                .clone(),
            globals: graph
                .get_buffer(self.globals_buf)
                .expect("globals buf")
                .clone(),
            metadata: graph
                .get_buffer(self.metadata_buf)
                .expect("meta buf")
                .clone(),
        }
    }

    /// Rebuild the global bind group of every registered pipeline whose
    /// dependencies changed.
    ///
    /// Each pipeline supplies its own rebuild closure, so a replacement of the
    /// camera, globals or metadata buffer refreshes the built-in unlit
    /// pipeline and any custom one that binds those buffers alike.
    fn rebuild_dirty_global_groups(&mut self, world: &LocalWorld) {
        let resources = self.render_resources(world);
        let rebuilt: Vec<_> = self
            .pipelines
            .iter()
            .filter_map(|registered| {
                let global = registered.global.as_ref()?;
                Self::graph(world, self.context)
                    .is_dirty(global.id)
                    .then(|| (global.id, Arc::clone(&global.rebuild)))
            })
            .collect();

        for (id, rebuild) in rebuilt {
            let bind_group = rebuild(&resources);
            let mut graph = Self::graph(world, self.context);
            graph
                .replace(id, Resource::BindGroup(bind_group))
                .expect("global group exists");
            graph.mark_clean(id);
        }
    }

    /// Upload the camera uniform, staging the bytes through the frame's
    /// encoder.
    fn upload_camera(
        &mut self,
        world: &LocalWorld,
        encoder: &mut wgpu::CommandEncoder,
        view: &View,
    ) {
        let buffer = Self::graph(world, self.context)
            .get_buffer(self.camera_buf)
            .expect("camera buffer exists")
            .clone();
        self.camera_staging
            .write(&self.device(world), encoder, &buffer, 0, view.as_bytes());
    }

    /// Upload the frame-globals uniform, staging the bytes through the frame's
    /// encoder.
    fn upload_globals(&mut self, world: &LocalWorld, encoder: &mut wgpu::CommandEncoder) {
        let buffer = Self::graph(world, self.context)
            .get_buffer(self.globals_buf)
            .expect("globals buffer exists")
            .clone();
        self.globals_staging.write(
            &self.device(world),
            encoder,
            &buffer,
            0,
            self.globals.as_bytes(),
        );
    }

    /// Pack and upload the frame's instance data, staging the bytes through
    /// the frame's encoder.
    fn upload_instances(&mut self, world: &LocalWorld, encoder: &mut wgpu::CommandEncoder) {
        self.packed_instances_cache.clear();
        self.packed_instances_cache
            .extend(self.visible_cache.iter().map(|entry| entry.mesh.instance));
        let buffer = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();
        self.instance_staging.write(
            &self.device(world),
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
    fn upload_metadata(&mut self, world: &LocalWorld, encoder: &mut wgpu::CommandEncoder) {
        if !self.metadata_dirty {
            return;
        }
        self.metadata_dirty = false;

        let needed = self.metadata.len().max(1) as u32;
        if needed > self.metadata_capacity {
            // Grow geometrically so repeated allocations amortize, and
            // recreate rather than resize: a buffer has a fixed size.
            let capacity = needed.max(self.metadata_capacity * 2);
            let buf = self.device(world).create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::mesh_metadata"),
                size: capacity as u64 * size_of::<MeshMetadata>() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Self::graph(world, self.context)
                .replace(self.metadata_buf, Resource::Buffer(buf))
                .expect("metadata buffer exists");
            self.metadata_capacity = capacity;
            // A replaced buffer invalidates every global group bound to it.
            self.rebuild_dirty_global_groups(world);
        }
        let buffer = Self::graph(world, self.context)
            .get_buffer(self.metadata_buf)
            .expect("metadata buffer exists")
            .clone();
        self.metadata_staging.write(
            &self.device(world),
            encoder,
            &buffer,
            0,
            self.metadata.as_bytes(),
        );
    }

    /// The graph node of the vertex pool's buffer for `layout`, creating it if
    /// the pool has never been asked for the layout before.
    ///
    /// The node is strong: the source owns the pool, no mesh does. See
    /// [`MeshSource::sync_pool_node`] for why nothing may depend on it.
    fn vertex_node(&mut self, world: &LocalWorld, layout: &VertexBufferLayoutDesc) -> ResourceId {
        if let Some(&id) = self.vertex_pool_ids.get(layout) {
            return id;
        }
        let id = Self::graph(world, self.context)
            .insert_strong(Resource::Virtual, &[])
            .expect("a new stream node has no dependencies");
        self.vertex_pool_ids.insert(layout.clone(), id);
        id
    }

    /// Point the stream node `id` at the vertex pool's buffer for `layout`.
    ///
    /// A stream node starts as a virtual stand-in, so its first sync swaps in
    /// the real buffer; later syncs only happen when the pool grew.
    fn sync_vertex_node(
        &mut self,
        world: &LocalWorld,
        id: ResourceId,
        layout: &VertexBufferLayoutDesc,
    ) {
        let buffer = self
            .vertex_pool
            .buffer(layout)
            .expect("the layout has a buffer");
        let mut graph = Self::graph(world, self.context);
        if graph.get_buffer(id) != Some(buffer) {
            graph
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
    fn ensure_instance_buffer(&mut self, world: &LocalWorld, count: u32) {
        if count <= self.instance_capacity {
            return;
        }
        let new_cap = count.max(self.instance_capacity * 2).max(1);
        let buf = self.device(world).create_buffer(&wgpu::BufferDescriptor {
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
    /// The pass itself lives in [crate::scene]; this only unpacks the source's
    /// caches and the closure that registers a newly resolved pipeline, so a
    /// family never borrows the whole source.
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
        let resources = self.render_resources(world);
        let device = self.device(world);

        // Split the borrows: the families mutate the pipeline list and the
        // resource graph through the register closure, while the caches are
        // disjoint fields.
        let (families, meshes, visible) = (
            &mut self.families,
            &mut self.visible_meshes_cache,
            &mut self.visible_cache,
        );
        let (pipelines, camera_buf, globals_buf, metadata_buf) = (
            &mut self.pipelines,
            self.camera_buf,
            self.globals_buf,
            self.metadata_buf,
        );
        let ctx = self.context;
        let mut graph = Self::graph(world, ctx);
        let mut register = |desc| {
            register_concrete(
                pipelines,
                &mut graph,
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

    /// Assemble the draws of the frame's visible set into `self.scene`.
    ///
    /// The handles every draw names — the bind groups, the vertex and index
    /// buffers, the pipeline — are cloned out of the resource graph into reused
    /// scratch lists first, so assembling touches the graph once per entry and
    /// the assembled scene owns everything it names.
    fn assemble_frame(&mut self, world: &LocalWorld) {
        let mut handles = std::mem::take(&mut self.entry_handle_cache);
        handles.clear();
        {
            let graph = Self::graph(world, self.context);
            for entry in &self.visible_cache {
                let mesh = world
                    .get::<GpuMesh>(entry.mesh.entity)
                    .expect("visible entity has GpuMesh");

                let mesh_bg = mesh.bind_group_id.map(|id| {
                    graph
                        .get_bind_group(id)
                        .expect("mesh bind group exists")
                        .clone()
                });

                let material_bg = world.get::<GpuMaterial>(entry.mesh.entity).map(|material| {
                    graph
                        .get_bind_group(material.bind_group_id)
                        .expect("material bind group exists")
                        .clone()
                });

                let mut vertex_buffers = ArrayVec::new();
                for &(slot, buffer) in &mesh.vertex_buffers {
                    let buffer = graph
                        .get_buffer(buffer)
                        .expect("mesh vertex buffer exists")
                        .clone();
                    // A mesh binds its vertex buffers whole; the draw's range
                    // is what picks the mesh's slice out of the pool.
                    vertex_buffers.push((slot, buffer.clone(), 0..buffer.size()));
                }

                let index_buffer = mesh.index_buffer.map(|(buffer, format)| {
                    (
                        graph
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

        // Every registered pipeline's handle and global bind group, indexed the
        // same way [`MeshSource::pipelines`] is. A pipeline that binds no
        // global group carries `None`. The list is reused between frames, so a
        // steady scene allocates nothing.
        let mut pipeline_handles = std::mem::take(&mut self.pipeline_handle_cache);
        pipeline_handles.clear();
        {
            let graph = Self::graph(world, self.context);
            pipeline_handles.extend(self.pipelines.iter().map(|registered| PipelineHandles {
                pipeline: registered.pipeline.clone(),
                global: registered.global.as_ref().map(|global| {
                    graph
                        .get_bind_group(global.id)
                        .expect("global group exists")
                        .clone()
                }),
            }));
        }

        let instance_buf = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();

        // The scene keeps its allocation across frames: `clear` drops the
        // draws but not the buffer behind them.
        self.scene.clear();
        assemble_scene(
            &mut self.scene,
            &self.visible_cache,
            &pipeline_handles,
            &handles,
            &instance_buf,
        );

        self.pipeline_handle_cache = pipeline_handles;
        self.entry_handle_cache = handles;
    }
}

impl FrameSource for MeshSource {
    fn build_scene(
        &mut self,
        world: &LocalWorld,
        _ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // The scene is cleared on every path: a frame that records it must
        // never replay the previous frame's draws, and a source with nothing
        // to draw leaves it empty rather than absent.
        self.scene.clear();

        // A frame that draws still has to publish a changed metadata array, but
        // one that does not draw can leave it for the next frame.
        self.upload_metadata(world, encoder);

        // The frame is drawn from the first camera in `world`, copied out of
        // its cell so the borrow does not block the world accesses below.
        let camera = world.query::<&Camera>().next().map(|(_, c)| Camera {
            clip_from_world: c.clip_from_world,
            position: c.position,
        });
        let Some(camera) = camera else {
            return;
        };

        // The target the frame is specialized for, written by the frame loop
        // before it renders. Without it no pipeline can be resolved, and a
        // stale one is worse than none: reading the previous frame's target
        // would silently specialize for the wrong attachments.
        let Some(target) = frame_target(world) else {
            log::warn!(
                "MeshSource::build_scene: no FrameTarget in the world, so the \
                 frame's render target is unknown and nothing is drawn; write \
                 one by binding a render target before rendering"
            );
            return;
        };
        let surface = target.surface;

        // Update global uniforms.
        self.globals.time += self.globals.delta_time;
        self.globals.frame_count += 1;
        self.upload_globals(world, encoder);

        let view = View::new(camera.clip_from_world, camera.position);
        self.upload_camera(world, encoder, &view);

        // A rebuild has to happen before the global groups are read, and the
        // metadata upload above may have replaced the buffer one of them binds.
        self.rebuild_dirty_global_groups(world);

        // Collect, cull and sort the visible set in one pass. The cache keeps
        // its allocation between frames, so a steady scene allocates nothing.
        self.collect_and_sort_visible(world, &camera, surface);
        if self.visible_cache.is_empty() {
            return;
        }
        let instance_count = self.visible_cache.len() as u32;

        // Pack instance data into the reused scratch buffer and upload it.
        self.ensure_instance_buffer(world, instance_count);
        self.upload_instances(world, encoder);

        self.assemble_frame(world);
    }

    fn scene(&self) -> &Scene {
        &self.scene
    }

    fn order(&self) -> FrameOrder {
        FrameOrder::MESH
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Transform, UnlitPipeline, ZSortedDrawing};
    use crate::source::{FrameTarget, set_frame_target, spawn_context};
    use unlit_ecs::Entity;
    use wgpu_unlit_render::render_attachments::{RenderAttachments, create_render_target};

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
        world: LocalWorld,
        source: MeshSource,
        key: UnlitPipelineKey,
        target: FrameTarget,
    }

    impl Harness {
        /// The triangle mesh every mesh-level test allocates.
        fn tri_mesh(&mut self) -> GpuMesh {
            self.source.allocate_unlit_mesh(
                &self.world,
                &self.key,
                &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                Some(&[0u32, 1, 2]),
            )
        }

        /// A command encoder to record a test's uploads into.
        fn encoder(&self) -> wgpu::CommandEncoder {
            self.source.device(&self.world).create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("test::encoder"),
                },
            )
        }
    }

    /// Resolve every entity in `world` on `surface` and return the concrete
    /// pipeline the last one was drawn with.
    fn resolve_draw(
        source: &mut MeshSource,
        world: &LocalWorld,
        surface: SurfaceKey,
    ) -> PipelineId {
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
        let mut world = LocalWorld::new();
        let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
        let mut source = MeshSource::new(&world, ctx);
        source.register_unlit_family(&world);
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&source.device(&world)));
        let target = bind_test_target(&mut world, ctx);
        Harness {
            world,
            source,
            key,
            target,
        }
    }

    /// Register a color texture and a matching depth texture in the context's
    /// graph and state them as the frame's target.
    fn bind_test_target(world: &mut LocalWorld, ctx: RenderContext) -> FrameTarget {
        let device = world
            .get::<wgpu::Device>(ctx.device)
            .expect("the context's device")
            .clone();
        let ft = create_render_target(
            &device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            TEST_SIZE,
            TEST_SIZE,
            1,
        );
        let target = {
            let mut graph = world
                .get_mut::<ResourceGraph>(ctx.graph)
                .expect("the context's graph");
            let color = graph
                .insert_strong(
                    ft.color
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                    &[],
                )
                .expect("a color view has no dependencies");
            let depth = graph
                .insert_strong(
                    ft.depth
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                    &[],
                )
                .expect("a depth view has no dependencies");
            let attachments = RenderAttachments::from_views(
                graph.get_texture_view(color).cloned(),
                graph.get_texture_view(depth).cloned(),
                None,
            );
            FrameTarget {
                surface: attachments.surface_key(),
                width: TEST_SIZE,
                height: TEST_SIZE,
            }
        };
        assert!(
            set_frame_target(world, target),
            "the context spawned a slot"
        );
        target
    }

    /// A camera at `eye` looking at the origin, with a frustum wide enough
    /// that nothing in these tests is culled.
    fn test_camera(eye: glam::Vec3) -> Camera {
        let view = glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y);
        Camera {
            clip_from_world: test_perspective() * view,
            position: eye,
        }
    }

    /// The entities of the source's visible cache in draw order.
    fn drawn(source: &MeshSource) -> Vec<Entity> {
        source
            .visible_cache
            .iter()
            .map(|entry| entry.mesh.entity)
            .collect()
    }

    /// A variant of the standard one that reads no UV — so no base-color
    /// texture either — and therefore packs its vertices differently.
    fn uv_less_options(device: &wgpu::Device) -> UnlitOptions {
        use wgpu_unlit_render::pipeline::UnlitFlags;
        let mut options = UnlitOptions::standard(device);
        options.flags &= !(UnlitFlags::VERTEX_UV | UnlitFlags::BASE_COLOR_TEXTURE);
        options
    }

    /// Register a 2D texture with `source` and return a view id and a sampler
    /// id, ready for [`MeshSource::allocate_unlit_material`].
    fn test_material_resources(harness: &mut Harness) -> (ResourceId, ResourceId) {
        let texture =
            harness
                .source
                .device(&harness.world)
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
            .register_texture_and_default_view(&harness.world, texture)
            .1;
        let sampler = harness.source.register_sampler(&harness.world, None);
        (view, sampler)
    }

    /// A raw-layout key for `mesh` on `surface`.
    fn draw_key(surface: SurfaceKey, mesh: &GpuMesh) -> DrawKey {
        DrawKey::for_mesh(surface, mesh)
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

    #[test]
    fn registering_a_family_compiles_nothing() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut world = LocalWorld::new();
        let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
        let mut source = MeshSource::new(&world, ctx);

        source.register_unlit_family(&world);

        // Registration records the family; its first concrete pipeline is
        // built only when a draw resolves a variant.
        assert!(source.pipelines.is_empty());
        assert_eq!(source.families.len(), 1);
    }

    #[test]
    fn a_source_starts_with_no_pipelines() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut world = LocalWorld::new();
        let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
        let source = MeshSource::new(&world, ctx);

        // Nothing is privileged: pipelines arrive only through registration.
        assert!(source.pipelines.is_empty());
    }

    #[test]
    fn a_unlit_mesh_is_built_from_its_keys_options() {
        let mut h = harness();
        let standard = UnlitPipelineKey::new(UnlitOptions::standard(&h.source.device(&h.world)));
        let uv_less = UnlitPipelineKey::new(uv_less_options(&h.source.device(&h.world)));

        // Each mesh packs the channels its own key reads, not the last
        // registered family's.
        let standard_mesh = h.source.allocate_unlit_mesh(
            &h.world,
            &standard,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        );
        let uv_less_mesh = h.source.allocate_unlit_mesh(
            &h.world,
            &uv_less,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        );
        assert!(
            unlit_flags_for_layout(&standard_mesh.vertex_layout).contains(UnlitFlags::VERTEX_UV)
        );
        assert!(
            !unlit_flags_for_layout(&uv_less_mesh.vertex_layout).contains(UnlitFlags::VERTEX_UV)
        );

        // A material is built against the key's layout: the standard variant
        // samples a base-color texture, the UV-less one does not.
        let (view, sampler) = test_material_resources(&mut h);
        assert!(
            h.source
                .allocate_unlit_material(&h.world, &standard, view, sampler)
                .is_some()
        );
        assert!(
            h.source
                .allocate_unlit_material(&h.world, &uv_less, view, sampler)
                .is_none()
        );
    }

    #[test]
    fn every_registered_pipeline_gets_its_own_global_group() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        let a = h.target.surface;
        // A second target, so the family resolves two variants.
        let b = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            ..a
        };
        h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        resolve_draw(&mut h.source, &h.world, a);
        resolve_draw(&mut h.source, &h.world, b);
        assert_eq!(h.source.pipelines.len(), 2);

        let ctx = h.source.context();
        let ids: Vec<_> = h
            .source
            .pipelines
            .iter()
            .map(|registered| registered.global.as_ref().expect("has a global group").id)
            .collect();
        for (index, &id) in ids.iter().enumerate() {
            assert!(
                MeshSource::graph(&h.world, ctx)
                    .get_bind_group(id)
                    .is_some(),
                "pipeline {index}'s global group is in the graph"
            );
            // No two pipelines share one: each binds its own layout.
            assert!(!ids[..index].contains(&id));
        }
    }

    #[test]
    fn a_variant_is_compiled_when_a_draw_resolves_it() {
        let mut h = harness();

        // A family has no concrete pipeline until a draw asks for a variant.
        let mesh = h.tri_mesh();
        let surface = h.target.surface;
        h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(h.key.clone()),
        ));
        resolve_draw(&mut h.source, &h.world, surface);

        assert_eq!(h.source.pipelines.len(), 1);
    }

    #[test]
    fn a_material_is_allocated_from_caller_owned_resources() {
        let mut h = harness();
        let (view, sampler) = test_material_resources(&mut h);

        let material = h
            .source
            .allocate_unlit_material(&h.world, &h.key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        let ctx = h.source.context();
        assert!(
            MeshSource::graph(&h.world, ctx)
                .get_bind_group(material.bind_group_id)
                .is_some()
        );
    }

    #[test]
    fn a_variant_without_a_base_color_texture_allocates_no_material() {
        let mut h = harness();
        let key = UnlitPipelineKey::new(uv_less_options(&h.source.device(&h.world)));
        let (view, sampler) = test_material_resources(&mut h);

        assert!(
            h.source
                .allocate_unlit_material(&h.world, &key, view, sampler)
                .is_none()
        );
    }

    // -- removal -----------------------------------------------------------

    #[test]
    fn removing_a_mesh_frees_every_resource_built_from_it() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        let ctx = h.source.context();

        // The root is the mesh's lifetime entry point; its parts are the bind
        // group and the mesh-info uniform that feeds it. The mesh's vertices
        // and indices live in pools, so they are strong nodes the source owns
        // and survive the mesh — what the mesh loses is its share of them,
        // which `remove_mesh` hands back.
        let before = MeshSource::graph(&h.world, ctx).len();
        let index_pool_free = h.source.index_pool.free_space();
        assert!(matches!(
            MeshSource::graph(&h.world, ctx).get(mesh.root),
            Some(Resource::Virtual)
        ));
        let bind_group = mesh.bind_group_id.expect("the mesh has a group");
        let root = mesh.root;

        h.source.remove_mesh(&h.world, mesh);

        assert!(
            MeshSource::graph(&h.world, ctx).get(root).is_none(),
            "root removed"
        );
        assert!(
            MeshSource::graph(&h.world, ctx).get(bind_group).is_none(),
            "the bind group built from them removed"
        );
        assert!(
            MeshSource::graph(&h.world, ctx).len() < before,
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
        let mesh = h.tri_mesh();
        let ctx = h.source.context();

        // The pools are the source's, not the mesh's: a mesh that goes away
        // must not take the shared buffers with it, so they sit outside the
        // root and the root holds only what is the mesh's own.
        let dependencies: Vec<_> = MeshSource::graph(&h.world, ctx)
            .dependencies(mesh.root)
            .collect();
        let bind_group = mesh.bind_group_id.expect("the mesh has a group");
        assert!(
            dependencies.contains(&bind_group),
            "the bind group is under the root"
        );
        let pool = h.source.index_pool_id;
        assert!(
            !dependencies.contains(&pool),
            "the index pool is not under the root"
        );
        assert!(
            MeshSource::graph(&h.world, ctx).get(pool).is_some(),
            "the index pool survives"
        );
    }

    #[test]
    fn a_removed_mesh_frees_its_metadata_slot() {
        let mut h = harness();
        let first = h.tri_mesh();
        let second = h.tri_mesh();
        let first_index = first.metadata_index;
        assert_ne!(first_index, second.metadata_index);

        h.source.remove_mesh(&h.world, first);
        let reused = h.tri_mesh();

        assert_eq!(
            reused.metadata_index, first_index,
            "the slot the removed mesh held is handed out again"
        );
        assert_ne!(reused.metadata_index, second.metadata_index);
    }

    #[test]
    fn removing_a_material_keeps_the_resources_it_reads() {
        let mut h = harness();
        let ctx = h.source.context();
        let (view, sampler) = test_material_resources(&mut h);
        let material = h
            .source
            .allocate_unlit_material(&h.world, &h.key, view, sampler)
            .expect("the standard variant reads a base-color texture");

        h.source.remove_material(&h.world, material.clone());

        assert!(
            MeshSource::graph(&h.world, ctx)
                .get(material.bind_group_id)
                .is_none(),
            "the bind group is gone"
        );
        // The view and sampler are the caller's, so removing the material
        // that reads them leaves them alone.
        assert!(
            MeshSource::graph(&h.world, ctx).get(view).is_some(),
            "the view stays"
        );
        assert!(
            MeshSource::graph(&h.world, ctx).get(sampler).is_some(),
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
        let pool = h.source.index_pool_id;
        assert_eq!(first.index_buffer.map(|(id, _)| id), Some(pool));
        assert_eq!(second.index_buffer.map(|(id, _)| id), Some(pool));
        assert_ne!(first.first, second.first, "the slices do not overlap");

        // The ranges tile the pool in allocation order.
        let first_range = h.source.index_pool.allocation_size(
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
        let mut h = harness();
        let first = h.tri_mesh();
        let first_offset = first.first;
        h.source.remove_mesh(&h.world, first);

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
        let first_node = first.index_buffer.expect("the mesh is indexed").0;
        let first_offset = first.first;

        // Enough meshes to push the pool past its starting size.
        let mut last = None;
        for _ in 0..40 {
            last = Some(h.tri_mesh());
        }
        let last = last.expect("the loop runs");

        // The node still names the pool, and the graph holds the buffer the
        // pool currently does: a mesh needs no update after a grow.
        assert_eq!(last.index_buffer.map(|(id, _)| id), Some(first_node));
        assert_eq!(
            MeshSource::graph(&h.world, ctx).get_buffer(first_node),
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
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&h.source.device(&h.world)));
        let mesh = h.source.allocate_unlit_mesh(
            &h.world,
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
        let mut h = harness();
        let first = h.tri_mesh();
        let second = h.tri_mesh();
        let ctx = h.source.context();

        // Every stream of a mesh names its pool's node, and both meshes name
        // the same node per slot.
        for ((_, first_id), (_, second_id)) in first
            .vertex_buffers
            .iter()
            .zip(second.vertex_buffers.iter())
        {
            assert_eq!(first_id, second_id, "the streams share a buffer");
            assert!(
                MeshSource::graph(&h.world, ctx)
                    .get_buffer(*first_id)
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
        h.source.remove_mesh(&h.world, first);

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
        let nodes: Vec<_> = first.vertex_buffers.iter().map(|(_, id)| *id).collect();
        let base_vertex = first.base_vertex;

        // Enough meshes to push the pool past its starting capacity.
        let mut last = None;
        for _ in 0..80 {
            last = Some(h.tri_mesh());
        }
        let last = last.expect("the loop runs");

        // The nodes still name the streams, and each holds the buffer the pool
        // currently does for its layout: a mesh needs no update after a grow.
        // Only the pool-backed slots count: the per-instance slot is bound by
        // the source from its own buffer, not the pool's.
        let stream_slots = last
            .vertex_layout
            .iter()
            .filter(|(slot, _)| *slot != INSTANCE_SLOT);
        for (node, (_, layout)) in nodes.iter().zip(stream_slots) {
            let pool_buffer = h
                .source
                .vertex_pool
                .buffer(layout)
                .expect("the layout has a buffer");
            assert_eq!(
                MeshSource::graph(&h.world, ctx).get_buffer(*node),
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
        let z_sorted = h.world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));
        let opaque = h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(h.key.clone()),
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

        assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);
        assert!(h.source.visible_cache[0].depth > h.source.visible_cache[1].depth);
    }

    #[test]
    fn z_sorted_entities_are_drawn_back_to_front() {
        let mut h = harness();
        let mesh = h.tri_mesh();

        let near = h.world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));
        let middle = h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));
        let far = h.world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, -4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));
        // Spawned out of order on purpose: registration order is not draw
        // order for z-sorted entities.
        assert_ne!(drawn(&h.source), vec![far, middle, near]);

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

        assert_eq!(drawn(&h.source), vec![far, middle, near]);
    }

    #[test]
    fn entities_are_drawn_in_pipeline_id_order() {
        let mut h = harness();
        // A second key whose specialized options differ, so it resolves to its
        // own concrete pipeline.
        let uv_less_key = UnlitPipelineKey::new(uv_less_options(&h.source.device(&h.world)));

        // Warm both variants, first key first, so their pipeline ids follow
        // the order they were resolved in.
        let standard_mesh = h.tri_mesh();
        let uv_less_mesh = h.source.allocate_unlit_mesh(
            &h.world,
            &uv_less_key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        );
        let surface = h.target.surface;
        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        let warm_standard = h.world.spawn((
            Transform::default(),
            standard_mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let warm_uv_less = h.world.spawn((
            Transform::default(),
            uv_less_mesh.clone(),
            UnlitPipeline::new(uv_less_key.clone()),
        ));
        h.source
            .collect_and_sort_visible(&h.world, &camera, surface);
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
            .find(|entry| entry.mesh.entity == warm_uv_less)
            .expect("the uv-less entity is visible")
            .pipeline_id;
        assert!(first_id < second_id);
        assert!(h.world.despawn(warm_standard));
        assert!(h.world.despawn(warm_uv_less));

        // Spawn the later-drawn entity first: draw order is the pipeline id,
        // not the spawn order.
        let second = h.world.spawn((
            Transform::default(),
            uv_less_mesh,
            UnlitPipeline::new(uv_less_key),
        ));
        let first = h.world.spawn((
            Transform::default(),
            standard_mesh,
            UnlitPipeline::new(h.key.clone()),
        ));

        h.source
            .collect_and_sort_visible(&h.world, &camera, surface);

        assert_eq!(drawn(&h.source), vec![first, second]);
    }

    #[test]
    fn opaque_draws_sharing_a_material_stay_adjacent() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        let (view, sampler) = test_material_resources(&mut h);
        let shared = h
            .source
            .allocate_unlit_material(&h.world, &h.key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        let (view, sampler) = test_material_resources(&mut h);
        let other = h
            .source
            .allocate_unlit_material(&h.world, &h.key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        assert_ne!(shared.sort_key(), other.sort_key());

        let a = h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
            shared.clone(),
        ));
        // The odd one out: sharing no material with the other two, so it is
        // the draw that has to sit on the other side of the pair.
        let _other_material = h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
            other.clone(),
        ));
        let c = h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(h.key.clone()),
            shared.clone(),
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

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
        let opaque = h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let z_sorted = h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);
        assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);

        // The cache is cleared and refilled, not reallocated.
        let capacity = h.source.visible_cache.capacity();
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);
        assert_eq!(drawn(&h.source), vec![opaque, z_sorted]);
        assert_eq!(h.source.visible_cache.capacity(), capacity);
    }

    // -- specialization cache ------------------------------------------------

    #[test]
    fn a_render_target_change_respecializes_the_family() {
        let mut h = harness();
        let mesh = h.tri_mesh();

        let internal = h.target.surface;
        h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let first = resolve_draw(&mut h.source, &h.world, internal);
        // The same key twice reuses one compiled pipeline.
        assert_eq!(resolve_draw(&mut h.source, &h.world, internal), first);

        // A different color format is a different surface, so the family
        // compiles a second variant for it.
        let other = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            ..internal
        };
        let second = resolve_draw(&mut h.source, &h.world, other);
        assert_ne!(second, first);
        assert_eq!(h.source.pipelines.len(), 2);
    }

    #[test]
    fn a_mesh_layout_change_respecializes_the_family() {
        let mut h = harness();
        let surface = h.target.surface;
        let standard = h.tri_mesh();
        h.world.spawn((
            Transform::default(),
            standard.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let first = resolve_draw(&mut h.source, &h.world, surface);

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

        h.world.spawn((
            Transform::default(),
            other.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let second = resolve_draw(&mut h.source, &h.world, surface);

        assert_ne!(second, first);
        assert_eq!(h.source.pipelines.len(), 2);
    }

    #[test]
    fn meshes_that_share_flags_share_one_variant() {
        let mut h = harness();
        let surface = h.target.surface;
        let base = h.tri_mesh();
        h.world.spawn((
            Transform::default(),
            base.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let first = resolve_draw(&mut h.source, &h.world, surface);

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

        h.world.spawn((
            Transform::default(),
            twin.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let second = resolve_draw(&mut h.source, &h.world, surface);
        assert_eq!(second, first);
        assert_eq!(h.source.pipelines.len(), 1);
    }

    #[test]
    fn an_entity_without_a_pipeline_is_not_drawn() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        // No GpuPipeline: the query filters it out.
        let _unpipelined = h.world.spawn((Transform::default(), mesh.clone()));
        let drawn_entity = h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(h.key.clone()),
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

        assert_eq!(drawn(&h.source), vec![drawn_entity]);
    }

    // -- metadata buffer -----------------------------------------------------

    #[test]
    fn the_metadata_buffer_grows_only_when_the_array_outgrows_it() {
        let mut h = harness();
        let ctx = h.source.context();
        // The source starts with room for one entry and grows by doubling,
        // so only the entries past the capacity it holds recreate the buffer.
        assert_eq!(h.source.metadata_capacity, 1);

        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(h.source.metadata_capacity, 1, "one entry fills the room");

        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(
            h.source.metadata_capacity, 2,
            "a second entry outgrows room for one"
        );

        // A third entry does not fit in two, so the buffer doubles again.
        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(h.source.metadata_capacity, 4);

        // A fourth entry does fit in four, so the buffer is left alone.
        let before = MeshSource::graph(&h.world, ctx)
            .get_buffer(h.source.metadata_buf)
            .cloned();
        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(h.source.metadata_capacity, 4, "four entries fit");
        assert_eq!(
            MeshSource::graph(&h.world, ctx).get_buffer(h.source.metadata_buf),
            before.as_ref(),
            "no growth means the same buffer, so no global group is rebuilt"
        );
    }

    #[test]
    fn a_removed_mesh_marks_the_metadata_array_for_upload() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert!(!h.source.metadata_dirty, "the mesh's own upload cleared it");

        // Removing the mesh clears its entry in the array, so the array
        // reaches the GPU again on the next frame.
        h.source.remove_mesh(&h.world, mesh);
        assert!(h.source.metadata_dirty, "removing a mesh marks the array");
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert!(!h.source.metadata_dirty, "the upload cleared the mark");
    }

    // -- a whole built scene --------------------------------------------------

    #[test]
    fn a_frame_without_a_camera_only_clears() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        // A drawable entity, but nothing the source can view it from.
        h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(h.key.clone()),
        ));

        // The source builds nothing, so the driver's pass draws nothing while
        // still applying the frame's clears.
        let mut encoder = h.encoder();
        let ctx = h.source.context();
        h.source.build_scene(&h.world, ctx, &mut encoder);
        assert!(h.source.visible_cache.is_empty(), "nothing was drawn");
        assert!(h.source.scene().draws.is_empty(), "the scene is empty");
    }

    #[test]
    fn rendering_a_world_twice_reuses_every_per_frame_cache() {
        let mut h = harness();
        // A variant that binds no material group, so the frame needs no
        // material to be a complete draw.
        let key = UnlitPipelineKey::new(uv_less_options(&h.source.device(&h.world)));
        let mesh = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        );
        h.world
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        h.world.spawn((
            Transform::default(),
            mesh,
            UnlitPipeline::new(key),
            ZSortedDrawing,
        ));

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);
        let drawn_first = drawn(&h.source);
        assert_eq!(drawn_first.len(), 1, "the entity was drawn");
        let capacities = (
            h.source.visible_cache.capacity(),
            h.source.entry_handle_cache.capacity(),
            h.source.pipeline_handle_cache.capacity(),
            h.source.scene.draws.capacity(),
        );

        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);
        assert_eq!(drawn(&h.source), drawn_first, "the same draws, in order");
        assert_eq!(
            (
                h.source.visible_cache.capacity(),
                h.source.entry_handle_cache.capacity(),
                h.source.pipeline_handle_cache.capacity(),
                h.source.scene.draws.capacity(),
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
        let key = UnlitPipelineKey::new(uv_less_options(&h.source.device(&h.world)));
        // Both meshes are indexed and share the pool's index buffer, so what
        // tells their draws apart is the slice each one names.
        let first = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        );
        let second = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        );
        assert!(first.indexed && second.indexed, "both meshes are indexed");
        assert_ne!(first.first, second.first, "the slices do not overlap");

        h.world
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        h.world
            .spawn((Transform::default(), first, UnlitPipeline::new(key.clone())));
        h.world
            .spawn((Transform::default(), second, UnlitPipeline::new(key)));

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);
        let handles = &h.source.entry_handle_cache;
        assert_eq!(handles.len(), 2, "one handle set per drawn entity");
        assert_eq!(handles[0].shape.first, 0, "the first mesh starts at zero");
        assert_ne!(
            handles[0].shape.first, handles[1].shape.first,
            "each draw keeps its own slice of the pool"
        );
        assert_eq!(drawn(&h.source).len(), 2, "both entities were drawn");
    }
}
