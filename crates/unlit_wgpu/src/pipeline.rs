//! The built-in lit pipeline and the WESL composition behind it.
//!
//! [`SpecializedLitPipeline`] composes an [`LitVariant`]'s entry shader —
//! the built-in `lit.wesl`, or the caller's own when
//! [`LitOptions::shader`] carries one — builds the layouts the shader
//! expects, and exposes the vertex-buffer layouts the caller declares when
//! creating meshes. Each variant contains exactly the bindings and attributes
//! the selected channels need, so nothing unused reaches the GPU.

use crate::mesh::{
    ChannelEncoding, PositionStreamChannels, PositionStreamWriter, SecondaryFlags,
    SecondaryStreamWriter,
};
use crate::specialize::{PipelineDescriptor, PipelineVariant, SurfaceKey, VertexBufferLayoutDesc};
use crate::util::Hashed;
use std::sync::Arc;

/// Binding slot of the camera uniform in the global bind group.
pub const CAMERA_BINDING: u32 = 0;
/// Binding slot of the frame-globals uniform in the global bind group.
pub const FRAME_BINDING: u32 = 1;
/// Binding slot of the pass's view uniform in the global bind group.
///
/// It carries the region of the target the pass draws through, which is what a
/// shader measuring in pixels divides by. The camera and this differ in who
/// owns them: a camera is the frame's, a view is the pass's, so one camera drawn
/// through two viewports is two views.
pub const VIEW_BINDING: u32 = 2;
/// Binding slot of the mesh-metadata storage buffer in the global bind group.
pub const MESH_METADATA_BINDING: u32 = 3;
/// Binding slot of the frame's joint matrices in the global bind group.
///
/// The array holds the pose of every visible instance, so it is per-frame data
/// the source packs rather than anything a mesh owns; an instance's own slice
/// starts at [`crate::mesh::MeshInstance::joints_base`].
pub const JOINTS_BINDING: u32 = 4;
/// Binding slot of the frame's morph weights in the global bind group.
///
/// Like the joint matrices this is one array for the whole frame, and an
/// instance's own slice starts at [`crate::mesh::MeshInstance::morph_base`].
pub const MORPH_WEIGHTS_BINDING: u32 = 5;
/// Binding slot of the frame's morph position displacements in the global bind
/// group.
///
/// A mesh's displacements are its own geometry, but one array holds them for
/// the whole frame and the mesh names its slice through the `morph_deltas_offset`
/// of its [`MeshMetadata`](crate::mesh::MeshMetadata) entry. Pooling them here
/// rather than in a per-mesh buffer is what lets a draw bind no mesh group.
pub const MORPH_DELTAS_BINDING: u32 = 6;
/// Binding slot of the frame's lights in the global bind group.
///
/// One record per light, in view space, packed by the source every frame. The
/// fragment stage reads it, so it is not visible to a vertex stage.
pub const LIGHTS_BINDING: u32 = 7;
/// Binding slot of the frame's cluster cells in the global bind group.
///
/// Each cell's record lists the lights assigned to it; see
/// [`crate::light::CELL_WORDS`].
pub const LIGHT_CELLS_BINDING: u32 = 8;
/// Binding slot of the frame's polygon-light vertices in the global bind group.
///
/// A light whose shape has corners names its slice through its record's
/// `kind.y` and `kind.z`.
pub const LIGHT_VERTICES_BINDING: u32 = 9;
/// Binding slot of the cluster uniform in the global bind group.
///
/// The cluster grid's shape and depth mapping: what a fragment needs to find the
/// cell it is in.
pub const CLUSTER_BINDING: u32 = 10;
/// Binding slot of the frame-wide lighting uniform in the global bind group.
///
/// The ambient term and the frame's directional lights, which reach every
/// fragment: what a fragment needs to light itself before it asks the grid.
pub const LIGHTING_BINDING: u32 = 11;
/// Bind-group index of the global group.
pub const GLOBAL_GROUP: u32 = 0;
/// Bind-group index of the material group.
pub const MATERIAL_GROUP: u32 = 1;
/// Binding slot of the base-color texture in the material group.
pub const BASE_COLOR_TEXTURE_BINDING: u32 = 0;
/// Binding slot of the base-color sampler in the material group.
pub const BASE_COLOR_SAMPLER_BINDING: u32 = 1;
/// Bind-group index of the mesh group.
///
/// The built-in lit variants bind nothing here: every input they read is in
/// the global group or on the instance stream. The index stays a general
/// extension point, so a caller's own pipeline can bind per-mesh data — a tint,
/// a transform palette, a material parameter — and
/// [`crate::scene::DrawEntry`] will bind it.
pub const MESH_GROUP: u32 = 2;

