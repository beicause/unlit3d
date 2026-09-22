//! The renderer's mesh description.
//!
//! A [`MeshDesc`] says what a mesh is in wgpu's own terms: vertex buffers
//! tagged with the slot a pipeline's vertex state expects them in, plus an
//! optional index buffer. The renderer assumes no particular vertex layout,
//! so a mesh can carry any combination of attributes and one pipeline,
//! several, or none may specialize on it.

/// A vertex buffer bound at `slot` for every draw of a mesh.
#[derive(Clone, Debug)]
pub struct VertexBufferDesc {
    /// The vertex-buffer slot the pipeline's vertex state declares.
    pub slot: u32,
    /// The buffer itself.
    pub buffer: wgpu::Buffer,
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
    /// A bind group bound at
    /// [`MESH_GROUP`](wgpu_unlit_render::pipeline::MESH_GROUP), for a
    /// pipeline that reads per-mesh data such as the metadata index.
    pub bind_group: Option<wgpu::BindGroup>,
}
