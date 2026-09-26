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
    /// pipeline that reads per-mesh data such as the metadata index.
    ///
    /// `None` for a pipeline that binds nothing at that index.
    pub bind_group: Option<wgpu::BindGroup>,
    /// The per-mesh `MeshInfo` uniform the `bind_group` reads, if it reads
    /// one.
    ///
    /// The buffer is moved into the resource graph, so the caller hands over
    /// ownership. It is recorded as a *weak* node under the mesh's virtual
    /// root, so it lives exactly as long as the mesh: replacing it marks the
    /// bind group dirty, and removing the mesh releases it with the rest of
    /// the mesh's resources.
    pub mesh_info_buffer: Option<wgpu::Buffer>,

    /// The morph displacements the mesh's `bind_group` reads.
    ///
    /// The buffer is moved into the resource graph with the mesh and recorded
    /// as a *weak* node under the mesh's virtual root, so removing the mesh
    /// releases it with the rest of the mesh's resources. A mesh with no morph
    /// targets leaves this `None`.
    ///
    /// The weights that blend these displacements are *not* here: they are
    /// per-instance pose state a [`MorphWeights`](crate::components::MorphWeights)
    /// entity holds, so a mesh names the entity rather than owning the weights.
    pub morph_deltas: Option<MorphDeltas>,
}

/// A mesh's morph displacements and how many targets follow each vertex.
#[derive(Clone, Debug)]
pub struct MorphDeltas {
    /// Every target's per-vertex position displacement, flat and tightly
    /// packed: for each vertex, `target_count` targets in order, three
    /// components each.
    pub buffer: wgpu::Buffer,
    /// How many targets follow each vertex.
    pub target_count: u32,
}

/// The channels of one mesh uploaded through
/// [`MeshSource::allocate_unlit_mesh`](crate::mesh_source::MeshSource::allocate_unlit_mesh).
///
/// Every slice describes the same vertices, in the same order; only
/// [`Self::positions`] is required, because a variant without a position
/// stream draws a single point and needs no geometry at all.
///
/// The pose a mesh is drawn with is not here: joint matrices and morph weights
/// are CPU-driven per-frame state, so they live on the entities a
/// [`SkinBinding`](crate::components::SkinBinding) and a
/// [`MorphBinding`](crate::components::MorphBinding) name and the renderer
/// uploads them every frame.
#[derive(Clone, Debug, Default)]
pub struct UnlitMeshDesc<'a> {
    /// Per-vertex positions, in the mesh's local space.
    pub positions: &'a [[f32; 3]],
    /// Per-vertex UVs, for a variant that reads them.
    pub uvs: Option<&'a [[f32; 2]]>,
    /// Per-vertex linear RGBA colors, for a variant that reads them.
    pub colors: Option<&'a [[u8; 4]]>,
    /// Triangle indices, for an indexed draw.
    pub indices: Option<&'a [u32]>,
    /// The joints each vertex is bound to, for a variant that reads joints.
    pub joints: Option<&'a [[u16; 4]]>,
    /// The joint weights of each vertex, four per vertex, for a variant that
    /// reads joints.
    ///
    /// Weights are normalized on upload, so a caller may pass unnormalized
    /// ones; a vertex whose weights sum to zero is left undeformed.
    pub weights: Option<&'a [[f32; 4]]>,
    /// The morph targets that displace the mesh, in target order.
    pub morph_targets: &'a [UnlitMorphTarget<'a>],
}

/// One morph target of a mesh: the displacement it applies to every vertex.
///
/// Only positions displace — a morph target carries no normal or tangent —
/// and a target displaces every vertex of the mesh it belongs to, so
/// [`Self::positions`] must be as long as the mesh's own vertex list. How much
/// of the displacement applies is the mesh's
/// [`MorphWeights`](crate::components::MorphWeights) component, not the
/// target's: the same target can be weighted differently by two meshes sharing
/// it.
#[derive(Clone, Copy, Debug)]
pub struct UnlitMorphTarget<'a> {
    /// Per-vertex position displacement, in the mesh's local space.
    pub positions: &'a [[f32; 3]],
}
