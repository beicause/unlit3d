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
//! `&World` and fetches the device, the queue and the resource graph
//! itself, so the source never holds a borrow of them between calls.

use arrayvec::ArrayVec;
use core::any::TypeId;
use core::ops::DerefMut;
use hashbrown::HashMap;
use std::rc::Rc;

use unlit_ecs::{TypeIdHashMap, World};
use unlit_wgpu::array_pool::ArrayPool;
use unlit_wgpu::buffer_pool::BufferPool;
use unlit_wgpu::globals::{Globals, View};
use unlit_wgpu::mesh::{JointMatrix, MeshMetadata};
use unlit_wgpu::pipeline::supports_storage_buffers;
use unlit_wgpu::resources::{
    Resource, ResourceGraph, ResourceId, TextureExt, TextureView, Virtual,
};
use unlit_wgpu::scene::{MAX_VERTEX_BUFFERS, Scene};
use unlit_wgpu::specialize::{PipelineVariant, SurfaceKey, VertexBufferLayoutDesc, VertexLayout};
use unlit_wgpu::staging::StagingBuffer;
use unlit_wgpu::texel_array::{Array, ArrayHandle};
use unlit_wgpu::vertex_pool::VertexStreamPool;
use zerocopy::IntoBytes;

use crate::components::{Camera, GpuMaterial, GpuMesh, MeshParts};
use crate::culling::VisibleMesh;
use crate::mesh::MeshDesc;
use crate::pipeline::{
    GlobalResources, InstanceContext, InstanceData, Rebuild, RegisteredGlobal,
    RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineId, RenderPipelineKey,
};
use crate::scene::{
    AnyFamily, DrawHandlesKey, EntryHandles, Family, FamilyId, RenderPipelineHandles, SceneFrame,
    VisibleEntry, assemble_scene, collect_and_sort_visible,
};
use crate::source::{FrameOrder, FrameSource, RenderContext, frame_target, frame_viewport};
use unlit_wgpu::capabilities::DeviceCapabilities;