/// Vertex-buffer slot carrying the primary stream: positions and, for a skinned
/// mesh, the joint indices and weights that deform them.
pub const PRIMARY_SLOT: u32 = 0;
/// Vertex-buffer slot carrying the secondary stream: the UV, color and normal a
/// mesh carries beside its position.
pub const SECONDARY_SLOT: u32 = 1;
/// Vertex-buffer slot carrying per-instance data.
pub const INSTANCE_SLOT: u32 = 2;

/// Vertex attribute locations declared by the built-in shader.
pub mod location {
    /// Compressed position (`Snorm16x4`).
    pub const POSITION: u32 = 0;
    /// Compressed UV (`Snorm16x2`).
    pub const UV: u32 = 1;
    /// Vertex color (`Unorm8x4`).
    pub const COLOR: u32 = 2;
    /// First column of the per-instance model matrix.
    pub const MODEL_0: u32 = 3;
    /// Second column of the per-instance model matrix.
    pub const MODEL_1: u32 = 4;
    /// Third column of the per-instance model matrix.
    pub const MODEL_2: u32 = 5;
    /// Per-instance base color.
    pub const BASE_COLOR: u32 = 6;
    /// Joint indices (`Uint16x4`), read from the position stream.
    pub const JOINTS: u32 = 7;
    /// Joint weights (`Unorm16x4`), read from the position stream right after
    /// the indices.
    pub const JOINTS_WEIGHTS: u32 = 8;
    /// Per-instance joint-matrix base (`Uint32`), read from the instance
    /// stream.
    ///
    /// The instance's first joint matrix in the frame's shared joint array; the
    /// shader reads a joint at this base plus its own joint index.
    pub const JOINTS_BASE: u32 = 9;
    /// Per-instance index of the mesh metadata entry (`Uint32`), read from the
    /// instance stream.
    pub const METADATA_INDEX: u32 = 10;
    /// Per-instance alpha cutoff (`Float32`), read from the instance stream.
    ///
    /// A [`LitOptions::alpha_cutoff`](crate::pipeline::LitOptions::alpha_cutoff)
    /// variant compares a fragment's alpha against it and discards the
    /// fragment when it is below.
    pub const CUTOFF: u32 = 11;
    /// Per-instance morph-weight base (`Uint32`), read from the instance
    /// stream.
    ///
    /// The instance's first morph weight in the frame's shared weight array;
    /// the shader reads a target's weight at this base plus its own target
    /// index. It is a location of its own rather than a lane of
    /// [`JOINTS_BASE`] because skinning and morphing are independent — a
    /// variant that reads one has no business declaring the other.
    pub const MORPH_BASE: u32 = 12;
    /// Vertex normal (`Snorm16x2`), read from the secondary stream.
    ///
    /// Octahedral-encoded, so the shader decodes it on its own; see
    /// [`crate::mesh::compress_normals`].
    pub const NORMAL: u32 = 13;
    /// Per-instance inverted-hull width (`Float32`), read from the instance
    /// stream.
    ///
    /// The distance the shader pushes a vertex out along its normal. An
    /// [`LitOptions::expanded`](crate::pipeline::LitOptions::expanded) variant
    /// reads it; every other variant leaves the field unread, and an instance
    /// that is not a hull carries zero.
    pub const HULL_WIDTH: u32 = 14;
}

/// Entry point name of a lit shader's vertex stage.
///
/// The pipeline names this entry point, so every lit entry shader — the built-in
/// `lit.wesl` and a caller's module alike — declares it.
pub const VS_MAIN: &str = "vs_main";
/// Entry point name of a lit shader's fragment stage.
pub const FS_MAIN: &str = "fs_main";
/// Label of the built-in entry shader: its shader module's label, and the name
/// a composition error is reported under.
const BUILT_IN_SHADER_LABEL: &str = "unlit_wgpu::lit";

/// Name of the WESL module the built-in entry shader is composed from: the file
/// `shaders/lit.wesl` of this crate's package.
const BUILT_IN_MODULE: &str = "lit";

mod channels;
mod layouts;
mod options;
mod shader;
mod shader_cache;
mod variant;

pub use self::{channels::*, layouts::*, options::*, shader::*, shader_cache::*, variant::*};

#[cfg(test)]
mod tests;
