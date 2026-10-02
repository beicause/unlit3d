//! The frame's render-pipeline abstraction.
//!
//! Every type here names a *render* pipeline rather than the compute kind, and
//! says so: [RegisteredRenderPipeline], [RenderPipelineKey],
//! [RenderPipelineId], [RenderPipelineFactory] and
//! [GpuRenderPipeline](crate::components::GpuRenderPipeline).
//!
//! A [RegisteredRenderPipeline] is everything the renderer needs to draw with a
//! wgpu render pipeline: the pipeline itself and -- for a pipeline that reads
//! the source's own camera, globals or metadata buffers -- a way to rebuild
//! its global bind group when those buffers change. Nothing here is specific
//! to the built-in unlit shader: the unlit family is registered through the
//! same [register_family](crate::mesh_source::MeshSource::register_family) a
//! caller's own family uses, and supplies its own factory like anyone else.
//!
//! # Render pipeline keys and families
//!
//! A [GpuRenderPipeline](crate::components::GpuRenderPipeline) component does not name a compiled pipeline. It carries a
//! [RenderPipelineKey], the entity's request for one family's variant: which
//! concrete pipeline an entity needs depends on the frame's render target and
//! on the mesh's vertex layout, neither of which is known when the entity is
//! spawned. A *family* closes that gap. It pairs a [Variants](unlit_wgpu::specialize::Variants) cache with a
//! [RenderPipelineFactory], queries the world for the entities that carry its
//! key type, and resolves each to a concrete pipeline. The renderer registers
//! every family under the [TypeId](core::any::TypeId) of its key type;
//! [crate::scene] drives them all once per frame. A family's variant is
//! compiled and registered the first time a key is seen, and later frames
//! reuse it.
//!
//! The entity, not the renderer, chooses its variant: a [RenderPipelineKey]
//! reports -- through [RenderPipelineKey::variant] -- the full, hashable
//! description of the pipeline its entity needs, derived from the entity's
//! own options and from the [DrawContext] the frame hands it. The context is
//! the frame's answer to everything an entity cannot know before it has a
//! mesh: the target, the mesh's vertex layout and index format, and whether a
//! material group is bound. Because the key derives that description rather
//! than carrying a base descriptor, one family serves entities that differ in
//! material or target policy, and the cache key is canonical by construction.
//!
//! Geometry is described by [crate::mesh::MeshDesc], which lists vertex buffers
//! tagged with the slot a pipeline expects them in and carries the layout each
//! buffer has. The renderer assumes no vertex layout, so a mesh can carry any
//! combination of attributes and a family can specialize on it at draw time.

use unlit_ecs::{Entity, World};
use unlit_wgpu::resources::ResourceId;
use unlit_wgpu::specialize::{PipelineVariant, SpecializedPipeline, SurfaceKey};
use unlit_wgpu::texel_array::ArrayHandle;

use crate::components::{GpuMaterial, GpuMesh};

pub use unlit_wgpu::resources::Rebuild;

/// The ids of the source's global arrays and buffers.
///
/// These are the resources every pipeline can rely on the renderer keeping up
/// to date: the camera uniform, the frame globals, the mesh-metadata array and
/// the frame's two pose arrays.
///
/// A factory hands these ids to the [Rebuild] it returns, and the recipe reads
/// the current resource out of the graph each time it runs. That indirection is
/// what makes rebinding lazy: when the source replaces a buffer, its readers
/// only find out at the next maintain, and a [Rebuild] that reads the id never
/// holds a stale buffer handle.
///
/// The three arrays are [`ArrayHandle`]s rather than buffers because which
/// resource holds them follows from the device: a device with storage buffers
/// holds each in one, a device without them -- WebGL2 -- holds the same bytes in
/// a texture. A pipeline that binds them asks the handle for its binding
/// resource and never has to know which it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalResources {
    /// The camera uniform buffer.
    pub camera: ResourceId<wgpu::Buffer>,
    /// The frame-globals uniform buffer.
    pub globals: ResourceId<wgpu::Buffer>,
    /// The mesh-metadata array.
    pub metadata: ResourceId<ArrayHandle>,
    /// The frame's joint matrices: every visible skinned instance's joints,
    /// one array for the whole frame.
    pub joints: ResourceId<ArrayHandle>,
    /// The frame's morph weights: every visible morphed instance's weights, one
    /// array for the whole frame.
    pub morph_weights: ResourceId<ArrayHandle>,
    /// The frame's morph displacements: every morphed mesh's per-vertex
    /// displacements, one array for the whole frame. A mesh names its slice
    /// through its metadata entry's `morph_deltas_offset`.
    pub morph_deltas: ResourceId<ArrayHandle>,
}

