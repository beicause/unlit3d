//! The declarative description of a frame.
//!
//! A [`Scene`] is pure data: it names the pipelines to run, the bind groups
//! and vertex buffers each draw binds, and the ranges to draw. Recording it
//! ([`Scene::record`]) is a separate step, so a caller can build the
//! description once and reuse, inspect or modify it — and custom draw
//! commands are expressed with the same shapes as the built-in ones.
//!
//! The recording loop mirrors the resource hierarchy:
//!
//! ```text
//! for pipeline in scene.pipelines {
//!     set pipeline, then the pipeline's global bind groups
//!     for material in pipeline.materials {
//!         set the material's bind groups
//!         for mesh in material.meshes {
//!             set the mesh's bind groups and vertex buffers
//!             draw (indexed or not)
//!         }
//!     }
//! }
//! ```
//!
//! Consecutive draws that bind the same resource skip the redundant
//! `set_*` call.

use arrayvec::ArrayVec;
use core::ops::Range;

/// One bind-group slot: the group index and the group to bind there.
pub type BindGroupBinding<'a> = (u32, &'a wgpu::BindGroup);

/// One vertex-buffer slot: the slot index and the buffer slice to bind there.
pub type VertexBufferBinding<'a> = (u32, wgpu::BufferSlice<'a>);

/// What to draw for one mesh.
#[derive(Clone, Debug)]
pub enum DrawRange {
    /// Draw `vertices` without an index buffer, instanced over `instances`.
    Vertices {
        /// Vertex range to draw.
        vertices: Range<u32>,
        /// Instance range to draw.
        instances: Range<u32>,
    },
    /// Draw indexed, instanced over `instances`.
    Indexed {
        /// Index range to draw.
        indices: Range<u32>,
        /// Value added to every index before fetching a vertex.
        base_vertex: i32,
        /// Instance range to draw.
        instances: Range<u32>,
    },
}

impl DrawRange {
    /// A single non-indexed draw of `vertices`.
    pub fn vertices(vertices: Range<u32>) -> Self {
        Self::Vertices {
            vertices,
            instances: 0..1,
        }
    }

    /// A single indexed draw of `indices`.
    pub fn indexed(indices: Range<u32>) -> Self {
        Self::Indexed {
            indices,
            base_vertex: 0,
            instances: 0..1,
        }
    }

    /// Draw `count` instances instead of one.
    pub fn with_instances(self, instances: Range<u32>) -> Self {
        match self {
            Self::Vertices { vertices, .. } => Self::Vertices {
                vertices,
                instances,
            },
            Self::Indexed {
                indices,
                base_vertex,
                ..
            } => Self::Indexed {
                indices,
                base_vertex,
                instances,
            },
        }
    }

    /// Offset every fetched vertex index by `base_vertex` (indexed draws
    /// only; a no-op for non-indexed draws).
    pub fn with_base_vertex(self, base_vertex: i32) -> Self {
        match self {
            Self::Indexed {
                indices, instances, ..
            } => Self::Indexed {
                indices,
                base_vertex,
                instances,
            },
            other => other,
        }
    }
}

/// One mesh draw: its bind groups, its vertex buffers, and what to draw.
///
/// The built-in pipeline expects three or four vertex buffers — position,
/// optional joints and weights, UV and vertex color, and the per-instance
/// model matrix and base color — but the shape is generic, so custom
/// pipelines can bind whatever they declare.
#[derive(Clone, Debug)]
pub struct MeshDraw<'a> {
    /// Mesh-level bind groups, bound at the slots they name.
    pub bind_groups: Vec<BindGroupBinding<'a>>,
    /// Vertex buffers, bound at the slots they name.
    pub vertex_buffers: Vec<VertexBufferBinding<'a>>,
    /// Index buffer and its format, when the draw is indexed.
    pub index_buffer: Option<(wgpu::BufferSlice<'a>, wgpu::IndexFormat)>,
    /// What to draw.
    pub range: DrawRange,
}

impl<'a> MeshDraw<'a> {
    /// A mesh draw with no bind groups, no vertex buffers and no index
    /// buffer; fill in the fields the pipeline needs.
    pub fn new(range: DrawRange) -> Self {
        Self {
            bind_groups: Vec::new(),
            vertex_buffers: Vec::new(),
            index_buffer: None,
            range,
        }
    }

    /// Bind `bind_group` at `index`.
    pub fn with_bind_group(mut self, index: u32, bind_group: &'a wgpu::BindGroup) -> Self {
        self.bind_groups.push((index, bind_group));
        self
    }

    /// Bind `buffer` at vertex-buffer `slot`.
    pub fn with_vertex_buffer(mut self, slot: u32, buffer: wgpu::BufferSlice<'a>) -> Self {
        self.vertex_buffers.push((slot, buffer));
        self
    }

