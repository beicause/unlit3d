//! The mesh description.
//!
//! A [`MeshDesc`] says what a mesh is in wgpu's own terms: vertex buffers
//! tagged with the slot a pipeline's vertex state expects them in, an optional
//! index buffer, and the mesh's local-space [`Aabb`]. The renderer assumes no
//! particular vertex layout, so a mesh can carry any combination of attributes
//! and one pipeline, several, or none may specialize on it.
//!
//! A mesh's *pose* is not part of its description. Joint matrices and morph
//! weights are per-frame CPU state, so they live in components of their own —
//! [`SkinPose`](crate::components::SkinPose) and
//! [`MorphWeights`](crate::components::MorphWeights) — that the mesh's entity
//! references through a [`SkinBinding`](crate::components::SkinBinding) and a
//! [`MorphBinding`](crate::components::MorphBinding).

use arrayvec::ArrayVec;

use crate::bounds::Aabb;
pub use unlit_wgpu::mesh::JointMatrix;
use unlit_wgpu::scene::MAX_VERTEX_BUFFERS;
use unlit_wgpu::specialize::VertexAttributes;

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
    pub attributes: VertexAttributes,
}

/// Geometry to upload, described the way wgpu describes it.
///
/// [`crate::mesh_source::MeshSource::allocate_mesh`] takes one of these and returns a
/// [`crate::components::GpuMesh`] handle. The buffers are moved into the source's
/// resource graph, so the caller hands over ownership.
#[derive(Clone, Debug, Default)]
pub struct MeshDesc {
    /// Vertex buffers, each tagged with its slot.
    ///
    /// At most [`MAX_VERTEX_BUFFERS`], the number of vertex-buffer slots a
    /// pass can bind: a mesh needing more cannot be drawn.
    pub vertex_buffers: ArrayVec<VertexBufferDesc, MAX_VERTEX_BUFFERS>,
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
    /// [`MESH_GROUP`](unlit_wgpu::pipeline::MESH_GROUP), for a
    /// pipeline that reads per-mesh data.
    ///
    /// `None` for a pipeline that binds nothing at that index.
    pub bind_group: Option<wgpu::BindGroup>,

    /// The morph displacements the mesh's vertices are displaced by.
    ///
    /// A mesh with no morph targets leaves this `None`.
    ///
    /// The weights that blend these displacements are *not* here: they are
    /// per-instance pose state a [`MorphWeights`](crate::components::MorphWeights)
    /// entity holds, so a mesh names the entity rather than owning the weights.
    pub morph_deltas: Option<MorphDeltas>,
}

/// A mesh's morph displacements and how many targets follow each vertex.
///
/// The displacements are CPU data: the source pools them into one frame-wide
/// array alongside every other morphed mesh's, and the mesh's metadata entry
/// names the slice it took. Handing the source the geometry rather than a
/// ready-made GPU buffer is what lets it lay every mesh's displacements out in
/// one array.
#[derive(Clone, Debug)]
pub struct MorphDeltas {
    /// Every target's per-vertex position displacement, flat and tightly
    /// packed: for each vertex, `target_count` targets in order, three
    /// components each.
    pub deltas: Vec<f32>,
    /// How many targets follow each vertex.
    pub target_count: u32,
}
