//! The built-in mesh source: the 3D half of a frame.
//!
//! [`MeshSource`] owns everything the mesh path needs: the resource-graph nodes
//! a mesh is uploaded into, the pipeline families an entity's key resolves
//! through, the per-frame caches the draw list is built from, and the
//! [`Scene`] those draws are recorded from. It is an ordinary
//! [`FrameSource`] — the frame driver knows nothing about meshes, so a
//! caller's own source runs beside it with no less privilege.
//!
//! The state the source owns is grouped by what it is for, and each group
//! lives in its own submodule next to the code that maintains it:
//! `FrameArrays` carries the metadata entry every mesh owns and the pose data
//! a draw reads, [`Lights`] the clustered lights and the uniforms describing
//! them, `MeshPools` the vertex and index pools a pooled mesh shares, and
//! `FrameCaches` the allocations one frame reuses.
//!
//! The behaviour lives beside them: `pooled` allocates a mesh that shares the
//! source's pools, `geometry` allocates meshes that own their buffers together
//! with the textures and materials both kinds draw with, `arrays` and
//! `lighting` upload the frame-level arrays, and `families` registers pipeline
//! families, packs per-frame instances and assembles the draw list.
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

    /// The arrays that carry the metadata entry every mesh owns and the pose
    /// data a draw reads: the joint matrices, the morph weights and the pooled
    /// morph displacements.
    pub(crate) arrays: FrameArrays,
    /// The frame's lights and the cluster assignment they are read through.
    pub lights: Lights,

    /// The vertex and index pools a pooled mesh shares with every other.
    pub(crate) pools: MeshPools,

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
    /// The allocations this frame reuses: the visible set, the interned draw
    /// handles and the per-family bindings.
    pub(crate) caches: FrameCaches,
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
        // it does not.
        let array_max_dimension =
            (!supports_storage_buffers(&device)).then(|| device.limits().max_texture_dimension_2d);
        let arrays = FrameArrays::new(&device, graph, array_max_dimension);

        let lights = Lights::new(&device, graph, array_max_dimension);

        let pools = MeshPools::new(&device, graph);

        Self {
            context: ctx,
            arrays,
            lights,
            pools,
            pipelines: Vec::new(),
            families: Vec::new(),
            family_ids: TypeIdHashMap::default(),
            caches: FrameCaches::new(),
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

    /// The ids of the source's global arrays, as a factory and a rebuild
    /// recipe see them. The camera, the globals and the view are not among
    /// them: the frame holds those.
    pub(super) fn global_resources(&self) -> GlobalResources {
        GlobalResources {
            metadata: self.arrays.metadata_buf.clone(),
            joints: self.arrays.joints_buf.clone(),
            morph_weights: self.arrays.morph_weights_buf.clone(),
            morph_deltas: self.arrays.morph_deltas_buf.clone(),
            lights: self.lights.lights_buf.clone(),
            light_cells: self.lights.light_cells_buf.clone(),
            light_vertices: self.lights.light_vertices_buf.clone(),
            cluster: self.lights.cluster_buf.clone(),
            lighting: self.lights.lighting_buf.clone(),
        }
    }
}

mod arrays;
mod caches;
mod families;
mod geometry;
mod lighting;
mod mesh_pools;
mod pooled;

#[cfg(test)]
mod tests;

pub use self::lighting::Lights;

use self::caches::FrameCaches;
use self::mesh_pools::MeshPools;
pub use self::pooled::{PooledMeshDesc, PooledStream};

use self::arrays::{FrameArrays, grow_array_to_fit};

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
            self.arrays.upload_metadata(world, ctx, encoder);
        }

        // The frame draws through the camera the pass states, or the first
        // *active* camera the source reaches when it states none — a fallback
        // that follows row order, so a caller who cares states the camera on
        // the pass.
        let camera = match info.camera {
            Some(camera_entity) => world
                .query_one::<&Camera>(camera_entity)
                .ok()
                .map(|mut row| {
                    let camera = row.get().clone();
                    (camera_entity, camera)
                })
                .or_else(|| frame_camera(world)),
            None => frame_camera(world),
        };
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
        if self.caches.visible_cache.is_empty() {
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
            self.arrays.upload_joints(world, ctx, encoder);
            self.arrays.upload_morph_weights(world, ctx, encoder);
            // The pool's upload replaces the array when it has to grow, which
            // marks every global group bound to it dirty for the maintain
            // below.
            self.arrays.upload_morph_deltas(world, ctx, encoder);
        }
        {
            // The lights are uploaded on every frame that gets this far, not
            // only on frames that draw a lit variant: a family serves whatever
            // the scene presents each frame, so which frames those are is not
            // known ahead of the upload and it cannot wait for the first one.
            profiling::scope!("mesh_source.lights.upload");
            self.lights
                .upload(world, ctx, encoder, &camera, (info.width, info.height));
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