impl GlobalResources {
    /// Declare that the group behind `group` was built from every one of these
    /// resources, so replacing any of them marks it dirty.
    ///
    /// A global bind group reads all of them, so it depends on all of them.
    /// They are added one at a time because the uniforms and the arrays are
    /// different kinds.
    pub(crate) fn declare_dependencies(
        &self,
        graph: &mut unlit_wgpu::resources::ResourceGraph,
        group: &ResourceId<wgpu::BindGroup>,
    ) {
        graph.add_dependency(group, &self.camera);
        graph.add_dependency(group, &self.globals);
        graph.add_dependency(group, &self.metadata);
        graph.add_dependency(group, &self.joints);
        graph.add_dependency(group, &self.morph_weights);
        graph.add_dependency(group, &self.morph_deltas);
    }
}

/// A pipeline and the global bind group it draws with.
///
/// This is what a [RenderPipelineFactory] returns and what the renderer
/// registers. Every concrete pipeline in the renderer is described this way,
/// the built-in unlit ones included.
pub struct RegisteredRenderPipeline {
    /// The compiled render pipeline.
    pub pipeline: wgpu::RenderPipeline,

    /// How to rebuild the bind group bound at
    /// [GLOBAL_GROUP](unlit_wgpu::pipeline::GLOBAL_GROUP) when the source's
    /// buffers change.
    ///
    /// None for a pipeline that binds nothing at that index -- a shader
    /// with no uniform or storage inputs, say.
    pub global: Option<Rebuild>,
}

impl core::fmt::Debug for RegisteredRenderPipeline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegisteredRenderPipeline")
            .field("pipeline", &self.pipeline)
            .field("global", &self.global)
            .finish()
    }
}

/// The resource id a registered pipeline's global group lives under.
#[derive(Clone)]
pub(crate) struct RegisteredGlobal {
    /// The id in the source's resource graph.
    pub(crate) id: ResourceId<wgpu::BindGroup>,
}

/// What a [RenderPipelineFactory] may read from the source.
pub struct FamilyContext<'a> {
    /// The device the pipeline is compiled on.
    pub device: &'a wgpu::Device,
    /// The ids of the source's global buffers, for a pipeline that binds them.
    pub resources: GlobalResources,
}

/// The pipeline an entity draws with, as the entity's own key describes it.
///
/// A [GpuRenderPipeline](crate::components::GpuRenderPipeline) component carries a key of this type. It selects the family
/// the entity draws with -- the family registered for this key type -- and
/// derives, from the entity's options and the frame's [DrawContext], the full
/// description of the variant it needs. Because the key derives that
/// description rather than reporting a separate base descriptor, the variant
/// it returns is itself the cache key: equal variants compile to equal
/// pipelines, and no canonicalization step is needed.
pub trait RenderPipelineKey: 'static {
    /// The full, hashable description of the pipeline this key resolves to.
    type Variant: PipelineVariant<wgpu::RenderPipeline>;

    /// The variant this key needs for the draw described by `draw`.
    ///
    /// Called once per visible entity per frame, and the result is hashed to
    /// find the compiled pipeline, so it should be cheap and derive every
    /// field the compiled pipeline depends on.
    fn variant(&self, draw: &DrawContext<'_>) -> Self::Variant;
}

/// Everything about a draw that an entity's [RenderPipelineKey] cannot know
/// before the frame resolves it.
///
/// A key is a component, spawned before it has a mesh; the concrete pipeline it
/// needs, though, depends on the mesh's vertex layout and index format, on the
/// frame's render target, and on whether the draw binds a material group. The
/// frame fills this context in and hands it to [RenderPipelineKey::variant], so
/// the entity can derive its variant from what the frame actually draws with.
///
/// Every field is read-only: a key derives its variant from the context, never
/// mutates it.
pub struct DrawContext<'a> {
    /// The frame's render target.
    pub surface: SurfaceKey,
    /// The device the pipeline is compiled on.
    pub device: &'a wgpu::Device,
    /// The world the entity is drawn from, for per-entity components a variant
    /// depends on -- an alpha cutoff, say.
    pub world: &'a World,
    /// The entity being drawn.
    pub entity: Entity,
    /// The mesh the entity is drawn with.
    pub mesh: &'a GpuMesh,
    /// The material the draw binds, or `None` when it binds no material group.
    pub material: Option<&'a GpuMaterial>,
}

/// A concrete pipeline's position in a source's own pipeline list.
///
/// A lower value draws before a higher one. Opaque so the index is only ever
/// compared with another [RenderPipelineId], never a plain integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderPipelineId(u32);

impl RenderPipelineId {
    /// The id for the pipeline registered at `index`.
    pub(crate) fn new(index: u32) -> Self {
        Self(index)
    }

    /// The id as a `usize`, for indexing the source's pipeline list.
    pub(crate) fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Turns a specialized pipeline into the [RegisteredRenderPipeline] the renderer
/// registers.
///
/// The type parameter is the descriptor the family specializes, so a factory
/// reads the layouts a variant declares from the descriptor the pipeline was
/// compiled from.
pub trait RenderPipelineFactory<D> {
    /// The description of the pipeline for `value`, as the renderer should
    /// register it.
    fn descriptor(
        &self,
        context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, D>,
    ) -> RegisteredRenderPipeline;
}
