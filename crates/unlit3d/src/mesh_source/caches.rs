//! The per-frame caches the draw list is built from.
//!
//! Every collection here is reused between frames: a steady scene clears and
//! refills them instead of allocating, so the frame reaches the GPU without
//! touching the allocator.

use super::*;

/// The allocations one frame of the mesh path reuses.
pub(crate) struct FrameCaches {
    /// Reused Vec of the meshes that passed this frame's frustum culling.
    pub visible_meshes_cache: Vec<VisibleMesh>,
    /// Reused Vec for visible-entity collection and per-frame sorting.
    pub visible_cache: Vec<VisibleEntry>,
    /// Reused Vec of the instance-stream binding of each registered family,
    /// indexed by [FamilyId].
    pub family_instance_cache: Vec<Option<(u32, wgpu::Buffer)>>,
    /// Reused Vec for cloned pipeline handles while the scene is built.
    pub pipeline_handle_cache: Vec<RenderPipelineHandles>,
    /// The frame's interned draw handles, one per distinct
    /// [`DrawHandlesKey`] the visible set named. Reused between frames, so a
    /// steady scene allocates nothing.
    pub entry_handle_cache: HashMap<DrawHandlesKey, EntryHandles>,
}

impl FrameCaches {
    /// Create the caches empty. They grow to the scene's size on the first
    /// frame that draws and keep that room afterwards.
    pub(super) fn new() -> Self {
        Self {
            visible_meshes_cache: Vec::new(),
            visible_cache: Vec::new(),
            family_instance_cache: Vec::new(),
            pipeline_handle_cache: Vec::new(),
            entry_handle_cache: HashMap::new(),
        }
    }
}
