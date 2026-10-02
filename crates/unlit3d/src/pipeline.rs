//! The frame's render-pipeline abstraction.
//!
//! Every type here names a *render* pipeline rather than the compute kind, and
//! says so: [RegisteredRenderPipeline], [RenderPipelineKey],
//! [RenderPipelineId], [RenderPipelineFactory] and
//! [GpuRenderPipeline](crate::components::GpuRenderPipeline).
//!
//! A [RegisteredRenderPipeline] is everything the renderer needs to draw with a
//! wgpu render pipeline: the pipeline itself, the bind-group layouts its
//! draws agree with, and -- for a pipeline that reads the source's own
//! camera, globals or metadata buffers -- a way to rebuild its global bind
//! group when those buffers change. Nothing here is specific to the built-in
//! unlit shader: the unlit family is registered through the same
//! [MeshSource::register_family](crate::mesh_source::MeshSource::register_family) a caller's
//! own family uses, and supplies its own factory like anyone else.
//!
//! # Render pipeline keys and families
//!
//! A [GpuRenderPipeline](crate::components::GpuRenderPipeline) component does not name a compiled pipeline. It carries a
//! [RenderPipelineKey], the entity's request for one family's variant: which
//! concrete pipeline an entity needs depends on the frame's render target and
//! on the mesh's vertex layout, neither of which is known when the entity is
//! spawned. A *family* closes that gap. It pairs a [Variants](unlit_wgpu::specialize::Variants) cache with a
//! [Specializer](unlit_wgpu::specialize::Specializer) and a [RenderPipelineFactory], queries the world for the entities
//! that carry its key type, and resolves each to a concrete pipeline. The
//! renderer registers every family under the [TypeId](core::any::TypeId) of
//! its key type; [crate::scene] drives them all once per frame. A family's
//! variant is compiled and registered the first time a key is seen, and later
//! frames reuse it.
//!
//! The entity, not the renderer, chooses its base descriptor: a
//! [RenderPipelineKey] reports the blueprint ([RenderPipelineKey::base_descriptor]) its
//! variants start from, so one family can draw entities whose base options
//! differ. The blueprint is supplied to [Variants::specialize](unlit_wgpu::specialize::Variants::specialize) lazily and only
//! on a cache miss.
//!
//! Geometry is described by [crate::mesh::MeshDesc], which lists vertex buffers
//! tagged with the slot a pipeline expects them in and carries the layout each
//! buffer has. The renderer assumes no vertex layout, so a mesh can carry any
//! combination of attributes and a family can specialize on it at draw time.

use core::hash::Hash;

use unlit_wgpu::resources::ResourceId;
use unlit_wgpu::specialize::{PipelineDescriptor, SpecializedPipeline, SurfaceKey, VertexLayout};
use unlit_wgpu::texel_array::ArrayHandle;

use crate::components::GpuMesh;

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
/// holds each in one, a device without them — WebGL2 — holds the same bytes in
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

/// A pipeline and the layouts its draws agree with.
///
/// This is what a [RenderPipelineFactory] returns and what the renderer registers.
/// Every concrete pipeline in the renderer is described this way, the built-in
/// unlit ones included.
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

    /// The layout a material bind group must be built from, bound at
    /// [MATERIAL_GROUP](unlit_wgpu::pipeline::MATERIAL_GROUP).
    ///
    /// None for a pipeline whose draws bind no material group.
    pub material_layout: Option<wgpu::BindGroupLayout>,

    /// The layout a mesh bind group must be built from, bound at
    /// [MESH_GROUP](unlit_wgpu::pipeline::MESH_GROUP).
    ///
    /// None for a pipeline whose draws bind no per-mesh group.
    pub mesh_layout: Option<wgpu::BindGroupLayout>,
}

impl core::fmt::Debug for RegisteredRenderPipeline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegisteredRenderPipeline")
            .field("pipeline", &self.pipeline)
            .field("global", &self.global)
            .field("material_layout", &self.material_layout)
            .field("mesh_layout", &self.mesh_layout)
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

/// The base descriptor an entity's pipeline variants start from.
///
/// A [GpuRenderPipeline](crate::components::GpuRenderPipeline) component carries a key of this type. It selects the family
/// the entity draws with -- the family registered for this key type -- and
/// supplies the blueprint that family's
/// [Specializer](unlit_wgpu::specialize::Specializer) rewrites into the
/// concrete descriptor. Because the key carries the base, one family can serve
/// entities that begin from different descriptors; because it is the component
/// itself, the renderer never hands out a family handle.
pub trait RenderPipelineKey: Clone + Hash + Eq + 'static {
    /// The descriptor this key's variants are specialized from.
    type Descriptor: PipelineDescriptor<wgpu::RenderPipeline>;

    /// The blueprint this key's variant is specialized from. Called only when
    /// the key's variant is compiled for the first time.
    fn base_descriptor(&self) -> Self::Descriptor;
}

/// Everything that can change which concrete pipeline an entity needs beyond
/// the entity's own [RenderPipelineKey].
///
/// The mesh's vertex layout is held as a [VertexLayout], a shared handle with
/// its hash precomputed, because this key is rebuilt and hashed once per
/// visible entity per frame: the inline layout it replaces was the frame's
/// single largest cost once a scene had many entities sharing few meshes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DrawKey {
    /// The frame's render target.
    pub surface: SurfaceKey,
    /// The mesh's vertex layout, slot by slot.
    pub vertex_buffers: VertexLayout,
    /// The format of the mesh's index buffer, or `None` when the mesh draws
    /// its vertices in order without one.
    ///
    /// A strip topology's pipeline has to declare the index width its draw
    /// binds, and only the mesh knows it: the source picks the narrowest
    /// format the mesh's vertex count fits, and on a device without
    /// `base_vertex` it can widen a mesh's indices while baking in the pool
    /// offset. Carrying the resolved format here is what lets a family derive
    /// the pipeline's `strip_index_format` from the draw rather than ask the
    /// caller to predict it.
    pub index_format: Option<wgpu::IndexFormat>,
    /// Whether the draw binds a material group at [`MATERIAL_GROUP`].
    ///
    /// The base-color texture is sampled from the material group, so whether a
    /// variant reads one is the draw's answer: an entity carrying a material
    /// binds the group, one without one does not. The entity's own key,
    /// [`UnlitOptions::base_color_texture`](unlit_wgpu::pipeline::UnlitOptions::base_color_texture),
    /// decides it too — the two have to
    /// agree, or the group does not fit the pipeline — and carrying it on the
    /// draw as well is what lets a family specialize the variant from what the
    /// frame actually binds.
    ///
    /// [`MATERIAL_GROUP`]: unlit_wgpu::pipeline::MATERIAL_GROUP
    pub material: bool,
}

impl DrawKey {
    /// The key for drawing a mesh into an attachment set with `surface`.
    pub fn for_mesh(surface: SurfaceKey, mesh: &GpuMesh) -> Self {
        Self {
            surface,
            vertex_buffers: mesh.vertex_layout.clone(),
            index_format: mesh.parts.index_buffer.as_ref().map(|(_, format)| *format),
            material: false,
        }
    }

    /// Whether the draw binds a material group.
    ///
    /// A family that specializes the base-color-texture answer from the frame
    /// sets this from the material the draw binds; users recording draws by
    /// hand leave it at the default unless they bind one themselves.
    pub fn with_material(mut self, material: bool) -> Self {
        self.material = material;
        self
    }
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