/// The capacity that covers `needed` after growing from `current`.
///
/// Growing by 1.5x amortizes repeated growth, and the `max` keeps the buffer
/// from shrinking back when the array temporarily shrinks; the floor of one
/// keeps every buffer non-empty.
fn grown_capacity(current: u32, needed: u32) -> u32 {
    needed.max(current + current / 2).max(1)
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
    buffers: GlobalResources,
    desc: RegisteredRenderPipeline,
) -> RenderPipelineId {
    // The material and mesh layouts describe a pipeline's binding interface,
    // but they do not outlive registration: the source builds those groups
    // from the family it registered, and wgpu already holds the pipeline's own
    // layout internally.
    let RegisteredRenderPipeline {
        pipeline, global, ..
    } = desc;

    // A globally bound pipeline reads the source's own frame buffers, so it
    // depends on all of them: a rebuild of any marks the group dirty. The
    // group is rebuilt through the pipeline's own recipe, so a custom pipeline
    // keeps control of what its layout binds.
    let global = global.map(|rebuild| {
        // The first build is eager: a node has to hold a resource of the kind
        // its typed id names, and only the rebuilds that follow are deferred.
        let bind_group: wgpu::BindGroup = rebuild.build(graph);
        let id = graph.insert(bind_group, Some(rebuild));
        buffers.declare_dependencies(graph, &id);
        RegisteredGlobal { id }
    });

    pipelines.push(RegisteredPipeline { pipeline, global });
    // The length before the push is the index the pipeline landed on.
    RenderPipelineId::new((pipelines.len() - 1) as u32)
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
/// — [`MeshSourceUnlitExt::register_unlit_family`](crate::unlit::MeshSourceUnlitExt::register_unlit_family) for the built-in unlit shader — and
/// mount it with [`spawn_source`](crate::source::WorldSourceExt::spawn_source).
pub struct MeshSource {
    /// The world addresses of the frame's GPU state, kept so every entry point
    /// fetches the device, the queue and the graph for itself instead of
    /// holding a borrow of them between calls.
    pub(crate) context: RenderContext,

    /// Resource id of the camera uniform buffer.
    camera_buf: ResourceId<wgpu::Buffer>,
    /// Resource id of the frame-globals uniform buffer.
    globals_buf: ResourceId<wgpu::Buffer>,
    /// The mesh-metadata array, held in whichever resource the device reads.
    metadata_array: Array,
    /// Resource id of the mesh-metadata array.
    metadata_buf: ResourceId<ArrayHandle>,
    /// The frame's joint-matrix array, held in whichever resource the device
    /// reads.
    joints_array: Array,
    /// Resource id of the frame's joint-matrix array.
    joints_buf: ResourceId<ArrayHandle>,
    /// The frame's morph-weight array, held in whichever resource the device
    /// reads.
    morph_weights_array: Array,
    /// Resource id of the frame's morph-weight array.
    morph_weights_buf: ResourceId<ArrayHandle>,
    /// The frame's morph-displacement array, pooling every morphed mesh's
    /// deltas into one resource.
    ///
    /// A mesh's displacements are its own geometry, but one array holds them
    /// for the frame and the mesh names its slice through its metadata entry's
    /// `morph_deltas_offset`. That is what leaves the mesh group nothing to
    /// bind.
    morph_deltas_pool: ArrayPool,
    /// Resource id of the frame's morph-displacement array.
    morph_deltas_buf: ResourceId<ArrayHandle>,
    /// Whether a mesh's displacements were written into the pool since the
    /// array was last uploaded.
    ///
    /// Allocating or removing a mesh marks it; the next
    /// [build](FrameSource::build_scene) uploads the array then, so the upload
    /// always lands in the same encoder as the draws that read it.
    morph_deltas_dirty: bool,
    /// Per-frame globals (advanced once per built scene).
    globals: Globals,
    /// Metadata entries, one per live uploaded mesh.
    metadata: Vec<MeshMetadata>,
    /// Metadata slots whose mesh was removed and whose index is free to hand
    /// out again.
    free_metadata: Vec<u32>,
    /// Whether the metadata array changed since it was last uploaded.
    ///
    /// Allocating or removing a mesh marks it; the next
    /// [build](FrameSource::build_scene) uploads the array then, so an upload
    /// always lands in the same encoder as the draws that read it.
    metadata_dirty: bool,

    /// The frame's joint matrices, packed in visible-instance order.
    ///
    /// Reused across frames rather than reallocated: the packed data is exactly
    /// what is uploaded, so the array and the buffer stay one allocation each.
    packed_joints: Vec<JointMatrix>,
    /// The frame's morph weights, packed in visible-instance order.
    packed_morph_weights: Vec<f32>,

    /// Reused staging buffers, one per buffer uploaded to every frame, so a
    /// steady frame reaches the GPU without a per-frame allocation or
    /// submission.
    camera_staging: StagingBuffer,
    globals_staging: StagingBuffer,

    /// The pool every mesh uploaded through
    /// [`MeshSourceUnlitExt::allocate_unlit_mesh`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_mesh) keeps its indices in, as one large
    /// buffer shared by every indexed mesh.
    ///
    /// A mesh names it through [`MeshParts::index_buffer`], which is why the
    /// buffer is a node in the resource graph and has to be replaced there
    /// when the pool grows; see [`MeshSource::sync_pool_node`].
    pub(crate) index_pool: BufferPool,
    /// The graph node of [`MeshSource::index_pool`]'s buffer.
    pub(crate) index_pool_id: ResourceId<wgpu::Buffer>,
    /// The pool every mesh uploaded through
    /// [`MeshSourceUnlitExt::allocate_unlit_mesh`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_mesh) keeps its vertices in: one large
    /// buffer per vertex layout, so meshes that share a layout share a buffer,
    /// and one element allocation covers every stream of a mesh at the same
    /// element index.
    pub(crate) vertex_pool: VertexStreamPool,
    /// The graph node of each of [`MeshSource::vertex_pool`]'s buffers, by the
    /// layout the buffer is shaped for.
    vertex_pool_ids: HashMap<VertexBufferLayoutDesc, ResourceId<wgpu::Buffer>>,

    /// Every concrete pipeline registered with this source, in registration
    /// order.
    ///
    /// A pipeline is appended the first time a family resolves a key that
    /// needs it, so an entity's concrete pipeline exists once its family has
    /// resolved the draw's surface and mesh layout.
    pipelines: Vec<RegisteredPipeline>,

    /// Every pipeline family registered with this source, in registration
    /// order. A family's index in this list is the [FamilyId] its visible
    /// entries carry.
    ///
    /// Registration compiles nothing: a family appends to
    /// [`MeshSource::pipelines`] lazily, the first time one of its variant
    /// keys is resolved.
    families: Vec<Box<dyn AnyFamily>>,
    /// The [FamilyId] of each registered family, keyed by the [TypeId] of its
    /// key type.
    family_ids: TypeIdHashMap<FamilyId>,

    // -- cached per-frame allocations ------------------------------------------
    /// Reused Vec of the meshes that passed this frame's frustum culling.
    visible_meshes_cache: Vec<VisibleMesh>,
    /// Reused Vec for visible-entity collection and per-frame sorting.
    visible_cache: Vec<VisibleEntry>,
    /// Reused Vec of the instance-stream binding of each registered family,
    /// indexed by [FamilyId].
    family_instance_cache: Vec<Option<(u32, wgpu::Buffer)>>,
    /// Reused Vec for cloned pipeline handles while the scene is built.
    pipeline_handle_cache: Vec<RenderPipelineHandles>,
    /// The frame's interned draw handles, one per distinct
    /// [`DrawHandlesKey`] the visible set named. Reused between frames, so a
    /// steady scene allocates nothing.
    entry_handle_cache: HashMap<DrawHandlesKey, EntryHandles>,
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
    /// [`MeshSourceUnlitExt::register_unlit_family`](crate::unlit::MeshSourceUnlitExt::register_unlit_family) does it for you. Registration
    /// compiles nothing; a family's first concrete pipeline is built the first
    /// time an entity that uses it is drawn.
    ///
    /// # Panics
    ///
    /// If `ctx` does not name the world's device, queue and resource graph.
    pub fn new(world: &World, ctx: RenderContext) -> Self {
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
        let camera_buf = graph.insert(
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::camera"),
                size: size_of::<View>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            None,
        );

        // Globals uniform buffer.
        let globals = Globals::default();
        let globals_buf = graph.insert(
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::globals"),
                size: size_of::<Globals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            None,
        );

        // The arrays the frame's shaders read, each in whichever resource the
        // device can read: a storage buffer where it has them, a texture where
        // it does not. Metadata starts with room for one entry and the pose
        // arrays with room for one element — the smallest a shader can read,
        // since neither resource can be empty — and all three grow on the first
        // built scene that needs more.
        let array_max_dimension =
            (!supports_storage_buffers(&device)).then(|| device.limits().max_texture_dimension_2d);
        let metadata_array = Array::new(
            &device,
            Some("unlit3d::mesh_metadata"),
            <MeshMetadata as const_shader_layout::ShaderLayout>::SIZE.get(),
            1,
            array_max_dimension,
        );
        let metadata_buf = graph.insert(metadata_array.handle(), None);
        let joints_array = Array::new(
            &device,
            Some("unlit3d::pose::joints"),
            <JointMatrix as const_shader_layout::ShaderLayout>::SIZE.get(),
            1,
            array_max_dimension,
        );
        let joints_buf = graph.insert(joints_array.handle(), None);
        let morph_weights_array = Array::new(
            &device,
            Some("unlit3d::pose::morph_weights"),
            size_of::<f32>() as u64,
            1,
            array_max_dimension,
        );
        let morph_weights_buf = graph.insert(morph_weights_array.handle(), None);
        // The morph displacements are pooled rather than indexed: a mesh's
        // slice is as long as it has vertices, so it takes a sub-allocation
        // like a vertex stream rather than a fixed slot like an entry.
        let morph_deltas_pool = ArrayPool::new(
            &device,
            "unlit3d::morph_deltas",
            size_of::<f32>() as u64,
            1,
            array_max_dimension,
        );
        let morph_deltas_buf = graph.insert(morph_deltas_pool.handle(), None);

        // Initial upload of camera and globals.
        queue.write_buffer(
            graph.get(&camera_buf).expect("just inserted"),
            0,
            View::from_clip_from_world(glam::Mat4::IDENTITY, glam::Vec3::ZERO).as_bytes(),
        );
        queue.write_buffer(
            graph.get(&globals_buf).expect("just inserted"),
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
        let index_pool_id = graph.insert(index_pool.buffer().clone(), None);
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
            metadata_array,
            metadata_buf,
            joints_array,
            joints_buf,
            morph_weights_array,
            morph_weights_buf,
            morph_deltas_pool,
            morph_deltas_buf,
            morph_deltas_dirty: false,
            globals,
            metadata: Vec::new(),
            free_metadata: Vec::new(),
            metadata_dirty: false,
            packed_joints: Vec::new(),
            packed_morph_weights: Vec::new(),
            camera_staging: StagingBuffer::new(),
            globals_staging: StagingBuffer::new(),
            index_pool,
            index_pool_id,
            vertex_pool,
            vertex_pool_ids: HashMap::new(),
            pipelines: Vec::new(),
            families: Vec::new(),
            family_ids: TypeIdHashMap::default(),
            visible_meshes_cache: Vec::new(),
            visible_cache: Vec::new(),
            family_instance_cache: Vec::new(),
            pipeline_handle_cache: Vec::new(),
            entry_handle_cache: HashMap::new(),
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
    pub fn device(&self, world: &World) -> wgpu::Device {
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
    pub fn queue(&self, world: &World) -> wgpu::Queue {
        world
            .get::<wgpu::Queue>(self.context.queue)
            .expect("the context's queue resource exists")
            .clone()
    }

    /// What the device can do beyond the WebGPU baseline.
    ///
    /// Captured from the adapter when the context was spawned, because the
    /// device cannot report it; see
    /// [`DeviceCapabilities`].
    ///
    /// # Panics
    ///
    /// If the context's capabilities resource is gone.
    pub fn capabilities(&self, world: &World) -> DeviceCapabilities {
        *world
            .get::<DeviceCapabilities>(self.context.capabilities)
            .expect("the context's capabilities resource exists")
    }

    /// Whether the device supports a non-zero `base_vertex` on an indexed draw.
    ///
    /// Shorthand for the one capability the mesh upload asks about.
    pub(crate) fn base_vertex(&self, world: &World) -> bool {
        self.capabilities(world).base_vertex()
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
        world: &'w World,
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
    /// unlit shader and a caller own alike. The key type is the family
    /// identity: entities draw with it when they carry a
    /// [GpuRenderPipeline](crate::components::GpuRenderPipeline) of that type, and registering a
    /// second family for the same key type is a programming error.
    ///
    /// A family compiles nothing on registration: its concrete pipelines are
    /// built lazily, the first time a draw resolves a variant, and are
    /// appended to the source pipeline list in resolution order.
    ///
    /// [`MeshSourceUnlitExt::register_unlit_family`](crate::unlit::MeshSourceUnlitExt::register_unlit_family) is the built-in unlit family; a
    /// caller registers their own by supplying the key whose
    /// [RenderPipelineKey::variant] resolves a draw into a variant, the
    /// [RenderPipelineFactory] that describes the compiled result, and the
    /// [InstanceData] that writes its per-instance records. A family with no
    /// per-instance state passes `()`.
    ///
    /// # Panics
    ///
    /// If a family is already registered for `K`.
    pub fn register_family<K, F, I>(&mut self, world: &World, factory: F, instances: I)
    where
        K: RenderPipelineKey + 'static,
        F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>
            + 'static,
        I: InstanceData,
    {
        let key = TypeId::of::<K>();
        assert!(
            !self.family_ids.contains_key(&key),
            "a pipeline family is already registered for this key type"
        );
        let id = FamilyId::new(self.families.len() as u32);
        self.families.push(Box::new(Family::<K, F, I>::new(
            &self.device(world),
            factory,
            instances,
        )));
        self.family_ids.insert(key, id);
    }

    // -- mesh allocation -------------------------------------------------------

    /// Upload vertex and index data and return a [GpuMesh] handle.
    ///
    /// The source assumes no vertex layout: `vertex_buffers` lists exactly the
    /// buffers a draw binds, each tagged with the slot the pipeline's vertex
    /// state declares, so a mesh may carry any combination of attributes in
    /// any format. The pipeline specializes on the layout.
    ///
    /// The mesh's [`Aabb`](crate::bounds::Aabb) is recorded in the source's
    /// mesh-metadata array, which the next built scene uploads.
    ///
    /// [`MeshSourceUnlitExt::allocate_unlit_mesh`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_mesh) is the helper that builds the
    /// compressed layout the built-in unlit shader expects.
    ///
    /// # Panics
    ///
    /// If `count` is zero for a non-empty draw, or if the index format does
    /// not match the packed data.
    pub fn allocate_mesh(&mut self, world: &World, desc: MeshDesc) -> GpuMesh {
        let metadata = MeshMetadata {
            aabb_center: desc.aabb.center,
            aabb_half_extents: desc.aabb.half_extents,
            ..Default::default()
        };
        // The raw API says nothing about the mesh's channels, so the mesh is
        // not known to carry joints; a caller that uploads a skinned layout
        // goes through `allocate_unlit_mesh`, which does know. It owns its
        // buffers whole, so it is drawn from vertex zero and morphs nothing.
        self.allocate_mesh_with_metadata(world, desc, metadata, false, 0)
    }

    /// Upload a mesh together with the full metadata entry it owns.
    ///
    /// The entry is appended to the CPU-side array and reaches the GPU on the
    /// next built scene; the returned handle names its index. `vertex_offset`
    /// is the draw addressing the entry carries; the morph count is the one the
    /// mesh's own displacements declare.
    ///
    /// `skinned` records whether the mesh's position stream carries joints, so
    /// a skinned draw can be required to name the pose entity it deforms by.
    pub(crate) fn allocate_mesh_with_metadata(
        &mut self,
        world: &World,
        desc: MeshDesc,
        mut metadata: MeshMetadata,
        skinned: bool,
        vertex_offset: u32,
    ) -> GpuMesh {
        let MeshDesc {
            vertex_buffers,
            index_buffer,
            count,
            indexed,
            aabb,
            bind_group,
            morph_deltas,
        } = desc;

        // The vertex offset is part of the entry the mesh owns, so the shader
        // reaches it through the same index the decode parameters come from.
        // It maps `@builtin(vertex_index)` back to this mesh's own vertex
        // ordinal for the morph displacements.
        metadata.vertex_offset = vertex_offset;

        // The parts, capped like the description they come from: a mesh cannot
        // have more vertex buffers than a pass can bind.
        let mut buffers = ArrayVec::<ResourceId<wgpu::Buffer>, MAX_VERTEX_BUFFERS>::new();
        let mut vertex_slots =
            ArrayVec::<(u32, ResourceId<wgpu::Buffer>), MAX_VERTEX_BUFFERS>::new();
        let mut layouts = Vec::with_capacity(vertex_buffers.len());
        for desc in vertex_buffers {
            // The mesh's virtual root is built from it, so the buffer lives
            // exactly as long as the root does.
            let id = Self::graph(world, self.context).insert(desc.buffer, None);
            vertex_slots.push((desc.slot, id.clone()));
            // The layout is owned by the mesh so a family can key on it
            // without reading the description again.
            layouts.push((
                desc.slot,
                VertexBufferLayoutDesc {
                    array_stride: desc.array_stride,
                    step_mode: desc.step_mode,
                    attributes: desc.attributes,
                },
            ));
            buffers.push(id);
        }
        let vertex_layout = VertexLayout::new(layouts);

        let index_buffer = index_buffer.map(|(buffer, format)| {
            // Weak for the same reason as the vertex buffers.
            let id = Self::graph(world, self.context).insert(buffer, None);
            (id, format)
        });

        // The morph displacements are this mesh's geometry, but they go into
        // the frame-wide pool rather than a resource of their own, so the mesh
        // holds an element range like it holds a vertex range. The range's
        // offset is what its metadata entry names.
        //
        // The joint matrices and the morph weights are *not* here. They are
        // per-instance pose state: the frame's global group binds them, so two
        // instances of one mesh can deform differently.
        let (morph_target_count, morph_deltas_offset, morph_deltas_allocation) = match morph_deltas
        {
            Some(morph) => {
                let range = self
                    .morph_deltas_pool
                    .allocate(&self.device(world), morph.deltas.len() as u32)
                    .expect("the morph-displacement pool grows");
                self.morph_deltas_pool.write(range, morph.deltas.as_bytes());
                self.morph_deltas_dirty = true;
                (morph.target_count, range.offset(), Some(range.allocation()))
            }
            None => (0, 0, None),
        };

        // A bind group the caller supplied — a custom pipeline's per-mesh data
        // — hangs under the mesh's root, so giving the mesh up frees it with
        // the rest of its parts. The pools a mesh's own data lives in
        // are deliberately not dependencies: a pooled resource changes when its
        // pool grows, and a dependency would rebuild every group of every mesh
        // sharing the pool for nothing.
        let bind_group_id =
            bind_group.map(|bind_group| Self::graph(world, self.context).insert(bind_group, None));

        // The entry is owned whether or not the pipeline reads it: a draw that
        // reads no metadata simply leaves the index unused. A slot a removed
        // mesh held is reused, so the array stays as dense as the meshes that
        // are still alive.
        //
        // The morph count and displacement offset the addressing carries are
        // this mesh's own: no other values describe the displacements it
        // pooled.
        metadata.morph_count = morph_target_count;
        metadata.morph_deltas_offset = morph_deltas_offset;
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

        // The virtual root below is the one node that keeps the parts alive:
        // each part is registered on its own and the root is built from all of
        // them, so dropping the last id to the root leaves the parts nothing
        // else holds for the next `maintain` to collect. Depending on the
        // parts is also what makes a replaced part mark the root dirty.
        let mut graph = Self::graph(world, self.context);
        let root = graph.insert(Virtual, None);
        for id in &buffers {
            graph.add_dependency(&root, id);
        }
        if let Some((id, _format)) = &index_buffer {
            graph.add_dependency(&root, id);
        }
        if let Some(id) = &bind_group_id {
            graph.add_dependency(&root, id);
        }
        drop(graph);

        GpuMesh {
            parts: Rc::new(MeshParts {
                root,
                vertex_buffers: vertex_slots,
                index_buffer,
                bind_group_id,
                vertex_allocation: None,
                index_allocation: None,
                morph_deltas_allocation,
                metadata_index,
            }),
            vertex_layout,
            count,
            first: 0,
            base_vertex: 0,
            indexed,
            aabb,
            morph_targets: morph_target_count,
            skinned,
        }
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
        world: &World,
        texture: wgpu::Texture,
    ) -> (ResourceId<wgpu::Texture>, ResourceId<TextureView>) {
        let mut graph = Self::graph(world, self.context);
        let texture_id = graph.insert(texture, None);
        let view = TextureExt::create_view(
            graph.get(&texture_id).expect("texture exists"),
            &wgpu::TextureViewDescriptor::default(),
        );
        let view_id = graph.insert(view, None);
        graph.add_dependency(&view_id, &texture_id);
        (texture_id, view_id)
    }

    /// Create a sampler with `descriptor` — or the default one when `None` —
    /// insert it into the resource graph and return its id.
    pub fn register_sampler(
        &mut self,
        world: &World,
        descriptor: Option<wgpu::SamplerDescriptor<'_>>,
    ) -> ResourceId<wgpu::Sampler> {
        let sampler = self
            .device(world)
            .create_sampler(&descriptor.unwrap_or_default());
        Self::graph(world, self.context).insert(sampler, None)
    }

    /// Build a material bind group with `build` and return its [GpuMaterial]
    /// handle.
    ///
    /// Only the bind group is created here: the resources it reads are the
    /// caller's, already in the resource graph and named in `dependencies` so
    /// replacing one marks the group dirty. `build` owns the layout the group is
    /// built against and is called again by [`Self::maintain`] whenever a
    /// dependency is replaced, so it has to read whatever it binds back out of
    /// the graph rather than capture a handle. The layout is the material layout
    /// of the pipeline the material is for —
    /// [`UnlitVariant::bind_group_layouts`](unlit_wgpu::pipeline::UnlitVariant::bind_group_layouts)
    /// for the built-in shader, or the one a custom pipeline registered.
    ///
    /// # Panics
    ///
    /// If a resource `build` reads is not in the graph.
    pub fn allocate_material(
        &mut self,
        world: &World,
        build: impl Fn(&ResourceGraph) -> wgpu::BindGroup + 'static,
        dependencies: impl IntoIterator<Item = ResourceId>,
    ) -> GpuMaterial {
        // The first build is eager: the node has to hold a bind group from the
        // start, and only the rebuilds that follow are deferred to `maintain`.
        let bind_group = {
            let graph = Self::graph(world, self.context);
            build(&graph)
        };
        let mut graph = Self::graph(world, self.context);
        let rebuild = Rebuild::new(move |graph| Resource::BindGroup(build(graph)));
        let bind_group_id = graph.insert(bind_group, Some(rebuild));
        for dependency in dependencies {
            graph.add_dependency(&bind_group_id, &dependency);
        }

        GpuMaterial { bind_group_id }
    }

    /// Free `mesh` and every resource it owns, and drop its mesh-metadata
    /// entry.
    ///
    /// The whole mesh hangs off its virtual root: the parts are held by the
    /// root and by nothing else, so dropping `mesh` — the last holder of the
    /// root — makes them collectable, and the next
    /// [`Self::maintain`](MeshSource::maintain) frees the root, its bind group
    /// and the per-mesh uniform that only fed that bind group. A mesh uploaded
    /// through [`allocate_unlit_mesh`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_mesh) keeps
    /// its vertices and indices in pools the source shares between meshes;
    /// those allocations are handed back here, and the pool buffers outlive the
    /// mesh. A mesh uploaded with [`allocate_mesh`](MeshSource::allocate_mesh)
    /// owns its buffers, and they die with it.
    ///
    /// Its metadata slot is freed and reused by a mesh allocated later, so
    /// removing meshes does not grow the array a long-lived source uploads.
    /// The next built scene uploads the array the shader reads. Removing a mesh
    /// does not shrink the metadata buffer: it grows to the largest array it
    /// has ever held and stays there.
    ///
    /// Any other [`GpuMesh`] value sharing this mesh's parts keeps it alive:
    /// the parts are reference-counted, so a caller that cloned the handle
    /// before calling this still reads a live mesh.
    pub fn remove_mesh(&mut self, mesh: GpuMesh) {
        if let Some(vertex) = mesh.parts.vertex_allocation {
            self.vertex_pool.release(vertex);
        }
        if let Some(index) = mesh.parts.index_allocation {
            self.index_pool.release(index);
        }
        if let Some(deltas) = mesh.parts.morph_deltas_allocation {
            self.morph_deltas_pool.release(deltas);
        }

        let metadata_index = mesh.parts.metadata_index;
        let emptied = self.metadata.get_mut(metadata_index as usize);
        if let Some(entry) = emptied {
            *entry = MeshMetadata::default();
            self.free_metadata.push(metadata_index);
            self.metadata_dirty = true;
        }
    }

    // -- skin and morph arrays --------------------------------------------------

    /// Grow the frame's joint-matrix array if the packed data outgrew it, and
    /// upload it through the frame's encoder.
    ///
    /// Like the metadata array the resource is replaced only when it is too
    /// small, so a steady frame rewrites in place and rebuilds no bind group.
    fn upload_joints(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder) {
        let device = self.device(world);
        let needed = self.packed_joints.len().max(1) as u64;
        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::pose::joints",
            &mut self.joints_array,
            &self.joints_buf,
            &device,
            needed,
        );
        self.joints_array
            .upload(&device, encoder, self.packed_joints.as_bytes());
    }

    /// Grow the frame's morph-displacement array if a mesh outgrew it, and
    /// upload the pooled slices through the frame's encoder.
    ///
    /// Allocating or removing a mesh marks the array dirty, so the upload
    /// lands in the next frame together with the draws that read it. The
    /// resource is
    /// replaced only when it is too small, so a steady scene rewrites in place
    /// and rebuilds no bind group.
    fn upload_morph_deltas(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder) {
        if !self.morph_deltas_dirty {
            return;
        }
        self.morph_deltas_dirty = false;
        let device = self.device(world);
        if self.morph_deltas_pool.upload(&device, encoder) {
            // A replaced array invalidates every global group bound to it; the
            // next maintain rebuilds them.
            Self::graph(world, self.context)
                .replace(&self.morph_deltas_buf, self.morph_deltas_pool.handle())
                .expect("the morph-displacement array's node exists");
        }
    }

    /// Grow the frame's morph-weight array if the packed data outgrew it, and
    /// upload it through the frame's encoder.
    fn upload_morph_weights(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder) {
        let device = self.device(world);
        let needed = self.packed_morph_weights.len().max(1) as u64;
        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::pose::morph_weights",
            &mut self.morph_weights_array,
            &self.morph_weights_buf,
            &device,
            needed,
        );
        self.morph_weights_array
            .upload(&device, encoder, self.packed_morph_weights.as_bytes());
    }

    // -- internal helpers ------------------------------------------------------

    /// The ids of the source's global buffers, as a factory and a rebuild
    /// recipe see them.
    fn global_resources(&self) -> GlobalResources {
        GlobalResources {
            camera: self.camera_buf.clone(),
            globals: self.globals_buf.clone(),
            metadata: self.metadata_buf.clone(),
            joints: self.joints_buf.clone(),
            morph_weights: self.morph_weights_buf.clone(),
            morph_deltas: self.morph_deltas_buf.clone(),
        }
    }

    /// Grow `array` to hold at least `capacity` elements, publishing the new
    /// resource into the graph when it had to be replaced.
    ///
    /// A replaced array leaves every bind group built from the old one stale;
    /// the graph marks their nodes dirty here, and the next
    /// [`Self::maintain`] rebuilds them. A no-op when the array already has the
    /// room, so a steady frame replaces nothing and marks nothing.
    fn grow_array_to_fit(
        world: &World,
        ctx: RenderContext,
        label: &str,
        array: &mut Array,
        node: &ResourceId<ArrayHandle>,
        device: &wgpu::Device,
        needed: u64,
    ) {
        // Grow only when the array is genuinely too small: growing by a factor
        // on every call would inflate the capacity without bound.
        if needed <= array.capacity() {
            return;
        }
        let capacity = u64::from(grown_capacity(array.capacity() as u32, needed as u32));
        if !array.grow_to(device, Some(label), capacity) {
            return;
        }
        Self::graph(world, ctx)
            .replace(node, array.handle())
            .expect("the array's node exists");
    }

    /// Bring the resource graph up to date for this frame.
    ///
    /// Collects the resources nothing holds any more, then rebuilds every
    /// dirty node whose recipe the graph holds — the global bind groups among
    /// them. A built scene runs it once, at the point before the frame reads
    /// any of those groups; [`RenderContext::maintain_scope`] is how a build
    /// states that point without a call at each of its returns.
    pub fn maintain(&mut self, world: &World) {
        Self::graph(world, self.context).maintain();
    }

    /// Upload the camera uniform, staging the bytes through the frame's
    /// encoder.
    fn upload_camera(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder, view: &View) {
        let buffer = Self::graph(world, self.context)
            .get(&self.camera_buf)
            .expect("camera buffer exists")
            .clone();
        self.camera_staging
            .write(&self.device(world), encoder, &buffer, 0, view.as_bytes());
    }

    /// Upload the frame-globals uniform, staging the bytes through the frame's
    /// encoder.
    fn upload_globals(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder) {
        let buffer = Self::graph(world, self.context)
            .get(&self.globals_buf)
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

    /// Pack and upload every family's per-instance data for this frame.
    ///
    /// Each family owns one instance stream. The records are written in
    /// visible order, so the entry an instance index names is the entry whose
    /// draw range covers that index.
    fn pack_family_instances(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder) {
        let device = self.device(world);
        // Split the borrows: each entry is written while its family's stream
        // grows, and while the two frame arrays a family may append to grow.
        let Self {
            families,
            visible_cache,
            packed_joints,
            packed_morph_weights,
            ..
        } = self;
        packed_joints.clear();
        packed_morph_weights.clear();
        for family in families.iter_mut() {
            family.begin_instances();
        }
        for entry in visible_cache.iter_mut() {
            let mesh = &entry.mesh;
            let mut context = InstanceContext::new(
                world,
                mesh.entity,
                mesh.world_from_local,
                packed_joints,
                packed_morph_weights,
            );
            entry.instance_index = families[entry.family.as_usize()].push_instance(&mut context);
        }
        for family in families.iter_mut() {
            family.upload_instances(&device, encoder);
        }
    }

    /// Grow the metadata buffer if the array outgrew it, and upload the array
    /// through the frame's encoder if it changed.
    ///
    /// Allocating or removing a mesh marks the array dirty, so the upload lands
    /// in the next frame together with the draws that read it. The storage
    /// buffer is recreated only when the array outgrows it, so a steady scene
    /// rewrites in place and rebuilds no bind group.
    fn upload_metadata(&mut self, world: &World, encoder: &mut wgpu::CommandEncoder) {
        if !self.metadata_dirty {
            return;
        }
        self.metadata_dirty = false;

        let device = self.device(world);
        let needed = self.metadata.len().max(1) as u64;
        // Replace rather than resize: neither a buffer nor a texture has a size
        // that can change.
        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::mesh_metadata",
            &mut self.metadata_array,
            &self.metadata_buf,
            &device,
            needed,
        );
        self.metadata_array
            .upload(&device, encoder, self.metadata.as_bytes());
    }

    /// The graph node of the vertex pool's buffer for `layout`, creating it if
    /// the pool has never been asked for the layout before.
    ///
    /// The node is strong: the source owns the pool, no mesh does. See
    /// [`MeshSource::sync_pool_node`] for why nothing may depend on it.
    pub(crate) fn vertex_node(
        &mut self,
        world: &World,
        layout: &VertexBufferLayoutDesc,
    ) -> ResourceId<wgpu::Buffer> {
        if let Some(id) = self.vertex_pool_ids.get(layout) {
            return id.clone();
        }
        let buffer = self
            .vertex_pool
            .buffer(layout)
            .expect("the layout was allocated a buffer")
            .clone();
        let id = Self::graph(world, self.context).insert(buffer, None);
        self.vertex_pool_ids.insert(layout.clone(), id.clone());
        id
    }

    /// Point the stream node `id` at the vertex pool's buffer for `layout`.
    ///
    /// The pool hands out a new buffer when it grows, so the node has to
    /// follow it.
    pub(crate) fn sync_vertex_node(
        &mut self,
        world: &World,
        id: &ResourceId<wgpu::Buffer>,
        layout: &VertexBufferLayoutDesc,
    ) {
        let buffer = self
            .vertex_pool
            .buffer(layout)
            .expect("the layout has a buffer");
        let mut graph = Self::graph(world, self.context);
        if graph.get(id) != Some(buffer) {
            graph
                .replace(id, buffer.clone())
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
    /// Nothing depends on a pool node — the built-in variants bind no
    /// per-mesh resource at all — so replacing one marks nothing else dirty. A
    /// dependency added onto a pool buffer makes every grow rebuild it, which
    /// is why there must not be one.
    pub(crate) fn sync_pool_node(
        pool: &BufferPool,
        id: &ResourceId<wgpu::Buffer>,
        graph: &mut ResourceGraph,
    ) {
        if graph.get(id) != Some(pool.buffer()) {
            graph
                .replace(id, pool.buffer().clone())
                .expect("a pool node exists");
        }
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
    fn collect_and_sort_visible(&mut self, world: &World, camera: &Camera, surface: SurfaceKey) {
        let resources = self.global_resources();
        let device = self.device(world);

        // Split the borrows: the families mutate the pipeline list and the
        // resource graph through the register closure, while the caches are
        // disjoint fields.
        let (families, meshes, visible) = (
            &mut self.families,
            &mut self.visible_meshes_cache,
            &mut self.visible_cache,
        );
        let (pipelines, buffers) = (&mut self.pipelines, resources.clone());
        let ctx = self.context;
        let mut graph = Self::graph(world, ctx);
        let mut register = |desc| register_concrete(pipelines, &mut graph, buffers.clone(), desc);

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
            resources,
        );
    }

    /// Assemble the draws of the frame's visible set into `self.scene`.
    ///
    /// The handles every draw names — the bind groups, the vertex and index
    /// buffers, the pipeline — are cloned out of the resource graph into reused
    /// scratch tables first, so assembling touches the graph once per distinct
    /// draw state and the assembled scene owns everything it names.
    ///
    /// A scene usually draws many entities per mesh, and every one of them
    /// names the same handles. The visible set therefore carries a
    /// [`DrawHandlesKey`] by value rather than the handles themselves, and the
    /// expensive part — reading the mesh component and cloning its buffers out
    /// of the graph — happens once per distinct key instead of once per entity.
    fn assemble_frame(&mut self, world: &World) {
        let mut handles = std::mem::take(&mut self.entry_handle_cache);
        handles.clear();
        {
            profiling::scope!("mesh_source.assemble.handles");
            let graph = Self::graph(world, self.context);
            for entry in &self.visible_cache {
                let key = entry.handles_key.clone();
                if handles.contains_key(&key) {
                    continue;
                }

                let mesh = world
                    .get::<GpuMesh>(entry.mesh.entity)
                    .expect("visible entity has GpuMesh");

                let mesh_bg = mesh
                    .parts
                    .bind_group_id
                    .as_ref()
                    .map(|id| graph.get(id).expect("mesh bind group exists").clone());

                let material_bg = key
                    .material
                    .as_ref()
                    .map(|id| graph.get(id).expect("material bind group exists").clone());

                let mut vertex_buffers = ArrayVec::new();
                for (slot, buffer) in &mesh.parts.vertex_buffers {
                    let buffer = graph
                        .get(buffer)
                        .expect("mesh vertex buffer exists")
                        .clone();
                    // A mesh binds its vertex buffers whole; the draw's range
                    // is what picks the mesh's slice out of the pool.
                    vertex_buffers.push((*slot, buffer.clone(), 0..buffer.size()));
                }

                let index_buffer = mesh.parts.index_buffer.as_ref().map(|(buffer, format)| {
                    (
                        graph.get(buffer).expect("mesh index buffer exists").clone(),
                        *format,
                    )
                });

                handles.insert(
                    key,
                    EntryHandles {
                        mesh_bg,
                        material_bg,
                        vertex_buffers,
                        index_buffer,
                    },
                );
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
            pipeline_handles.extend(self.pipelines.iter().map(|registered| {
                RenderPipelineHandles {
                    pipeline: registered.pipeline.clone(),
                    global: registered
                        .global
                        .as_ref()
                        .map(|global| graph.get(&global.id).expect("global group exists").clone()),
                }
            }));
        }

        // One instance-stream binding per family, indexed the same way
        // [`MeshSource::families`] is. A family with no instance stream —
        // stride zero — carries `None`, and its draws bind no instance slot.
        let mut family_instances = std::mem::take(&mut self.family_instance_cache);
        family_instances.clear();
        family_instances.extend(self.families.iter().map(|family| family.instance_binding()));

        // The scene keeps its allocation across frames: `clear` drops the
        // draws but not the buffer behind them.
        self.scene.clear();
        profiling::scope!("mesh_source.assemble.draws");
        assemble_scene(
            &mut self.scene,
            &self.visible_cache,
            &pipeline_handles,
            &handles,
            &family_instances,
        );

        self.pipeline_handle_cache = pipeline_handles;
        self.entry_handle_cache = handles;
        self.family_instance_cache = family_instances;
    }
}

/// The camera a frame draws through: the first **active** [`Camera`] in
/// `world`, copied out of its cell so the caller's borrow of the world does
/// not block the accesses that follow.
///
/// Several cameras may live in one world — a caller switches between them by
/// toggling [`Camera::active`] rather than by respawning. `None` means no
/// camera is active, and the frame draws nothing.
fn frame_camera(world: &World) -> Option<Camera> {
    world
        .query::<&Camera>()
        .find(|(_, camera)| camera.active)
        .map(|(_, camera)| Camera {
            view_from_world: camera.view_from_world,
            clip_from_view: camera.clip_from_view,
            active: camera.active,
        })
}

impl FrameSource for MeshSource {
    fn build_scene(
        &mut self,
        world: &World,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Settle the graph on every way out of this build, the early returns
        // below included, rather than only on the path that draws. The pass
        // runs where this scope ends — see the explicit drop before the frame
        // is assembled — and on any return in between.
        let maintain = ctx.maintain_scope(world);
        profiling::scope!("mesh_source.build");
        // The scene is cleared on every path: a frame that records it must
        // never replay the previous frame's draws, and a source with nothing
        // to draw leaves it empty rather than absent.
        self.scene.clear();

        // A frame that draws still has to publish a changed metadata array, but
        // one that does not draw can leave it for the next frame.
        {
            profiling::scope!("mesh_source.metadata.upload");
            self.upload_metadata(world, encoder);
        }

        // The frame is drawn from the first *active* camera in `world`.
        let camera = frame_camera(world);
        let Some(camera) = camera else {
            // Even a frame that draws nothing has to settle the graph: meshes
            // allocated or removed since the last frame are waiting, and the
            // next frame's reads assume a maintained graph. Returning here
            // drops the scope, which is what settles it.
            return;
        };

        // Where in the target the frame's 3D content belongs, stated by the
        // frame loop. A loop that letterboxes — one keeping the content's
        // aspect on a differently-shaped target — puts the region here, and the
        // scene draws through it so the picture is scaled uniformly.
        //
        // The camera already carries the matching projection, since the frame
        // loop built it for the region's aspect, so the frustum this source
        // culls with follows the same shape and nothing is culled at the edges
        // the letterbox would have hidden.
        self.scene
            .set_viewport(frame_viewport(world).map(|viewport| viewport.0));

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
        {
            profiling::scope!("mesh_source.uniforms.upload");
            self.globals.time += self.globals.delta_time;
            self.globals.frame_count += 1;
            self.upload_globals(world, encoder);

            let view = View::new(
                camera.view_from_world,
                camera.clip_from_view,
                camera.position(),
            );
            self.upload_camera(world, encoder, &view);
        }

        // A rebuild has to happen before the global groups are read, and the
        // metadata upload above may have replaced the buffer one of them binds.
        // The maintain below, just before the frame is assembled, is what
        // rebuilds them; nothing else rebuilds on this path.

        // Collect, cull and sort the visible set in one pass. The cache keeps
        // its allocation between frames, so a steady scene allocates nothing.
        self.collect_and_sort_visible(world, &camera, surface);
        if self.visible_cache.is_empty() {
            return;
        }
        // Pack each family's instance data into its own stream and upload it.
        // A family that draws skinned or morphed meshes appends their joints
        // and weights to the frame's arrays here, while it writes its records;
        // the arrays are then uploaded, because the records that name slices of
        // them are what the draws read.
        {
            profiling::scope!("mesh_source.instances.upload");
            self.pack_family_instances(world, encoder);
        }
        {
            profiling::scope!("mesh_source.skin_morph.upload");
            self.upload_joints(world, encoder);
            self.upload_morph_weights(world, encoder);
            // The pool's upload replaces the array when it has to grow, which
            // marks every global group bound to it dirty for the maintain
            // below.
            self.upload_morph_deltas(world, encoder);
        }

        // Settle the graph for this frame: collect what nothing holds any more
        // and rebuild what the uploads above marked dirty. Dropping the scope
        // is the one pass, and it runs here — after every replacement and
        // before anything reads a bind group.
        {
            profiling::scope!("mesh_source.maintain");
            drop(maintain);
        }

        {
            profiling::scope!("mesh_source.assemble");
            self.assemble_frame(world);
        }
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
    use crate::components::{Transform, ZSortedDrawing};
    use crate::mesh::MorphDeltas;
    use crate::scene::DrawShape;
    use crate::source::{FrameTarget, set_frame_target, spawn_context};
    use crate::unlit::{MeshSourceUnlitExt, UnlitMeshDesc, UnlitPipeline, UnlitPipelineKey};
    use unlit_ecs::Entity;
    use unlit_wgpu::mesh::{MeshInstance, UvColorFlags};
    use unlit_wgpu::pipeline::{UnlitOptions, UnlitVertexChannels};
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
        key: UnlitPipelineKey,
        target: FrameTarget,
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
            self.source.allocate_unlit_mesh(
                &self.world,
                &self.key,
                UnlitMeshDesc {
                    positions: &[[0.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]],
                    uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                    colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                    indices: Some(&[0u32, 1, 2]),
                    ..Default::default()
                },
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

        /// An empty bind group, for a mesh that carries a group of its own.
        fn empty_bind_group(&self) -> wgpu::BindGroup {
            let layout = self.source.device(&self.world).create_bind_group_layout(
                &wgpu::BindGroupLayoutDescriptor {
                    label: Some("test::mesh::empty"),
                    entries: &[],
                },
            );
            self.source
                .device(&self.world)
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
        world: &World,
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
    fn bind_test_target(world: &mut World, ctx: RenderContext) -> FrameTarget {
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
            let color = graph.insert(
                TextureExt::create_view(&ft.color, &wgpu::TextureViewDescriptor::default()),
                None,
            );
            let depth = graph.insert(
                TextureExt::create_view(&ft.depth, &wgpu::TextureViewDescriptor::default()),
                None,
            );
            let attachments = RenderAttachments::from_views(
                graph.get(&color).cloned(),
                graph.get(&depth).cloned(),
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
        Camera {
            view_from_world: glam::camera::rh::view::look_at_mat4(
                eye,
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            ),
            clip_from_view: test_perspective(),
            active: true,
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

    /// A second policy that differs from the standard one in a field the
    /// pipeline is specialized on, so it resolves to its own variant.
    fn alternative_options(device: &wgpu::Device) -> UnlitOptions {
        let mut options = UnlitOptions::standard(device);
        options.srgb_to_linear_output = true;
        options
    }

    /// A triangle mesh with one morph target, for the tests that need the
    /// mesh group a morphing variant binds.
    fn morph_mesh(harness: &mut Harness) -> GpuMesh {
        let key = UnlitPipelineKey::new(UnlitOptions::standard(
            &harness.source.device(&harness.world),
        ));
        // One target displacing all three vertices by the same amount, packed
        // vertex-major: three components per vertex.
        let deltas = vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
        harness.source.allocate_unlit_mesh(
            &harness.world,
            &key,
            UnlitMeshDesc {
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
    /// id, ready for [`MeshSourceUnlitExt::allocate_unlit_material`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_material).
    fn test_material_resources(
        harness: &mut Harness,
    ) -> (ResourceId<TextureView>, ResourceId<wgpu::Sampler>) {
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

    #[test]
    fn a_frame_uses_the_first_active_camera() {
        let mut world = World::new();
        // Spawned first, so a selection that ignored `active` would pick this
        // one instead of the active camera below.
        let inactive = world.spawn((Camera {
            view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-1.0, 0.0, 0.0)),
            clip_from_view: glam::Mat4::IDENTITY,
            active: true,
        },));
        world
            .with_mut::<Camera, _>(inactive, |camera| camera.active = false)
            .expect("the entity carries a camera");
        let active = world.spawn((Camera {
            view_from_world: glam::Mat4::from_translation(glam::Vec3::new(-2.0, 0.0, 0.0)),
            clip_from_view: glam::Mat4::IDENTITY,
            active: true,
        },));

        let camera = frame_camera(&world).expect("a camera is active");
        assert_eq!(camera.position(), glam::Vec3::new(2.0, 0.0, 0.0));
        assert!(camera.active);

        // With every camera inactive, the frame has nothing to draw through.
        world
            .with_mut::<Camera, _>(active, |camera| camera.active = false)
            .expect("the entity carries a camera");
        assert!(frame_camera(&world).is_none());
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
        let mut world = World::new();
        let ctx = spawn_context(
            &mut world,
            device,
            queue,
            ResourceGraph::new(),
            DeviceCapabilities::default(),
        );
        let source = MeshSource::new(&world, ctx);

        // Nothing is privileged: pipelines arrive only through registration.
        assert!(source.pipelines.is_empty());
    }

    #[test]
    fn a_unlit_mesh_packs_the_channels_its_desc_carries() {
        let mut h = harness();
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&h.source.device(&h.world)));

        // The mesh packs the channels the caller passed, not the ones the key
        // used to declare: the options are pure policy now.
        let textured = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            UnlitMeshDesc {
                positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
        );
        let bare = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            UnlitMeshDesc {
                positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
        );
        let textured_channels = UnlitVertexChannels::for_vertex_layout(&textured.vertex_layout);
        assert!(textured_channels.uv_color.contains(UvColorFlags::UV));
        assert!(textured_channels.uv_color.contains(UvColorFlags::COLOR));
        let bare_channels = UnlitVertexChannels::for_vertex_layout(&bare.vertex_layout);
        assert!(!bare_channels.uv_color.contains(UvColorFlags::UV));
        assert!(!bare_channels.uv_color.contains(UvColorFlags::COLOR));

        // A material can be built for either: whether a draw samples it is the
        // draw's own answer — it binds the group or it does not.
        let (view, sampler) = test_material_resources(&mut h);
        let material = h
            .source
            .allocate_unlit_material(&h.world, &key, view, sampler);
        let ctx = h.source.context();
        assert!(
            MeshSource::graph(&h.world, ctx)
                .get(&material.bind_group_id)
                .is_some()
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
            .map(|registered| {
                registered
                    .global
                    .as_ref()
                    .expect("has a global group")
                    .id
                    .clone()
            })
            .collect();
        for (index, id) in ids.iter().enumerate() {
            assert!(
                MeshSource::graph(&h.world, ctx).get(id).is_some(),
                "pipeline {index}'s global group is in the graph"
            );
            // No two pipelines share one: each binds its own layout.
            assert!(!ids[..index].contains(id));
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

        let material =
            h.source
                .allocate_unlit_material(&h.world, &h.key, view.clone(), sampler.clone());
        let ctx = h.source.context();
        assert!(
            MeshSource::graph(&h.world, ctx)
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
        let before = MeshSource::graph(&h.world, ctx).len();
        let index_pool_free = h.source.index_pool.free_space();
        assert!(matches!(
            MeshSource::graph(&h.world, ctx).get(&mesh.parts.root),
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
        h.source.maintain(&h.world);

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
        // A caller-supplied bind group, which is the one part a mesh can own
        // outright: the built-in variants bind nothing per mesh, so their
        // meshes carry none.
        let bind_group = h.empty_bind_group();
        let mesh = h.source.allocate_mesh(
            &h.world,
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
        let dependencies: Vec<_> = MeshSource::graph(&h.world, ctx)
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
            MeshSource::graph(&h.world, ctx).get(&pool).is_some(),
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
        let material =
            h.source
                .allocate_unlit_material(&h.world, &h.key, view.clone(), sampler.clone());

        // Giving the handle up is the whole of it: an id is a strong
        // reference, so a clone kept here would keep the bind group alive and
        // the assertion below would have nothing to observe.
        let before = MeshSource::graph(&h.world, ctx).len();
        drop(material);
        h.source.maintain(&h.world);

        assert!(
            MeshSource::graph(&h.world, ctx).len() < before,
            "the bind group is gone"
        );
        // The view and sampler are the caller's, so giving up the material
        // that reads them leaves them alone.
        assert!(
            MeshSource::graph(&h.world, ctx).get(&view).is_some(),
            "the view stays"
        );
        assert!(
            MeshSource::graph(&h.world, ctx).get(&sampler).is_some(),
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
            MeshSource::graph(&h.world, ctx).get(&first_node),
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
            UnlitMeshDesc {
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
                MeshSource::graph(&h.world, ctx).get(first_id).is_some(),
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
                MeshSource::graph(&h.world, ctx).get(node),
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

    /// The depth an entry sorts by is measured at the geometry the mesh draws,
    /// not at the entity's origin: two meshes whose bounds sit on opposite
    /// sides of their own origin are equally deep by origin and only their
    /// bounds can order them.
    #[test]
    fn z_sorted_entries_sort_by_the_mesh_bounds_not_the_entity_origin() {
        let mut h = harness();
        let near = h.tri_mesh_at(1.5);
        let far = h.tri_mesh_at(-1.5);

        // Both entity origins are at the same depth; only the geometry differs.
        let translation = glam::Vec3::new(0.0, 0.0, 2.0);
        let near_entity = h.world.spawn((
            Transform {
                translation,
                ..Default::default()
            },
            near,
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));
        let far_entity = h.world.spawn((
            Transform {
                translation,
                ..Default::default()
            },
            far,
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

        // The far mesh is drawn first even though its entity origin is not
        // deeper than the other's.
        assert_eq!(drawn(&h.source), vec![far_entity, near_entity]);
        assert!(h.source.visible_cache[0].depth > h.source.visible_cache[1].depth);
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
        let entity = h.world.spawn((
            Transform {
                translation,
                ..Default::default()
            },
            mesh,
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

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
            view_from_world: glam::camera::rh::view::look_at_mat4(
                eye,
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            ),
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
        let far = h.tri_mesh_at(-1.5);

        // Both entity origins are at the same depth; only the geometry differs.
        let translation = glam::Vec3::new(0.0, 0.0, 2.0);
        let near_entity = h.world.spawn((
            Transform {
                translation,
                ..Default::default()
            },
            near,
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));
        let far_entity = h.world.spawn((
            Transform {
                translation,
                ..Default::default()
            },
            far,
            UnlitPipeline::new(h.key.clone()),
            ZSortedDrawing,
        ));

        let camera = test_orthographic_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        h.source
            .collect_and_sort_visible(&h.world, &camera, h.target.surface);

        assert_eq!(drawn(&h.source), vec![far_entity, near_entity]);
        assert!(h.source.visible_cache[0].depth > h.source.visible_cache[1].depth);
    }

    #[test]
    fn entities_are_drawn_in_pipeline_id_order() {
        let mut h = harness();
        // A second key whose specialized options differ, so it resolves to its
        // own concrete pipeline.
        let alternative_key =
            UnlitPipelineKey::new(alternative_options(&h.source.device(&h.world)));

        // Warm both variants, first key first, so their pipeline ids follow
        // the order they were resolved in.
        let standard_mesh = h.tri_mesh();
        let alternative_mesh = h.source.allocate_unlit_mesh(
            &h.world,
            &alternative_key,
            UnlitMeshDesc {
                positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
        );
        let surface = h.target.surface;
        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        let warm_standard = h.world.spawn((
            Transform::default(),
            standard_mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let warm_alternative = h.world.spawn((
            Transform::default(),
            alternative_mesh.clone(),
            UnlitPipeline::new(alternative_key.clone()),
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
            .find(|entry| entry.mesh.entity == warm_alternative)
            .expect("the uv-less entity is visible")
            .pipeline_id;
        assert!(first_id < second_id);
        assert!(h.world.despawn(warm_standard));
        assert!(h.world.despawn(warm_alternative));

        // Spawn the later-drawn entity first: draw order is the pipeline id,
        // not the spawn order.
        let second = h.world.spawn((
            Transform::default(),
            alternative_mesh,
            UnlitPipeline::new(alternative_key),
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
        let shared =
            h.source
                .allocate_unlit_material(&h.world, &h.key, view.clone(), sampler.clone());
        let (view, sampler) = test_material_resources(&mut h);
        let other =
            h.source
                .allocate_unlit_material(&h.world, &h.key, view.clone(), sampler.clone());
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
        other.vertex_layout = VertexLayout::new(layout);
        assert_ne!(other.vertex_layout, standard.vertex_layout);

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
        twin.vertex_layout = VertexLayout::new(layout);
        assert_ne!(twin.vertex_layout, base.vertex_layout);

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
        // No GpuRenderPipeline: the query filters it out.
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
        // The source starts with room for one entry and grows by 1.5x.
        assert_eq!(h.source.metadata_array.capacity(), 1);

        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(
            h.source.metadata_array.capacity(),
            1,
            "one entry fills the room"
        );

        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(
            h.source.metadata_array.capacity(),
            2,
            "a second entry outgrows room for one"
        );

        // A third and fourth entry outgrow the 1.5x capacity in turn.
        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(h.source.metadata_array.capacity(), 3);

        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(h.source.metadata_array.capacity(), 4);

        // A fifth entry outgrows four, so the buffer grows again.
        let before = MeshSource::graph(&h.world, ctx)
            .get(&h.source.metadata_buf)
            .cloned();
        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(
            h.source.metadata_array.capacity(),
            6,
            "growing past four reaches six (4 * 1.5)"
        );
        assert_ne!(
            MeshSource::graph(&h.world, ctx).get(&h.source.metadata_buf),
            before.as_ref(),
            "growth replaced the buffer"
        );

        // A sixth entry fits in six, so the buffer is left alone.
        let before = MeshSource::graph(&h.world, ctx)
            .get(&h.source.metadata_buf)
            .cloned();
        h.tri_mesh();
        let encoder = h.encoder();
        h.source.upload_metadata(&h.world, &mut { encoder });
        assert_eq!(h.source.metadata_array.capacity(), 6, "six entries fit");
        assert_eq!(
            MeshSource::graph(&h.world, ctx).get(&h.source.metadata_buf),
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
        h.source.remove_mesh(mesh);
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
        let key = UnlitPipelineKey::new(alternative_options(&h.source.device(&h.world)));
        let mesh = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            UnlitMeshDesc {
                positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
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
        let key = UnlitPipelineKey::new(alternative_options(&h.source.device(&h.world)));
        // Both meshes are indexed and share the pool's index buffer, so what
        // tells their draws apart is the slice each one names.
        let first = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            UnlitMeshDesc {
                positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                uvs: Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
                colors: Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
        );
        let second = h.source.allocate_unlit_mesh(
            &h.world,
            &key,
            UnlitMeshDesc {
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
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        h.world
            .spawn((Transform::default(), first, UnlitPipeline::new(key.clone())));
        h.world
            .spawn((Transform::default(), second, UnlitPipeline::new(key)));

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);
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
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        for i in 0..3 {
            h.world.spawn((
                Transform {
                    translation: glam::Vec3::new(i as f32, 0.0, 0.0),
                    ..Default::default()
                },
                mesh.clone(),
                UnlitPipeline::new(h.key.clone()),
            ));
        }

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);

        assert_eq!(h.source.scene().draws.len(), 1, "one draw for all three");
        let range = &h.source.scene().draws[0].range;
        let instances = match range {
            DrawRange::Indexed { instances, .. } | DrawRange::Vertices { instances, .. } => {
                instances
            }
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
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        for i in 0..3 {
            h.world.spawn((
                Transform {
                    translation: glam::Vec3::new(i as f32, 0.0, 0.0),
                    ..Default::default()
                },
                mesh.clone(),
                UnlitPipeline::new(h.key.clone()),
            ));
        }

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);

        assert_eq!(h.source.visible_cache.len(), 3, "all three are visible");
        assert_eq!(
            h.source.entry_handle_cache.len(),
            1,
            "one mesh, one draw state, one handle set"
        );
        assert_eq!(
            h.source.scene().draws.len(),
            1,
            "and therefore one merged instanced draw"
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
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        let key = h.key.clone();
        h.world
            .spawn((Transform::default(), first, UnlitPipeline::new(key.clone())));
        h.world
            .spawn((Transform::default(), second, UnlitPipeline::new(key)));

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);

        assert_eq!(
            h.source.scene().draws.len(),
            2,
            "different meshes stay one draw each"
        );
    }

    /// A translucent entry never merges, with another translucent entry or with
    /// an opaque one: its draw order is its blend order, so the source must
    /// not let the instance stream reorder it.
    #[test]
    fn z_sorted_entries_never_merge() {
        let mut h = harness();
        let mesh = h.tri_mesh();
        h.world
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        // Two translucent copies of the same mesh, drawn back to front.
        let key = h.key.clone();
        h.world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 1.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            ZSortedDrawing,
        ));
        h.world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 2.0),
                ..Default::default()
            },
            mesh,
            UnlitPipeline::new(key),
            ZSortedDrawing,
        ));

        let ctx = h.source.context();
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);

        assert_eq!(
            h.source.scene().draws.len(),
            2,
            "translucent entries keep their own draw order"
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
            .spawn((test_camera(glam::Vec3::new(0.0, 0.0, 5.0)),));
        let drawn = h.world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(h.key.clone()),
        ));
        let mut encoder = h.encoder();
        h.source.build_scene(&h.world, ctx, &mut encoder);

        let before = MeshSource::graph(&h.world, ctx).len();
        // Read the ids out before the source goes: once it is dropped there is
        // no `h.source` left to name them.
        let camera_buf = h.source.camera_buf.clone();
        let index_pool_id = h.source.index_pool_id.clone();

        // The camera/globals/metadata buffers, the index pool and the global
        // bind group are the source's own; the mesh's parts hang off its root.
        // Despawning the drawn entity and giving the local handle up leaves the
        // mesh root unheld, and dropping the source gives up everything it
        // registered itself.
        h.world.despawn(drawn);
        drop(mesh);
        drop(h.source);
        MeshSource::graph(&h.world, ctx).maintain();
        let after = MeshSource::graph(&h.world, ctx).len();

        assert!(
            after < before,
            "dropping the source must shrink the graph: {after} was {before}"
        );
        // The two ids this test still holds are strong references of their
        // own, so what they name outlives the source that registered it. That
        // is the point of a counted handle: the resource goes when the last id
        // does, not when its creator does.
        assert!(
            MeshSource::graph(&h.world, ctx).get(&camera_buf).is_some(),
            "a held id keeps the source's camera buffer alive"
        );
        assert!(
            MeshSource::graph(&h.world, ctx)
                .get(&index_pool_id)
                .is_some(),
            "a held id keeps the index pool alive"
        );

        // Giving those ids up is what lets the graph collect them.
        drop(camera_buf);
        drop(index_pool_id);
        MeshSource::graph(&h.world, ctx).maintain();
        assert!(
            MeshSource::graph(&h.world, ctx).len() < after,
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
}
