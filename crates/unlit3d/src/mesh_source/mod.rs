//! The built-in mesh source: the 3D half of a frame.
//!
//! [`MeshSource`] owns everything the mesh path needs: the resource-graph nodes
//! a mesh is uploaded into, the pipeline families an entity's key resolves
//! through, the per-frame caches the draw list is built from, and the
//! [`Scene`] those draws are recorded from. It is an ordinary
//! [`FrameSource`] — the frame driver knows nothing about meshes, so a
//! caller's own source runs beside it with no less privilege.
//!
//! The implementation lives in submodules by responsibility:
//! `geometry` allocates meshes, textures and materials into the pools,
//! `arrays` uploads the frame-level joint, morph and metadata arrays,
//! `lighting` uploads the clustered lights, and `families` registers
//! pipeline families, packs per-frame instances and assembles the draw list.
//!
//! The GPU state it draws with lives in the world, addressed by the
//! [`RenderContext`] it was created with: every setup-time method takes
//! `&World` and fetches the device, the queue and the resource graph
//! itself, so the source never holds a borrow of them between calls.

use arrayvec::ArrayVec;
use core::any::TypeId;
use hashbrown::HashMap;
use std::rc::Rc;

use unlit_ecs::{Scope, TypeIdHashMap};
use unlit_wgpu::array_pool::ArrayPool;
use unlit_wgpu::buffer_pool::BufferPool;
use unlit_wgpu::globals::Camera as CameraUniform;
use unlit_wgpu::light::{
    CELL_WORDS, ClusterConfig, ClusterUniform, DirectionalGpu, LightClusters, LightGpu,
    LightingUniform, MAX_DIRECTIONAL_LIGHTS, PackedLight,
};
use unlit_wgpu::mesh::{JointMatrix, MeshMetadata};
use unlit_wgpu::pipeline::supports_storage_buffers;
use unlit_wgpu::resources::{ResHandle, Resource, ResourceGraph, TextureExt, TextureView, Virtual};
use unlit_wgpu::scene::{MAX_VERTEX_BUFFERS, Scene};
use unlit_wgpu::specialize::{PipelineVariant, SurfaceKey, VertexBufferLayoutDesc, VertexLayout};
use unlit_wgpu::staging::StagingBuffer;
use unlit_wgpu::texel_array::{Array, ArrayHandle};
use unlit_wgpu::vertex_pool::VertexStreamPool;
use zerocopy::IntoBytes;

