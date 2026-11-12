//! Vertex compression and the per-mesh decode metadata.
//!
//! The built-in pipeline consumes a compact vertex layout: positions and UVs
//! are 16-bit normalized integers relative to a per-mesh bounding box, so the
//! shader needs the parameters that reverse that encoding. [`MeshMetadata`]
//! carries those parameters; [`compress_positions`] and [`compress_uvs`]
//! produce them together with the packed attribute streams.
//!
//! The Rust side and the WGSL side (`mesh_metadata.wesl`) are kept in sync by
//! the crate. [`SecondaryStreamWriter`] and [`PositionStreamWriter`] pack the
//! per-vertex streams a draw declares, whichever pipeline then draws them.
//!
//! # One description per stream
//!
//! A stream is described by [`StreamChannels`]: one attribute location and
//! [`ChannelEncoding`] per channel, in the order the channels are interleaved.
//! The pipeline's vertex layout, the compression that packs a mesh and the
//! streams a pool allocates all derive their bytes per vertex from that same
//! description, so the three cannot drift apart.

mod compress;
mod position;
mod secondary;

pub use compress::{
    compress_indices, compress_normals, compress_positions, compress_uvs, compress_weights,
    encode_uvs, index_fits_u16, quantize_colors,
};
pub use position::{JointPair, PositionStreamChannels, PositionStreamWriter};
pub use secondary::{SecondaryFlags, SecondaryStreamWriter};

use self::compress::f32_to_unorm8;

/// How one compressed channel is encoded as a vertex attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelEncoding {
    /// An AABB-quantized position: `Snorm16x4`, decoded through
    /// [`MeshMetadata`]'s bounding box.
    CompressedPosition,
    /// A full-precision position: `Float32x3`.
    UncompressedPosition,
    /// A full-precision planar position: `Float32x2`. The missing third
    /// component reads as zero, which is what geometry that already lies in the
    /// target's plane — a user interface's, say — would store anyway.
    PlanarPosition,
    /// A range-remapped UV: `Snorm16x2`, decoded through
    /// [`MeshMetadata::uv_min_and_extents`].
    CompressedUv,
    /// A full-precision UV: `Float32x2`.
    UncompressedUv,
    /// A vertex color, `Unorm8x4`, stored as it arrives.
    Color,
    /// A unit normal, octahedral-encoded into two `snorm16` lanes.
    ///
    /// A normal needs no decode parameters — the encoding is absolute, unlike
    /// the per-mesh ranges positions and UVs are quantized against — so a
    /// normal never makes its variant read the metadata array. The encoding is
    /// also the only one a stream offers: octahedral `Snorm16x2` resolves a
    /// direction to well under a degree, which three full-precision lanes
    /// would not improve on for shading.
    CompressedNormal,
    /// A joint index, `Uint16x4`. Indices address data structurally rather
    /// than interpolating, so they are never normalized.
    Joints,
    /// A joint weight, `Unorm16x4` in `[0, 1]`.
    Weights,
}

impl ChannelEncoding {
    /// The vertex format the channel is stored in.
    pub const fn format(self) -> wgpu::VertexFormat {
        match self {
            Self::CompressedPosition => wgpu::VertexFormat::Snorm16x4,
            Self::UncompressedPosition => wgpu::VertexFormat::Float32x3,
            Self::PlanarPosition => wgpu::VertexFormat::Float32x2,
            Self::CompressedUv => wgpu::VertexFormat::Snorm16x2,
            Self::UncompressedUv => wgpu::VertexFormat::Float32x2,
            Self::Color => wgpu::VertexFormat::Unorm8x4,
            Self::CompressedNormal => wgpu::VertexFormat::Snorm16x2,
            Self::Joints => wgpu::VertexFormat::Uint16x4,
            Self::Weights => wgpu::VertexFormat::Unorm16x4,
        }
    }

    /// Bytes the channel occupies, which is its attribute's size.
    pub const fn size(self) -> u32 {
        self.format().size() as u32
    }
}

/// The channels one per-vertex stream carries, in the order they are
/// interleaved.
///
/// Each channel is an attribute location paired with its encoding, and the
/// attributes of a wgpu vertex-buffer layout are laid out in exactly this
/// order: sequential offsets from the channel sizes, stride the sum of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct StreamChannels {
    /// The channels, in attribute order.
    channels: arrayvec::ArrayVec<(u32, ChannelEncoding), { StreamChannels::CAPACITY }>,
}

