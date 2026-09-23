//! The renderer's pipeline abstraction.
//!
//! A [PipelineDesc] is everything the renderer needs to draw with a
//! wgpu render pipeline: the pipeline itself, the bind-group layouts its
//! draws agree with, and -- for a pipeline that reads the renderer's own
//! camera, globals or metadata buffers -- a way to rebuild its global bind
//! group when those buffers change. Nothing here is specific to the built-in
//! unlit shader: the unlit family is registered through the same
//! [Renderer::register_family](crate::Renderer::register_family) a caller's
//! own family uses, and supplies its own factory like anyone else.
//!
//! # Pipeline keys and families
//!
//! A [GpuPipeline] component does not name a compiled pipeline. It carries a
//! [PipelineKey], the entity's request for one family's variant: which
//! concrete pipeline an entity needs depends on the frame's render target and
//! on the mesh's vertex layout, neither of which is known when the entity is
//! spawned. A *family* closes that gap. It pairs a [Variants] cache with a
//! [Specializer] and a [PipelineFactory], queries the world for the entities
//! that carry its key type, and resolves each to a concrete pipeline. The
//! renderer registers every family under the [TypeId](core::any::TypeId) of
//! its key type; [crate::scene] drives them all once per frame. A family's
//! variant is compiled and registered the first time a key is seen, and later
//! frames reuse it.
//!
//! The entity, not the renderer, chooses its base descriptor: a
//! [PipelineKey] reports the blueprint ([PipelineKey::base_descriptor]) its
//! variants start from, so one family can draw entities whose base options
//! differ. The blueprint is supplied to [Variants::specialize] lazily and only
//! on a cache miss.
//!
//! Geometry is described by [crate::MeshDesc], which lists vertex buffers
//! tagged with the slot a pipeline expects them in and carries the layout each
//! buffer has. The renderer assumes no vertex layout, so a mesh can carry any
//! combination of attributes and a family can specialize on it at draw time.

use arrayvec::ArrayVec;
use core::hash::Hash;
use core::marker::PhantomData;
use std::sync::Arc;

use wgpu_unlit_render::resources::ResourceId;
use wgpu_unlit_render::scene::MAX_VERTEX_BUFFERS;
use wgpu_unlit_render::specialize::{
    CachedRenderPipeline, Specializable, Specializer, SpecializerKey, SurfaceKey,
    VertexBufferLayoutDesc,
};

use crate::components::GpuMesh;

/// Rebuilds a pipeline's global bind group against the renderer's current
/// buffers.
///
/// The renderer calls it whenever a buffer the group was built from is
/// replaced -- the camera, globals or metadata buffer, say. A pipeline that
/// binds no global group passes None instead and is never visited.
///
/// Not `Send`: the closure captures the wgpu device it builds groups with,
/// and wgpu's web device is not `Send`. The renderer drives it on the thread
/// the world lives on, so no cross-thread bound is needed.
pub type GlobalGroupRebuild = Arc<dyn Fn(&RenderResources) -> wgpu::BindGroup>;

/// The renderer's global buffers, as a rebuild closure sees them.
///
/// These are the buffers every pipeline can rely on the renderer keeping
/// up to date: the camera uniform, the frame globals and the mesh-metadata
/// storage buffer.
#[derive(Clone, Debug)]
pub struct RenderResources {
    /// The camera uniform buffer.
    pub camera: wgpu::Buffer,
    /// The frame-globals uniform buffer.
    pub globals: wgpu::Buffer,
    /// The mesh-metadata storage buffer.
    pub metadata: wgpu::Buffer,
}

/// A pipeline and the layouts its draws agree with.
///
/// This is what a [PipelineFactory] returns and what the renderer registers.
/// Every concrete pipeline in the renderer is described this way, the built-in
/// unlit ones included.
pub struct PipelineDesc {
    /// The compiled render pipeline.
    pub pipeline: wgpu::RenderPipeline,

    /// The bind group bound at
    /// [GLOBAL_GROUP](wgpu_unlit_render::pipeline::GLOBAL_GROUP), together
    /// with how to rebuild it when the renderer's buffers change.
    ///
    /// None for a pipeline that binds nothing at that index -- a shader
    /// with no uniform or storage inputs, say.
    pub global: Option<GlobalBinding>,

    /// The layout a material bind group must be built from, bound at
    /// [MATERIAL_GROUP](wgpu_unlit_render::pipeline::MATERIAL_GROUP).
    ///
    /// None for a pipeline whose draws bind no material group.
    pub material_layout: Option<wgpu::BindGroupLayout>,

    /// The layout a mesh bind group must be built from, bound at
    /// [MESH_GROUP](wgpu_unlit_render::pipeline::MESH_GROUP).
    ///
    /// None for a pipeline whose draws bind no per-mesh group.
    pub mesh_layout: Option<wgpu::BindGroupLayout>,
}

impl core::fmt::Debug for PipelineDesc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PipelineDesc")
            .field("pipeline", &self.pipeline)
            .field("global", &self.global)
            .field("material_layout", &self.material_layout)
            .field("mesh_layout", &self.mesh_layout)
            .finish()
    }
}

/// A pipeline's global bind group and the closure that keeps it current.
#[derive(Clone)]
pub struct GlobalBinding {
    /// The bind group bound for this pipeline's draws.
    pub bind_group: wgpu::BindGroup,
    /// Rebuilds [GlobalBinding::bind_group] from the renderer's current
    /// buffers after one of them is replaced.
    pub rebuild: GlobalGroupRebuild,
}

impl core::fmt::Debug for GlobalBinding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlobalBinding")
            .field("bind_group", &self.bind_group)
            .finish_non_exhaustive()
    }
}

