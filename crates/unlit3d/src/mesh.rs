//! The renderer's mesh description.
//!
//! A [`MeshDesc`] says what a mesh is in wgpu's own terms: vertex buffers
//! tagged with the slot a pipeline's vertex state expects them in, an optional
//! index buffer, and the mesh's local-space [`Aabb`]. The renderer assumes no
//! particular vertex layout, so a mesh can carry any combination of attributes
//! and one pipeline, several, or none may specialize on it.

use crate::bounds::Aabb;

/// A vertex buffer bound at `slot` for every draw of a mesh, with the
/// layout the pipeline's vertex state must match.
#[derive(Clone, Debug)]
pub struct VertexBufferDesc {
    /// The vertex-buffer slot the pipeline's vertex state declares.
    pub slot: u32,
    /// The buffer itself.
    pub buffer: wgpu::Buffer,
    /// Stride between elements.
    pub array_stride: u64,
    /// How the buffer advances: per vertex or per instance.
    pub step_mode: wgpu::VertexStepMode,
    /// The attributes this buffer provides.
    pub attributes: Vec<wgpu::VertexAttribute>,
}

/// Geometry to upload, described the way wgpu describes it.
///
/// [`crate::Renderer::allocate_mesh`] takes one of these and returns a
/// [`crate::GpuMesh`] handle. The buffers are moved into the renderer's
/// resource graph, so the caller hands over ownership.
#[derive(Clone, Debug, Default)]
pub struct MeshDesc {
    /// Vertex buffers, each tagged with its slot.
    pub vertex_buffers: Vec<VertexBufferDesc>,
    /// Index buffer and its format, for an indexed draw.
    pub index_buffer: Option<(wgpu::Buffer, wgpu::IndexFormat)>,
    /// Index count for an indexed draw, vertex count otherwise.
    pub count: u32,
    /// Whether draws of this mesh are indexed.
    pub indexed: bool,
    /// The mesh's local-space bounding box, recorded in the mesh-metadata
    /// array and used for CPU frustum culling.
    ///
    /// Defaults to [`Aabb::ZERO`]: a zero-extent box at the origin.
    pub aabb: Aabb,
    /// A bind group bound at
    /// [`MESH_GROUP`](wgpu_unlit_render::pipeline::MESH_GROUP), for a
    /// pipeline that reads per-mesh data such as the metadata index.
    ///
    /// `None` for a pipeline that binds nothing at that index.
    pub bind_group: Option<wgpu::BindGroup>,
    /// The per-mesh `MeshInfo` uniform the `bind_group` reads, if it reads
    /// one.
    ///
    /// The buffer is moved into the resource graph, so the caller hands over
    /// ownership. It is recorded as a *weak* node the bind group depends on:
    /// replacing it marks the group dirty, and removing the group orphans it
    /// for
    /// [`ResourceGraph::cleanup`](wgpu_unlit_render::resources::ResourceGraph::cleanup)
    /// to collect.
    pub mesh_info_buffer: Option<wgpu::Buffer>,
}