impl StreamChannels {
    /// How many channels one stream can carry.
    pub const CAPACITY: usize = 8;

    /// A stream carrying `channels`, in attribute order.
    ///
    /// # Panics
    ///
    /// If there are more than [`Self::CAPACITY`] of them.
    pub fn new(channels: impl IntoIterator<Item = (u32, ChannelEncoding)>) -> Self {
        Self {
            channels: channels.into_iter().collect(),
        }
    }

    /// Append one channel after the channels already declared.
    ///
    /// # Panics
    ///
    /// If the stream already holds [`Self::CAPACITY`] channels.
    pub fn push(&mut self, channel: (u32, ChannelEncoding)) {
        self.channels
            .try_push(channel)
            .expect("a vertex stream holds at most `StreamChannels::CAPACITY` channels");
    }

    /// The channels in attribute order.
    pub fn iter(&self) -> impl Iterator<Item = &(u32, ChannelEncoding)> {
        self.channels.iter()
    }

    /// Whether no channel is declared, in which case the slot must be left out
    /// of a pipeline's vertex-buffer list.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Bytes per vertex, the sum of the channel sizes.
    pub fn stride(&self) -> u32 {
        self.channels
            .iter()
            .map(|(_, encoding)| encoding.size())
            .sum()
    }

    /// Bytes a `vertex_count`-long stream occupies.
    pub fn byte_len(&self, vertex_count: usize) -> usize {
        vertex_count * self.stride() as usize
    }

    /// The offset each channel's attribute sits at, in attribute order.
    ///
    /// These are the offsets a wgpu vertex-buffer layout declares; a packed
    /// stream writes its channels at the same offsets.
    pub fn attribute_offsets(&self) -> impl Iterator<Item = (u32, ChannelEncoding, u64)> + '_ {
        let mut offset = 0u64;
        self.channels.iter().map(move |&(location, encoding)| {
            let at = offset;
            offset += u64::from(encoding.size());
            (location, encoding, at)
        })
    }

    /// The wgpu vertex-buffer layout these channels describe.
    ///
    /// The attributes are declared in channel order with the offsets
    /// [`Self::attribute_offsets`] reports and the stride [`Self::stride`]
    /// reports, so the layout a pipeline declares and the bytes
    /// [`PositionStreamWriter`]/[`SecondaryStreamWriter`] pack always agree.
    pub fn layout(&self) -> crate::specialize::VertexBufferLayoutDesc {
        let array_stride = u64::from(self.stride());
        assert!(
            array_stride.is_multiple_of(wgpu::VERTEX_ALIGNMENT),
            "vertex stride {array_stride} must be a multiple of `VERTEX_ALIGNMENT`"
        );
        crate::specialize::VertexBufferLayoutDesc {
            array_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: self
                .attribute_offsets()
                .map(
                    |(shader_location, encoding, offset)| wgpu::VertexAttribute {
                        format: encoding.format(),
                        offset,
                        shader_location,
                    },
                )
                .collect(),
        }
    }
}

/// Failure reasons reported by the compression helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CompressError {
    /// The input slice was empty.
    #[error("the input slice is empty")]
    EmptyInput,
    /// A vertex index is at or above the primitive-restart value `0xFFFF`,
    /// so the mesh cannot use 16-bit indices. Carries the offending index.
    #[error("vertex index {0} does not fit in u16")]
    TooManyVertices(u32),
}