/// The resource id a registered pipeline's global group lives under.
#[derive(Clone)]
pub(crate) struct RegisteredGlobal {
    /// The id in the renderer's resource graph.
    pub(crate) id: ResourceId,
    /// How to rebuild the group.
    pub(crate) rebuild: GlobalGroupRebuild,
}

/// What a [PipelineFactory] may read from the renderer.
pub struct FamilyContext<'a> {
    /// The device the pipeline is compiled on.
    pub device: &'a wgpu::Device,
    /// The renderer's global buffers, for a pipeline that binds them.
    pub resources: &'a RenderResources,
}

/// The base descriptor an entity's pipeline variants start from.
///
/// A [GpuPipeline] component carries a key of this type. It selects the family
/// the entity draws with -- the family registered for this key type -- and
/// supplies the blueprint that family's [Specializer] rewrites into the
/// concrete descriptor. Because the key carries the base, one family can serve
/// entities that begin from different descriptors; because it is the component
/// itself, the renderer never hands out a family handle.
pub trait PipelineKey: Clone + Hash + Eq + 'static {
    /// The specializable pipeline this key selects.
    type Pipeline: Specializable;

    /// The blueprint this key's variant is specialized from. Called only when
    /// the key's variant is compiled for the first time.
    fn base_descriptor(&self) -> <Self::Pipeline as Specializable>::Descriptor;
}

/// Everything that can change which concrete pipeline an entity needs beyond
/// the entity's own [PipelineKey].
///
/// The mesh layout uses the owned [VertexBufferLayoutDesc] from
/// [wgpu_unlit_render::specialize] (an owned mirror of a wgpu vertex buffer
/// layout), because a borrowed vertex layout cannot be stored in a key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DrawKey {
    /// The frame's render target.
    pub surface: SurfaceKey,
    /// The mesh's vertex layout, slot by slot.
    pub vertex_buffers: ArrayVec<(u32, VertexBufferLayoutDesc), MAX_VERTEX_BUFFERS>,
}

impl DrawKey {
    /// The key for drawing a mesh into an attachment set with `surface`.
    pub fn for_mesh(surface: SurfaceKey, mesh: &GpuMesh) -> Self {
        Self {
            surface,
            vertex_buffers: mesh.vertex_layout.clone(),
        }
    }
}

/// A concrete pipeline's position in a renderer's own pipeline list.
///
/// A lower value draws before a higher one. Opaque so the index is only ever
/// compared with another [PipelineId], never a plain integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PipelineId(u32);

impl PipelineId {
    /// The id for the pipeline registered at `index`.
    pub(crate) fn new(index: u32) -> Self {
        Self(index)
    }

    /// The id as a `usize`, for indexing the renderer's pipeline list.
    pub(crate) fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// A [SpecializerKey] a family that rewrites nothing uses.
///
/// It pairs the entity's [PipelineKey] with the [DrawKey] the draw resolved to.
/// Distinct keys always produce distinct descriptors, so the secondary cache is
/// skipped and every distinct key compiles its own variant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FamilyKey<K> {
    /// The entity's pipeline key, which supplied the base descriptor.
    pub(crate) key: K,
    /// The resolved draw.
    pub(crate) draw: DrawKey,
}

impl<K: Clone + Hash + Eq + 'static> SpecializerKey for FamilyKey<K> {
    // Every part of the key reaches the descriptor through the base, so
    // distinct keys are distinct descriptors.
    const IS_CANONICAL: bool = true;
    type Canonical = Self;
}

impl<K> From<(K, DrawKey)> for FamilyKey<K> {
    fn from((key, draw): (K, DrawKey)) -> Self {
        Self { key, draw }
    }
}

/// A [Specializer] that rewrites nothing: every key compiles the descriptor its
/// [PipelineKey] reported.
///
/// It is the specializer a one-off custom pipeline uses, where nothing about
/// the draw can change the pipeline. The key type parameter names the
/// [PipelineKey] the family is registered for.
pub struct TrivialSpecializer<K>(PhantomData<fn() -> K>);

impl<K> Default for TrivialSpecializer<K> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<K> Clone for TrivialSpecializer<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for TrivialSpecializer<K> {}

impl<K> core::fmt::Debug for TrivialSpecializer<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TrivialSpecializer").finish_non_exhaustive()
    }
}

impl<K: Clone + Hash + Eq + 'static, T: Specializable> Specializer<T> for TrivialSpecializer<K> {
    type Key = FamilyKey<K>;

    fn specialize(&self, key: FamilyKey<K>, _descriptor: &mut T::Descriptor) -> FamilyKey<K> {
        key
    }
}

/// A [PipelineFactory] for a pipeline that binds nothing beyond what a
/// [PipelineDesc] already carries: it registers the compiled
/// [CachedRenderPipeline] with no global, material or mesh group.
///
/// Paired with [TrivialSpecializer] it is the shortest route from a
/// [RenderPipelineDesc](wgpu_unlit_render::specialize::RenderPipelineDesc) to a
/// registered family, for a caller whose pipeline reads only its vertex
/// buffers.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderPipelineFactory;

impl PipelineFactory<CachedRenderPipeline> for RenderPipelineFactory {
    fn descriptor(
        &self,
        _context: &FamilyContext<'_>,
        value: &CachedRenderPipeline,
    ) -> PipelineDesc {
        PipelineDesc {
            pipeline: value.pipeline.clone(),
            global: None,
            material_layout: None,
            mesh_layout: None,
        }
    }
}

/// Turns a specialized value into the [PipelineDesc] the renderer registers.
pub trait PipelineFactory<T: Specializable> {
    /// The description of the pipeline for `value`, as the renderer should
    /// register it.
    fn descriptor(&self, context: &FamilyContext<'_>, value: &T) -> PipelineDesc;
}