    /// Bind `buffer` as the index buffer.
    pub fn with_index_buffer(
        mut self,
        buffer: wgpu::BufferSlice<'a>,
        format: wgpu::IndexFormat,
    ) -> Self {
        self.index_buffer = Some((buffer, format));
        self
    }
}

/// A material: its bind groups and the meshes drawn with them.
///
/// The built-in pipeline uses at most one material bind group (index 1) — the
/// base-color texture and sampler — and no material at all when the mesh is
/// untextured.
#[derive(Clone, Debug, Default)]
pub struct MaterialGroup<'a> {
    /// Material-level bind groups, bound at the slots they name.
    pub bind_groups: Vec<BindGroupBinding<'a>>,
    /// Meshes drawn with this material.
    pub meshes: Vec<MeshDraw<'a>>,
}

impl<'a> MaterialGroup<'a> {
    /// An empty material group.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `bind_group` at `index`.
    pub fn with_bind_group(mut self, index: u32, bind_group: &'a wgpu::BindGroup) -> Self {
        self.bind_groups.push((index, bind_group));
        self
    }

    /// Draw `mesh` with this material.
    pub fn with_mesh(mut self, mesh: MeshDraw<'a>) -> Self {
        self.meshes.push(mesh);
        self
    }
}

/// A pipeline: the pipeline object, its global bind groups, and its
/// materials.
#[derive(Clone, Debug)]
pub struct PipelineGroup<'a> {
    /// The render pipeline to bind.
    pub pipeline: &'a wgpu::RenderPipeline,
    /// Global bind groups, bound at the slots they name. The built-in
    /// pipeline uses index 0 for the camera, frame globals and mesh metadata.
    pub bind_groups: Vec<BindGroupBinding<'a>>,
    /// Materials drawn with this pipeline.
    pub materials: Vec<MaterialGroup<'a>>,
}

impl<'a> PipelineGroup<'a> {
    /// A pipeline group with no bind groups and no materials.
    pub fn new(pipeline: &'a wgpu::RenderPipeline) -> Self {
        Self {
            pipeline,
            bind_groups: Vec::new(),
            materials: Vec::new(),
        }
    }

    /// Bind `bind_group` at `index`.
    pub fn with_bind_group(mut self, index: u32, bind_group: &'a wgpu::BindGroup) -> Self {
        self.bind_groups.push((index, bind_group));
        self
    }

    /// Draw `material` with this pipeline.
    pub fn with_material(mut self, material: MaterialGroup<'a>) -> Self {
        self.materials.push(material);
        self
    }
}

/// A frame's worth of draw commands.
///
/// The order of [`PipelineGroup`]s, and of materials and meshes within them,
/// is the order they are recorded in — so a caller controls opaque-then-
/// transparent ordering by ordering the groups.
#[derive(Clone, Debug, Default)]
pub struct Scene<'a> {
    /// The pipelines to run, in order.
    pub pipelines: Vec<PipelineGroup<'a>>,
}

impl<'a> Scene<'a> {
    /// An empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `pipeline` in this scene.
    pub fn with_pipeline(mut self, pipeline: PipelineGroup<'a>) -> Self {
        self.pipelines.push(pipeline);
        self
    }

    /// Append a pipeline to this scene.
    pub fn push(&mut self, pipeline: PipelineGroup<'a>) {
        self.pipelines.push(pipeline);
    }

    /// Whether the scene draws nothing.
    pub fn is_empty(&self) -> bool {
        self.pipelines
            .iter()
            .all(|pipeline| pipeline.materials.is_empty())
    }

    /// Record the scene into `pass`, skipping any `set_*` call whose target is
    /// already bound from the previous draw.
    pub fn record(&self, pass: &mut wgpu::RenderPass<'_>) {
        let mut state = PassState::default();
        for pipeline in &self.pipelines {
            if state.pipeline != Some(pipeline.pipeline) {
                pass.set_pipeline(pipeline.pipeline);
                state.pipeline = Some(pipeline.pipeline);
            }
            for &(index, bind_group) in &pipeline.bind_groups {
                state.set_bind_group(pass, index, bind_group);
            }

            for material in &pipeline.materials {
                for &(index, bind_group) in &material.bind_groups {
                    state.set_bind_group(pass, index, bind_group);
                }

                for mesh in &material.meshes {
                    for &(index, bind_group) in &mesh.bind_groups {
                        state.set_bind_group(pass, index, bind_group);
                    }
                    for (slot, buffer) in &mesh.vertex_buffers {
                        state.set_vertex_buffer(pass, *slot, *buffer);
                    }
                    if let Some((buffer, format)) = &mesh.index_buffer {
                        state.set_index_buffer(pass, *buffer, *format);
                    }

                    match &mesh.range {
                        DrawRange::Vertices {
                            vertices,
                            instances,
                        } => pass.draw(vertices.clone(), instances.clone()),
                        DrawRange::Indexed {
                            indices,
                            base_vertex,
                            instances,
                        } => pass.draw_indexed(indices.clone(), *base_vertex, instances.clone()),
                    }
                }
            }
        }
    }
}