/// Per-mesh vertex-decode parameters and draw addressing.
///
/// Mirrors `mesh_metadata.wesl::MeshMetadata`; the layout is checked at
/// compile time against the WGSL storage-buffer alignment rules.
///
/// The struct carries the mesh's addressing as well as its decode parameters,
/// because both are per-mesh and immutable once the mesh is uploaded: a draw
/// reaches them through one index, the metadata index its instance record
/// carries. The addressing scalars fill what the `vec3` members would
/// otherwise leave as padding, so the two together cost no more than the decode
/// parameters alone.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
    const_shader_layout::ShaderLayout,
)]
pub struct MeshMetadata {
    /// Center of the position bounding box.
    pub aabb_center: glam::Vec3,
    /// First element this mesh's vertices occupy in the pool its streams live
    /// in, which is the draw's `firstVertex` or `baseVertex`.
    ///
    /// A shader that has to map a vertex back to its mesh-local ordinal — the
    /// morph deltas, which are stored per mesh — subtracts it from
    /// `@builtin(vertex_index)`.
    pub vertex_offset: u32,
    /// Half-extents of the position bounding box.
    pub aabb_half_extents: glam::Vec3,
    /// How many morph targets follow each vertex in the morph displacement
    /// array.
    ///
    /// Zero for a mesh that declares no morph targets.
    pub morph_count: u32,
    /// `xy` is the UV minimum and `zw` its extents.
    pub uv_min_and_extents: glam::Vec4,
    /// First element this mesh's morph displacements occupy in the frame's
    /// shared `array<f32>`.
    ///
    /// The displacements are the mesh's own geometry, but they live in one
    /// frame-wide array rather than a per-mesh buffer, so a draw names its
    /// slice by an offset like this one instead of binding a resource.
    pub morph_deltas_offset: u32,
    /// Explicit tail padding: the struct's 16-byte alignment rounds its 52 used
    /// bytes up to 64, and an array element is that whole stride.
    pub pad0: [u32; 3],
}

/// One per-instance record: the affine model matrix, base color, pose bases,
/// metadata index, alpha cutoff and inverted-hull width the built-in pipeline
/// reads from its per-instance vertex buffer.
///
/// This is the vertex stream of `lit_vertex`'s instance slot, so upload a
/// `&[MeshInstance]` as that slot's buffer. The matrix is packed as three
/// columns with each column's `.w` holding the matching translation
/// component, which is why it is stored as columns rather than a
/// [`glam::Mat4`]: the shader transforms a point with three dot products and
/// never treats it as a matrix.
///
/// [`Self::joints_base`] and [`Self::morph_base`] address the frame's shared
/// pose arrays, which is what lets two instances of one mesh deform
/// differently: the joints and morph weights a draw deforms by are per-instance
/// state, so they cannot live in the mesh's own bind group. Skinning and
/// morphing are independent, so the two bases are separate fields rather than
/// the lanes of one vector — a variant that only skins reads no weight base.
///
/// Vertex attributes are addressed by the explicit offsets of
/// [`crate::pipeline::LitVariant::vertex_buffer_layouts`], not by WGSL
/// shader-layout rules, so this type makes no `const_shader_layout` claim;
/// that the offsets line up is asserted by a test instead.
///
/// The record is tightly packed — the sum of its vertex formats is its size —
/// which is why the columns are `[f32; 4]` rather than [`glam::Vec4`]: a `Vec4`
/// is sixteen-byte aligned, which would round the record up and leave trailing
/// padding the layout would have to account for.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
)]
pub struct MeshInstance {
    /// Affine model matrix columns, translation in each `.w` lane.
    pub model: [[f32; 4]; 3],
    /// Linear RGBA base color, `Unorm8x4`.
    pub base_color: CompressedColor,
    /// This instance's first joint matrix in the frame's shared joint array.
    pub joints_base: u32,
    /// Index of the mesh's entry in the frame's metadata array, which carries
    /// its decode parameters and its draw addressing.
    ///
    /// The index is a property of the mesh rather than of the instance, but it
    /// rides the instance stream because that is what a draw can reach without
    /// a bind group of its own. Every instance of one mesh carries the same
    /// index.
    pub metadata_index: u32,
    /// Alpha below which a cut-off variant discards the fragment.
    ///
    /// Zero for an instance that cuts nothing off, which no alpha falls below.
    pub cutoff: f32,
    /// This instance's first morph weight in the frame's shared weight array.
    pub morph_base: u32,
    /// How far this instance's vertices are pushed out along their normals, in
    /// world units.
    ///
    /// Zero for an instance that draws the mesh as itself, which is every
    /// instance that is not an
    /// [inverted hull](crate::pipeline::LitOptions::inverted_hull). The width
    /// is per-instance rather than a uniform because one mesh drawn twice —
    /// once as itself, once as the shell behind it — needs two widths from one
    /// pipeline, and because two shells of one mesh may be drawn at different
    /// thicknesses.
    pub hull_width: f32,
}

