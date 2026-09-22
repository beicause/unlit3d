//! The renderer's pipeline abstraction.
//!
//! A [`PipelineDesc`] is everything the renderer needs to draw with a
//! `wgpu::RenderPipeline`: the pipeline itself, the bind-group layouts its
//! draws agree with, and — for a pipeline that reads the renderer's own
//! camera, globals or metadata buffers — a way to rebuild its global bind
//! group when those buffers change. Nothing here is specific to the built-in
//! unlit shader: the unlit pipeline is registered through the same
//! [`crate::Renderer::register_pipeline`] a caller's own pipeline uses, and
//! supplies its own rebuild closure like anyone else.
//!
//! Geometry is described by [`crate::MeshDesc`], which lists vertex buffers
//! tagged with the slot a pipeline expects them in. The renderer assumes no
//! vertex layout, so a mesh can carry any combination of attributes and a
//! pipeline can specialize on it at draw time.

use std::sync::Arc;

use wgpu_unlit_render::resources::ResourceId;

/// A bind group together with the layout it was created from.
#[derive(Clone, Debug)]
pub struct PipelineBinding {
    /// The layout the bind group was built from.
    pub layout: wgpu::BindGroupLayout,
    /// The bind group itself.
    pub bind_group: wgpu::BindGroup,
}

/// Rebuilds a pipeline's global bind group against the renderer's current
/// buffers.
///
/// The renderer calls it whenever a buffer the group was built from is
/// replaced — the camera, globals or metadata buffer, say. A pipeline that
/// binds no global group passes `None` instead and is never visited.
pub type GlobalGroupRebuild = Arc<dyn Fn(&RenderResources) -> wgpu::BindGroup + Send + Sync>;

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
/// This is what [`crate::Renderer::register_pipeline`] takes. Every pipeline
/// in the renderer is described this way, the built-in unlit one included.
pub struct PipelineDesc {
    /// The compiled render pipeline.
    pub pipeline: wgpu::RenderPipeline,

    /// The bind group bound at
    /// [`GLOBAL_GROUP`](wgpu_unlit_render::pipeline::GLOBAL_GROUP), together
    /// with how to rebuild it when the renderer's buffers change.
    ///
    /// `None` for a pipeline that binds nothing at that index — a shader
    /// with no uniform or storage inputs, say.
    pub global: Option<GlobalBinding>,

    /// The layout a material bind group must be built from, bound at
    /// [`MATERIAL_GROUP`](wgpu_unlit_render::pipeline::MATERIAL_GROUP).
    ///
    /// `None` for a pipeline whose draws bind no material group.
    pub material_layout: Option<wgpu::BindGroupLayout>,

    /// The layout a mesh bind group must be built from, bound at
    /// [`MESH_GROUP`](wgpu_unlit_render::pipeline::MESH_GROUP).
    ///
    /// `None` for a pipeline whose draws bind no per-mesh group.
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
    /// Rebuilds [`GlobalBinding::bind_group`] from the renderer's current
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
    pub id: ResourceId,
    /// How to rebuild the group.
    pub rebuild: GlobalGroupRebuild,
}
