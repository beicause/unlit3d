//! The mesh description.
//!
//! A [`MeshDesc`] says what a mesh is in wgpu's own terms: vertex buffers
//! tagged with the slot a pipeline's vertex state expects them in, an optional
//! index buffer, and the mesh's local-space [`Aabb`]. The renderer assumes no
//! particular vertex layout, so a mesh can carry any combination of attributes
//! and one pipeline, several, or none may specialize on it.

use arrayvec::ArrayVec;

use crate::bounds::Aabb;
pub use wgpu_unlit_render::mesh::JointMatrix;
use wgpu_unlit_render::resources::ResourceId;
use wgpu_unlit_render::scene::MAX_VERTEX_BUFFERS;
use wgpu_unlit_render::specialize::VertexAttributes;

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
    /// [`MESH_GROUP`](wgpu_unlit_render::pipeline::MESH_GROUP), for a
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

    /// The scene data the mesh's `bind_group` reads besides the mesh-info
    /// uniform: the joint matrices and morph targets it deforms with.
    ///
    /// Their buffers are moved into the resource graph with the mesh. Each is
    /// recorded as a *weak* node under the mesh's virtual root, and as a
    /// dependency of the bind group, so replacing one marks that group dirty
    /// and removing the mesh releases it with the rest of the mesh's
    /// resources. A mesh with no pose data leaves this at its default.
    pub pose: MeshPoseDesc,
}

/// The scene data a mesh's `bind_group` reads besides its `MeshInfo` uniform.
///
/// Every buffer is moved into the resource graph with the mesh, so the caller
/// hands over ownership; a mesh without pose data passes none of them.
#[derive(Clone, Debug, Default)]
pub struct MeshPoseDesc {
    /// The mesh's skin, for a pipeline that reads joint matrices.
    pub skin: Option<SkinDesc>,
    /// The mesh's morph displacements, for a pipeline that reads morph
    /// positions.
    pub morph: Option<MorphDesc>,
}

/// A mesh's joint matrices and how many of them there are.
#[derive(Clone, Debug)]
pub struct SkinDesc {
    /// The joint matrices, one per joint, moved into the resource graph.
    pub matrices: wgpu::Buffer,
    /// How many matrices the buffer holds.
    pub joint_count: u32,
}

/// A mesh's morph displacements.
///
/// The weights that blend them are *not* here: they are a
/// [`MorphWeights`] the caller owns and may share between meshes, so a mesh
/// names one rather than owning it.
#[derive(Clone, Debug)]
pub struct MorphDesc {
    /// Every target's per-vertex position displacement, flat and tightly
    /// packed: for each vertex, `target_count` targets in order, three
    /// components each.
    pub deltas: wgpu::Buffer,
    /// The weights that blend the targets, shared or private.
    pub weights: MorphWeights,
    /// How many targets follow each vertex.
    pub target_count: u32,
}

/// How many graph nodes a mesh's pose contributes at most: the joint matrices
/// and the morph displacements.
///
/// The morph weights are not counted: they are a [`MorphWeights`] resource the
/// mesh reads rather than one it contributes.
pub const MAX_POSE_PARTS: usize = 2;

/// The channels of one mesh uploaded through
/// [`MeshSource::allocate_unlit_mesh`](crate::mesh_source::MeshSource::allocate_unlit_mesh).
///
/// Every slice describes the same vertices, in the same order; only
/// [`Self::positions`] is required, because a variant without a position
/// stream draws a single point and needs no geometry at all.
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
    /// The skin that deforms the mesh, for a variant that reads joints.
    pub skin: Option<UnlitSkin<'a>>,
    /// The morph targets that displace the mesh, in target order.
    pub morph_targets: &'a [UnlitMorphTarget<'a>],
    /// The weights that blend [`Self::morph_targets`].
    ///
    /// Required — and required to be as long as the target slice — by a
    /// variant that reads morph positions, ignored by one that does not. The
    /// same handle may back several meshes, which then share one pose; see
    /// [`MorphWeights`].
    pub morph_weights: Option<MorphWeights>,
}

/// A mesh's skin: the joints each vertex is bound to, and the pose they deform
/// it by.
///
/// The joint matrices live in a buffer the mesh owns, so a later frame updates
/// the pose with
/// [`MeshSource::update_skin`](crate::mesh_source::MeshSource::update_skin)
/// rather than re-uploading the mesh.
#[derive(Clone, Copy, Debug)]
pub struct UnlitSkin<'a> {
    /// Per-vertex joint indices, four per vertex.
    pub joints: &'a [[u16; 4]],
    /// Per-vertex joint weights, four per vertex, summing to 1.
    ///
    /// Weights are normalized on upload, so a caller may pass unnormalized
    /// ones; a vertex whose weights sum to zero is left undeformed.
    pub weights: &'a [[f32; 4]],
    /// The bind pose: one matrix per joint, each the joint's world transform
    /// times the inverse of the transform it was bound in.
    ///
    /// A vertex is deformed by the weighted sum of the matrices its indices
    /// name.
    pub pose: &'a [JointMatrix],
}

/// One morph target of a mesh: the displacement it applies to every vertex.
///
/// Only positions displace — a morph target carries no normal or tangent —
/// and a target displaces every vertex of the mesh it belongs to, so
/// [`Self::positions`] must be as long as the mesh's own vertex list. How much
/// of the displacement applies is the mesh's
/// [`MorphWeights`], not the target's: the same target can be weighted
/// differently by two meshes sharing it.
#[derive(Clone, Copy, Debug)]
pub struct UnlitMorphTarget<'a> {
    /// Per-vertex position displacement, in the mesh's local space.
    pub positions: &'a [[f32; 3]],
}

/// The weights that blend a mesh's morph targets, as a resource of its own.
///
/// The handle names a weight buffer in the resource graph, which makes the
/// weights *shareable*: allocating several meshes with the same handle binds
/// them to one buffer, so
/// [`MeshSource::update_morph_weights`](crate::mesh_source::MeshSource::update_morph_weights)
/// writes one pose that every one of them draws with. That is what a crowd of
/// copies of one mesh usually wants, and it costs one buffer instead of one
/// per mesh.
///
/// A mesh whose weights should move on its own needs a handle of its own.
///
/// # Sharing rules
///
/// Every mesh sharing a handle must carry the same number of morph targets:
/// the shader loops up to the mesh's own target count, so a longer loop over a
/// shorter buffer would read out of bounds. Allocation enforces this.
#[derive(Clone, Debug)]
pub struct MorphWeights {
    /// The buffer's node in the resource graph.
    pub(crate) buffer: ResourceId,
    /// How many weights the buffer holds, which is the target count a mesh
    /// sharing it must carry.
    pub(crate) target_count: u32,
}

impl MorphWeights {
    /// How many weights the handle holds.
    pub fn target_count(&self) -> u32 {
        self.target_count
    }

    /// The graph node that holds the weights.
    ///
    /// Two handles with the same node name the same weights, and so the same
    /// pose. The node is useful for inspecting the buffer's lifetime; the
    /// weights themselves are written through
    /// [`MeshSource::update_morph_weights`](crate::mesh_source::MeshSource::update_morph_weights).
    pub fn resource(&self) -> ResourceId {
        self.buffer
    }
}