impl MeshInstance {
    /// Build an instance from a world transform and a linear RGBA base color.
    ///
    /// The color is quantized to the `Unorm8x4` the instance stream carries, so
    /// a component outside `0..=1` saturates rather than wrapping. The instance
    /// reads no pose data, no metadata, no alpha cutoff and expands by no
    /// hull; point it at them with the `with_*` setters.
    pub fn new(world_from_local: glam::Affine3A, base_color: glam::Vec4) -> Self {
        let linear = world_from_local.matrix3;
        let translation = world_from_local.translation;
        Self {
            model: [
                linear.x_axis.extend(translation.x).to_array(),
                linear.y_axis.extend(translation.y).to_array(),
                linear.z_axis.extend(translation.z).to_array(),
            ],
            base_color: base_color.to_array().map(f32_to_unorm8),
            joints_base: 0,
            metadata_index: 0,
            cutoff: 0.0,
            morph_base: 0,
            hull_width: 0.0,
        }
    }

    /// The world-space translation this instance places its mesh at.
    ///
    /// The model matrix stores it in the `.w` lane of each column, so this is
    /// how a caller reads it back out.
    pub fn translation(&self) -> glam::Vec3 {
        glam::Vec3::new(self.model[0][3], self.model[1][3], self.model[2][3])
    }

    /// Point this instance at the joint matrices starting at `joints_base` in
    /// the frame's shared joint array.
    #[must_use]
    pub fn with_joints_base(mut self, joints_base: u32) -> Self {
        self.joints_base = joints_base;
        self
    }

    /// Point this instance at the morph weights starting at `morph_base` in the
    /// frame's shared weight array.
    #[must_use]
    pub fn with_morph_base(mut self, morph_base: u32) -> Self {
        self.morph_base = morph_base;
        self
    }

    /// Point this instance at the mesh metadata entry at `metadata_index`.
    #[must_use]
    pub fn with_metadata_index(mut self, metadata_index: u32) -> Self {
        self.metadata_index = metadata_index;
        self
    }

    /// Discard this instance's fragments below the alpha `cutoff`.
    #[must_use]
    pub fn with_cutoff(mut self, cutoff: f32) -> Self {
        self.cutoff = cutoff;
        self
    }

    /// Push this instance's vertices out along their normals by `hull_width`.
    #[must_use]
    pub fn with_hull_width(mut self, hull_width: f32) -> Self {
        self.hull_width = hull_width;
        self
    }
}

impl Default for MeshInstance {
    /// Identity transform and opaque white.
    fn default() -> Self {
        Self::new(glam::Affine3A::IDENTITY, glam::Vec4::ONE)
    }
}

/// One compressed vertex position: `Snorm16x4` relative to the mesh AABB.
///
/// The `w` lane is unused by the built-in lit shader and is written as `0`.
pub type CompressedPosition = [i16; 4];

/// One compressed vertex UV: `Snorm16x2` remapped by
/// [`MeshMetadata::uv_min_and_extents`].
pub type CompressedUv = [i16; 2];

/// One vertex color: `Unorm8x4`, straight (non-premultiplied) linear RGBA.
pub type CompressedColor = [u8; 4];

/// One compressed vertex normal: `Snorm16x2`, octahedral-encoded.
///
/// The encoding is absolute rather than relative to a per-mesh range, so it
/// needs no decode parameters: a normal survives the round trip through
/// [`compress_normals`] and the `decode_normal` counterpart in the shader
/// without reading any metadata.
pub type CompressedNormal = [i16; 2];

/// One vertex's joint indices: `Uint16x4`.
pub type CompressedJoints = [u16; 4];

/// One vertex's joint weights: `Unorm16x4`, summing to 1.
pub type CompressedWeights = [u16; 4];

/// One joint's skinning matrix: the joint's transform times the inverse of
/// the bind-pose transform, applied to a vertex in the mesh's local space.
///
/// This is the element type of the `array<mat4x4<f32>>` a skinned variant
/// binds at [`JOINTS_BINDING`](crate::pipeline::JOINTS_BINDING), so a mesh
/// uploads a `&[JointMatrix]` there and a vertex's joint index selects one.
pub type JointMatrix = glam::Mat4;

/// The vertex count a stream's enabled channels agree on, or 0 for a stream
/// with no channels.
///
/// # Panics
/// If two enabled channels describe different numbers of vertices.
pub(super) fn vertex_count_of(lengths: &[(&str, Option<usize>)]) -> usize {
    let mut count = None;
    for (channel, length) in lengths.iter() {
        let Some(length) = *length else { continue };
        match count {
            None => count = Some(length),
            Some(expected) => assert_eq!(
                length, expected,
                "the {channel} stream must describe the same vertices as the first channel"
            ),
        }
    }
    count.unwrap_or(0)
}

#[cfg(test)]
mod tests;