use crate::components::{Camera, GpuMaterial, GpuMesh, MeshParts, frame_camera};
use crate::culling::VisibleMesh;
use crate::global_uniforms::{CameraKey, FrameUniforms, UniformGroups, ViewKey};
use crate::mesh::MeshDesc;
use crate::pass::PassInfo;
use crate::pipeline::{
    GlobalResources, InstanceContext, InstanceData, Rebuild, RegisteredGlobal,
    RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineId, RenderPipelineKey,
};
use crate::scene::{
    AnyFamily, DrawHandlesKey, EntryHandles, Family, FamilyId, RenderPipelineHandles, SceneFrame,
    VisibleEntry, assemble_scene, collect_and_sort_visible,
};
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};
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
/// borrow the disjoint fields this needs.
fn register_concrete(
    pipelines: &mut Vec<RegisteredPipeline>,
    desc: RegisteredRenderPipeline,
) -> RenderPipelineId {
    // The material and mesh layouts describe a pipeline's binding interface,
    // but they do not outlive registration: the source builds those groups
    // from the family it registered, and wgpu already holds the pipeline's own
    // layout internally.
    let RegisteredRenderPipeline {
        pipeline, global, ..
    } = desc;

    // The recipe is kept rather than run: its global bind group binds the
    // frame's camera and view buffers, which are only known once a draw names
    // them, so the groups are built when the frame is assembled.
    let global = global.map(|recipe| RegisteredGlobal {
        recipe,
        groups: UniformGroups::new(),
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

///
/// It owns the mesh pools, the pipeline families, the per-frame caches and the
/// [`Scene`] the frame's draws are assembled into, and it fetches the device,
/// the queue and the resource graph from the world through the
/// [`RenderContext`] it was created with. Register the families it draws with
/// — [`MeshSource::register_lit_family`](crate::mesh_source::MeshSource::register_lit_family) for the built-in lit shader — and
/// spawn it with [`Source::new`](crate::source::Source::new).
pub struct MeshSource {
    /// The world addresses of the frame's GPU state, kept so every entry point
    /// fetches the device, the queue and the graph for itself instead of
    /// holding a borrow of them between calls.
    pub(crate) context: RenderContext,

    /// The mesh-metadata array, held in whichever resource the device reads.
    metadata_array: Array,
    /// Resource id of the mesh-metadata array.
    metadata_buf: ResHandle<ArrayHandle>,
    /// The frame's joint-matrix array, held in whichever resource the device
    /// reads.
    joints_array: Array,
    /// Resource id of the frame's joint-matrix array.
    joints_buf: ResHandle<ArrayHandle>,
    /// The frame's morph-weight array, held in whichever resource the device
    /// reads.
    morph_weights_array: Array,
    /// Resource id of the frame's morph-weight array.
    morph_weights_buf: ResHandle<ArrayHandle>,
    /// The frame's morph-displacement array, pooling every morphed mesh's
    /// deltas into one resource.
    ///
    /// A mesh's displacements are its own geometry, but one array holds them
    /// for the frame and the mesh names its slice through its metadata entry's
    /// `morph_deltas_offset`. That is what leaves the mesh group nothing to
    /// bind.
    morph_deltas_pool: ArrayPool,
    /// Resource id of the frame's morph-displacement array.
    morph_deltas_buf: ResHandle<ArrayHandle>,
    /// The frame's lights, held in whichever resource the device reads.
    lights_array: Array,
    /// Resource id of the frame's light array.
    lights_buf: ResHandle<ArrayHandle>,
    /// The frame's cluster cells, held in whichever resource the device reads.
    light_cells_array: Array,
    /// Resource id of the frame's cluster-cell array.
    light_cells_buf: ResHandle<ArrayHandle>,
    /// The frame's light vertices: the corner pool every quadrilateral light
    /// appends to.
    light_vertices_array: Array,
    /// Resource id of the frame's light-vertex array.
    light_vertices_buf: ResHandle<ArrayHandle>,
    /// Resource id of the cluster uniform buffer.
    cluster_buf: ResHandle<wgpu::Buffer>,
    /// Resource id of the frame-wide lighting uniform buffer.
    lighting_buf: ResHandle<wgpu::Buffer>,
    /// The cluster grid this frame's lights are assigned to.
    ///
    /// The source clusters every frame that has an active camera and a visible
    /// mesh, whether or not it draws a lit variant, since a frame's camera
    /// moves and its lights move with it. Its near and far planes are the
    /// clustering's own: a [Camera] carries a projection, not the extents that
    /// projection was built from.
    pub clustering: ClusterConfig,
    /// Reused per-frame light records, ranked by importance by the clusterer.
    packed_lights: Vec<PackedLight>,
    /// Reused per-frame cluster assignment.
    light_clusters: LightClusters,
    /// Reused per-frame polygon-vertex pool, which a quadrilateral light
    /// appends its corners to.
    light_vertices: Vec<glam::Vec4>,
    /// Reused per-frame light records, in the order the clusterer ranked them.
    light_records: Vec<LightGpu>,
    /// The frame-wide lighting uniform, rebuilt and uploaded on every frame
    /// that reaches the upload.
    lighting: LightingUniform,
    /// Staging buffer for the cluster uniform, reused across frames.
    cluster_staging: StagingBuffer,
    /// Staging buffer for the lighting uniform, reused across frames.
    lighting_staging: StagingBuffer,
    /// Whether a mesh's displacements were written into the pool since the
    /// array was last uploaded.
    ///
    /// Allocating or removing a mesh marks it; the next
    /// [build](FrameSource::build_scene) uploads the array then, so the upload
    /// always lands in the same encoder as the draws that read it.
    morph_deltas_dirty: bool,

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
    ///
    /// The camera, globals and view uniforms are not among them: the frame's
    /// [GlobalUniforms](crate::global_uniforms::GlobalUniforms) stages those.

    /// The pool every mesh uploaded through
    /// [`MeshSource::allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh) keeps its indices in, as one large
    /// buffer shared by every indexed mesh.
    ///
    /// A mesh names it through [`MeshParts::index_buffer`], which is why the
    /// buffer is a node in the resource graph and has to be replaced there
    /// when the pool grows; see [`MeshSource::sync_pool_node`].
    pub(crate) index_pool: BufferPool,
    /// The graph node of [`MeshSource::index_pool`]'s buffer.
    pub(crate) index_pool_id: ResHandle<wgpu::Buffer>,
    /// The pool every mesh uploaded through
    /// [`MeshSource::allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh) keeps its vertices in: one large
    /// buffer per vertex layout, so meshes that share a layout share a buffer,
    /// and one element allocation covers every stream of a mesh at the same
    /// element index.
    pub(crate) vertex_pool: VertexStreamPool,
    /// The graph node of each of [`MeshSource::vertex_pool`]'s buffers, by the
    /// layout the buffer is shaped for.
    vertex_pool_ids: HashMap<VertexBufferLayoutDesc, ResHandle<wgpu::Buffer>>,

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
}
impl MeshSource {
    /// Create the source and the GPU resources the mesh path shares.
    ///
    /// The camera and globals uniforms, the mesh-metadata storage buffer and
    /// the index and vertex pools every uploaded mesh draws out of are inserted
    /// into the context's resource graph. The source starts with no families:
    /// register the ones you draw with through
    /// [`MeshSource::register_family`] — for the built-in lit shader,
    /// [`MeshSource::register_lit_family`](crate::mesh_source::MeshSource::register_lit_family) does it for you. Registration
    /// compiles nothing; a family's first concrete pipeline is built the first
    /// time an entity that uses it is drawn.
    ///
    /// # Panics
    ///
    /// If `ctx` does not name the world's device and resource graph.
    pub fn new(world: &Scope<'_>, ctx: RenderContext) -> Self {
        let device = ctx
            .device(world)
            .expect("the context names the world's device");
        let mut resource_graph_row = ctx.graph(world);

        let graph = resource_graph_row.get();

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

        // The frame's lights and the cluster assignment they are read through.
        // The light array starts with room for one entry, the cells with one
        // grid's worth, and the vertices with one quadrilateral's; each grows on
        // the first built scene that needs more.
        let lights_array = Array::new(
            &device,
            Some("unlit3d::lights"),
            <LightGpu as const_shader_layout::ShaderLayout>::SIZE.get(),
            1,
            array_max_dimension,
        );
        let lights_buf = graph.insert(lights_array.handle(), None);
        let light_cells_array = Array::new(
            &device,
            Some("unlit3d::light_cells"),
            (CELL_WORDS * size_of::<u32>()) as u64,
            1,
            array_max_dimension,
        );
        let light_cells_buf = graph.insert(light_cells_array.handle(), None);
        let light_vertices_array = Array::new(
            &device,
            Some("unlit3d::light_vertices"),
            size_of::<glam::Vec4>() as u64,
            1,
            array_max_dimension,
        );
        let light_vertices_buf = graph.insert(light_vertices_array.handle(), None);

        // The cluster and lighting uniforms. A shader that reads one before the
        // first lit frame finds unlit bytes rather than whatever the allocation
        // held; a lit pipeline only exists once a frame has uploaded them, so
        // nothing reads an unbuilt one.
        let clustering = ClusterConfig::DEFAULT;
        let cluster_buf = graph.insert(
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::cluster"),
                size: size_of::<ClusterUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            None,
        );
        let lighting_buf = graph.insert(
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::lighting"),
                size: size_of::<LightingUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            None,
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
            metadata_array,
            metadata_buf,
            joints_array,
            joints_buf,
            morph_weights_array,
            morph_weights_buf,
            morph_deltas_pool,
            morph_deltas_buf,
            lights_array,
            lights_buf,
            light_cells_array,
            light_cells_buf,
            light_vertices_array,
            light_vertices_buf,
            cluster_buf,
            lighting_buf,
            clustering,
            packed_lights: Vec::new(),
            light_clusters: LightClusters::default(),
            light_vertices: Vec::new(),
            light_records: Vec::new(),
            lighting: LightingUniform {
                directionals: [DirectionalGpu::default(); MAX_DIRECTIONAL_LIGHTS],
                ambient: glam::Vec4::ZERO,
            },
            cluster_staging: StagingBuffer::new(),
            lighting_staging: StagingBuffer::new(),
            morph_deltas_dirty: false,
            metadata: Vec::new(),
            free_metadata: Vec::new(),
            metadata_dirty: false,
            packed_joints: Vec::new(),
            packed_morph_weights: Vec::new(),
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
        }
    }
    /// The world addresses this source draws through.
    ///
    /// Setup-time code that needs the resource graph reaches it with
    /// `source.context().graph(&view)`, which is an ordinary ECS access: the
    /// graph and the source live in different cells.
    pub fn context(&self) -> RenderContext {
        self.context
    }

    /// The device the frame is drawn with, cloned out of `world`.
    ///
    /// # Panics
    ///
    /// If the context's device resource is gone.
    pub fn device(&self, world: &Scope<'_>) -> wgpu::Device {
        self.context
            .device(world)
            .expect("the context's device resource exists")
    }

    /// The queue the frame is submitted to, cloned out of `world`.
    ///
    /// # Panics
    ///
    /// If the context's queue resource is gone.
    pub fn queue(&self, world: &Scope<'_>) -> wgpu::Queue {
        self.context
            .queue(world)
            .expect("the context's queue resource exists")
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
    pub fn capabilities(&self, world: &Scope<'_>) -> DeviceCapabilities {
        self.context
            .capabilities(world)
            .expect("the context's capabilities resource exists")
    }

    /// Bring the resource graph up to date for this frame.
    ///
    /// Collects the resources nothing holds any more, then rebuilds every
    /// dirty node whose recipe the graph holds — the global bind groups among
    /// them. A built scene runs it once, at the point before the frame reads
    /// any of those groups; [`RenderContext::maintain_scope`] is how a build
    /// states that point without a call at each of its returns.
    pub fn maintain(&mut self, world: &Scope<'_>) {
        self.context.graph(world).get().maintain();
    }
}

mod arrays;
mod families;
mod geometry;
mod lighting;
#[cfg(test)]
mod tests;

impl FrameSource for MeshSource {
    fn build_scene(
        &mut self,
        world: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Settle the graph on every way out of this build, the early returns
        // below included, rather than only on the path that draws. The pass
        // runs where this scope ends — see the explicit drop before the frame
        // is assembled — and on any return in between.
        let maintain = ctx.maintain_scope(*world);
        profiling::scope!("mesh_source.build");
        // `scene` arrives cleared: the driver owns it, so a frame that records
        // it never replays the previous frame's draws, and an early return
        // below leaves it empty rather than stale.

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
        scene.viewport = info.viewport;

        // The pass states the target the frame is specialized for, so nothing
        // has to be read back out of the world and a stale target can never be
        // picked up: the driver builds a source only for a pass that has
        // attachments.
        let surface = info.surface;

        // The camera and view records the frame's registry holds for this draw.
        // The frame globals are the renderer's to advance; a source that wrote
        // them would write them once per build, and a frame with more than one
        // pass would stage them twice before either pass records.
        let (camera_entity, camera) = camera;
        let uniforms = {
            profiling::scope!("mesh_source.uniforms");
            // The viewport the pass draws through, or the whole attachment
            // when it draws through all of it: either way it is the region a
            // shader measuring in pixels has to divide by.
            let (viewport_width, viewport_height) = info
                .viewport
                .map_or((info.width as f32, info.height as f32), |viewport| {
                    (viewport.width, viewport.height)
                });
            let record = CameraUniform::new(
                camera.view_from_world,
                camera.clip_from_view,
                camera.position(),
            );
            let mut graph_holder = ctx.graph(world);
            let mut uniforms_holder = ctx.uniforms(world);
            uniforms_holder.get().uniforms(
                &self.device(world),
                graph_holder.get(),
                encoder,
                CameraKey::Entity(camera_entity),
                &record,
                ViewKey::new(viewport_width, viewport_height),
            )
        };

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
        {
            // The lights are uploaded on every frame that gets this far, not
            // only on frames that draw a lit variant: a family serves whatever
            // the scene presents each frame, so which frames those are is not
            // known ahead of the upload and it cannot wait for the first one.
            profiling::scope!("mesh_source.lights.upload");
            self.upload_lights(world, encoder, &camera, (info.width, info.height));
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
            self.assemble_frame(world, scene, &uniforms);
        }
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::MESH
    }
}