/// Maximum number of bind-group slots a pass is tracked for.
///
/// The largest `max_bind_groups` a WebGPU device can advertise for this
/// renderer; exceeding it is a programming error caught while recording.
pub const MAX_BIND_GROUPS: usize = 8;

/// Maximum number of vertex-buffer slots a pass is tracked for.
pub const MAX_VERTEX_BUFFERS: usize = 16;

/// Which resources the previous draw left bound.
///
/// `wgpu` resources compare by identity, so a plain `==` is the fast
/// "is this the same resource as last time" test the recording loop needs.
/// The slots are fixed-capacity ([`MAX_BIND_GROUPS`] / [`MAX_VERTEX_BUFFERS`])
/// so recording a frame never allocates.
#[derive(Default)]
struct PassState<'a> {
    pipeline: Option<&'a wgpu::RenderPipeline>,
    bind_groups: ArrayVec<(u32, &'a wgpu::BindGroup), MAX_BIND_GROUPS>,
    vertex_buffers: ArrayVec<(u32, wgpu::BufferSlice<'a>), MAX_VERTEX_BUFFERS>,
    index_buffer: Option<(wgpu::BufferSlice<'a>, wgpu::IndexFormat)>,
}

impl<'a> PassState<'a> {
    fn set_bind_group(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        index: u32,
        bind_group: &'a wgpu::BindGroup,
    ) {
        if let Some(slot) = self
            .bind_groups
            .iter_mut()
            .find(|(bound, _)| *bound == index)
        {
            if slot.1 == bind_group {
                return;
            }
            slot.1 = bind_group;
        } else {
            assert!(
                index < MAX_BIND_GROUPS as u32,
                "bind group index {index} exceeds MAX_BIND_GROUPS ({MAX_BIND_GROUPS})"
            );
            self.bind_groups.push((index, bind_group));
        }
        pass.set_bind_group(index, bind_group, &[]);
    }

    fn set_vertex_buffer(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        slot: u32,
        buffer: wgpu::BufferSlice<'a>,
    ) {
        if let Some(bound) = self
            .vertex_buffers
            .iter_mut()
            .find(|(bound, _)| *bound == slot)
        {
            if bound.1 == buffer {
                return;
            }
            bound.1 = buffer;
        } else {
            assert!(
                slot < MAX_VERTEX_BUFFERS as u32,
                "vertex buffer slot {slot} exceeds MAX_VERTEX_BUFFERS ({MAX_VERTEX_BUFFERS})"
            );
            self.vertex_buffers.push((slot, buffer));
        }
        pass.set_vertex_buffer(slot, buffer);
    }

    fn set_index_buffer(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        buffer: wgpu::BufferSlice<'a>,
        format: wgpu::IndexFormat,
    ) {
        if let Some((bound, bound_format)) = &self.index_buffer
            && *bound == buffer
            && *bound_format == format
        {
            return;
        }
        self.index_buffer = Some((buffer, format));
        pass.set_index_buffer(buffer, format);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_range_builders_compose() {
        let range = DrawRange::vertices(0..36).with_instances(2..5);
        match range {
            DrawRange::Vertices {
                vertices,
                instances,
            } => {
                assert_eq!(vertices, 0..36);
                assert_eq!(instances, 2..5);
            }
            other => panic!("expected a non-indexed draw, got {other:?}"),
        }

        let range = DrawRange::indexed(0..12)
            .with_base_vertex(7)
            .with_instances(0..3);
        match range {
            DrawRange::Indexed {
                indices,
                base_vertex,
                instances,
            } => {
                assert_eq!(indices, 0..12);
                assert_eq!(base_vertex, 7);
                assert_eq!(instances, 0..3);
            }
            other => panic!("expected an indexed draw, got {other:?}"),
        }
    }

    #[test]
    fn base_vertex_is_ignored_for_non_indexed_draws() {
        let range = DrawRange::vertices(0..3).with_base_vertex(9);
        assert!(matches!(range, DrawRange::Vertices { .. }));
    }

    #[test]
    fn empty_scene_reports_empty() {
        let scene = Scene::new();
        assert!(scene.is_empty());
    }

    #[test]
    fn pass_state_capacity_covers_the_wgpu_defaults() {
        // The fixed arrays must have room for everything a default device
        // accepts, otherwise the recording asserts would fire on valid scenes.
        assert!(MAX_BIND_GROUPS >= wgpu::Limits::default().max_bind_groups as usize);
        assert!(MAX_VERTEX_BUFFERS >= wgpu::Limits::default().max_vertex_buffers as usize);
    }
}
