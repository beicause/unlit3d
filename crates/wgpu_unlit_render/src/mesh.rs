//! Vertex compression and the per-mesh decode metadata.
//!
//! The built-in pipeline consumes a compact vertex layout: positions and UVs
//! are 16-bit normalized integers relative to a per-mesh bounding box, so the
//! shader needs the parameters that reverse that encoding. [`MeshMetadata`]
//! carries those parameters; [`compress_positions`] and [`compress_uvs`]
//! produce them together with the packed attribute streams.
//!
//! The Rust side and the WGSL side (`mesh_metadata.wesl`) are kept in sync by
//! the crate. [`MeshVertexStreamWriter`] and [`PositionStreamWriter`] pack the
//! per-vertex streams a draw declares, whichever pipeline then draws them.
//!
//! # One description per stream
//!
//! A stream is described by [`StreamChannels`]: one attribute location and
//! [`ChannelEncoding`] per channel, in the order the channels are interleaved.
//! The pipeline's vertex layout, the compression that packs a mesh and the
//! streams a pool allocates all derive their bytes per vertex from that same
//! description, so the three cannot drift apart.

use wgpu::WriteOnly;
use zerocopy::IntoBytes;

/// How one compressed channel is encoded as a vertex attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelEncoding {
    /// An AABB-quantized position: `Snorm16x4`, decoded through
    /// [`MeshMetadata`]'s bounding box.
    CompressedPosition,
    /// A full-precision position: `Float32x3`.
    UncompressedPosition,
    /// A range-remapped UV: `Snorm16x2`, decoded through
    /// [`MeshMetadata::uv_min_and_extents`].
    CompressedUv,
    /// A full-precision UV: `Float32x2`.
    UncompressedUv,
    /// A vertex color, `Unorm8x4`, stored as it arrives.
    Color,
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
            Self::CompressedUv => wgpu::VertexFormat::Snorm16x2,
            Self::UncompressedUv => wgpu::VertexFormat::Float32x2,
            Self::Color => wgpu::VertexFormat::Unorm8x4,
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
    /// [`PositionStreamWriter`]/[`MeshVertexStreamWriter`] pack always agree.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressError {
    /// The input slice was empty.
    EmptyInput,
    /// A vertex index is at or above the primitive-restart value `0xFFFF`,
    /// so the mesh cannot use 16-bit indices. Carries the offending index.
    TooManyVertices(u32),
}

impl core::fmt::Display for CompressError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyInput => write!(f, "the input slice is empty"),
            Self::TooManyVertices(index) => {
                write!(f, "vertex index {index} does not fit in u16")
            }
        }
    }
}

impl std::error::Error for CompressError {}

/// Per-mesh vertex-decode parameters.
///
/// Mirrors `mesh_metadata.wesl::MeshMetadata`; the layout is checked at
/// compile time against the WGSL storage-buffer alignment rules.
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
    /// Explicit alignment padding (`vec3<f32>` requires 16-byte alignment).
    pub pad0: u32,
    /// Half-extents of the position bounding box.
    pub aabb_half_extents: glam::Vec3,
    /// Explicit alignment padding.
    pub pad1: u32,
    /// `xy` is the UV minimum and `zw` its extents.
    pub uv_min_and_extents: glam::Vec4,
}

/// Per-draw addressing into the shared mesh-metadata array.
///
/// Mirrors `mesh_metadata.wesl::MeshInfo` and is bound as `var<uniform>`, so
/// the layout is checked against the WGSL uniform address-space rules. Draws
/// with no metadata of their own still bind this so the global group keeps a
/// stable layout.
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
    const_shader_layout::ShaderLayoutCompat,
)]
pub struct MeshInfo {
    /// Index into the `array<MeshMetadata>` bound with the global group.
    pub metadata_index: u32,
    /// First element this mesh's vertices occupy in the pool its streams live
    /// in, which is the draw's `firstVertex` or `baseVertex`.
    ///
    /// A shader that has to map a vertex back to its mesh-local ordinal — the
    /// morph deltas, which are stored per mesh — subtracts it from
    /// `@builtin(vertex_index)`.
    pub vertex_offset: u32,
    /// How many morph targets follow each vertex in the morph binding.
    ///
    /// Zero without [`UnlitFlags::MORPH_POSITIONS`].
    ///
    /// [`UnlitFlags::MORPH_POSITIONS`]:
    ///     crate::pipeline::UnlitFlags::MORPH_POSITIONS
    pub morph_count: u32,
    /// Explicit tail padding.
    pub pad0: u32,
}

impl MeshInfo {
    /// Address the metadata at `metadata_index`.
    pub fn new(metadata_index: u32) -> Self {
        Self {
            metadata_index,
            ..Default::default()
        }
    }
}

/// One per-instance record: the affine model matrix and base color the
/// built-in pipeline reads from its per-instance vertex buffer.
///
/// This is the vertex stream of `unlit.wesl`'s instance slot, so upload a
/// `&[MeshInstance]` as that slot's buffer. The matrix is packed as three
/// columns with each column's `.w` holding the matching translation
/// component, which is why it is stored as [`glam::Vec4`]s rather than a
/// [`glam::Mat4`]: the shader transforms a point with three dot products and
/// never treats it as a matrix.
///
/// Vertex attributes are addressed by the explicit offsets of
/// [`crate::pipeline::UnlitPipeline::vertex_buffer_layouts`], not by WGSL
/// shader-layout rules, so this type makes no `const_shader_layout` claim;
/// that the offsets line up is asserted by a test instead.
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
    pub model: [glam::Vec4; 3],
    /// Linear RGBA base color.
    pub base_color: glam::Vec4,
}

impl MeshInstance {
    /// Build an instance from a world transform and a linear RGBA base color.
    pub fn new(world_from_local: glam::Affine3A, base_color: glam::Vec4) -> Self {
        let linear = world_from_local.matrix3;
        let translation = world_from_local.translation;
        Self {
            model: [
                linear.x_axis.extend(translation.x),
                linear.y_axis.extend(translation.y),
                linear.z_axis.extend(translation.z),
            ],
            base_color,
        }
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
/// The `w` lane is unused by the built-in unlit shader and is written as `0`.
pub type CompressedPosition = [i16; 4];

/// One compressed vertex UV: `Snorm16x2` remapped by
/// [`MeshMetadata::uv_min_and_extents`].
pub type CompressedUv = [i16; 2];

/// One vertex color: `Unorm8x4`, straight (non-premultiplied) linear RGBA.
pub type CompressedColor = [u8; 4];

/// One vertex's joint indices: `Uint16x4`.
pub type CompressedJoints = [u16; 4];

/// One vertex's joint weights: `Unorm16x4`, summing to 1.
pub type CompressedWeights = [u16; 4];

/// The reciprocal of `extents`, with degenerate axes collapsed to zero.
///
/// A zero-extent axis would otherwise scale by infinity and turn `0 * inf`
/// into NaN, so a collapsed axis has to encode onto the box center instead.
#[inline]
fn encode_scale(extents: glam::Vec3) -> glam::Vec3 {
    glam::Vec3::select(
        (1.0 / extents).is_nan_mask(),
        glam::Vec3::ZERO,
        1.0 / extents,
    )
}

/// The 2D counterpart of [`encode_scale`], for the UV range.
#[inline]
fn encode_scale2(extents: glam::Vec2) -> glam::Vec2 {
    glam::Vec2::select(
        (1.0 / extents).is_nan_mask(),
        glam::Vec2::ZERO,
        1.0 / extents,
    )
}

/// Derive the position decode parameters from `positions` and write them into
/// `out_meta`.
///
/// The parameters are the bounding box the compression needs: positions
/// encode to `[-1, 1]` relative to it, which is what
/// [`mesh_compression.wesl`](../shaders/mesh_compression.wesl) reverses.
///
/// # Panics
/// If `positions` is empty.
fn derive_position_bounds(positions: &[[f32; 3]], out_meta: &mut MeshMetadata) {
    assert!(
        !positions.is_empty(),
        "the position stream must not be empty"
    );

    let mut min = glam::Vec3::splat(f32::INFINITY);
    let mut max = glam::Vec3::splat(f32::NEG_INFINITY);
    for position in positions {
        let position = glam::Vec3::from(*position);
        min = min.min(position);
        max = max.max(position);
    }

    out_meta.aabb_center = (min + max) * 0.5;
    out_meta.aabb_half_extents = (max - min) * 0.5;
}

/// Compress positions into `Snorm16x4` relative to their bounding box,
/// writing the decode parameters into `out_meta`.
///
/// The returned iterator is lazy: nothing is allocated, and a caller can
/// stream the values straight into a GPU buffer.
///
/// # Panics
/// If `positions` is empty.
pub fn compress_positions<'a>(
    positions: &'a [[f32; 3]],
    out_meta: &mut MeshMetadata,
) -> impl Iterator<Item = CompressedPosition> + 'a {
    derive_position_bounds(positions, out_meta);
    let center = out_meta.aabb_center;
    let half = out_meta.aabb_half_extents;
    let scale = encode_scale(half);
    positions.iter().map(move |position| {
        let encoded = (glam::Vec3::from(*position) - center) * scale;
        f32_to_snorm16(encoded.extend(0.0).to_array())
    })
}

/// Derive the UV decode parameters from `uvs` and write them into
/// `out_meta.uv_min_and_extents`.
///
/// # Panics
/// If `uvs` is empty.
fn derive_uv_bounds(uvs: &[[f32; 2]], out_meta: &mut MeshMetadata) {
    assert!(!uvs.is_empty(), "the UV stream must not be empty");

    let mut min = glam::Vec2::splat(f32::INFINITY);
    let mut max = glam::Vec2::splat(f32::NEG_INFINITY);
    for uv in uvs {
        let uv = glam::Vec2::from(*uv);
        min = min.min(uv);
        max = max.max(uv);
    }

    let extents = max - min;
    out_meta.uv_min_and_extents = glam::Vec4::new(min.x, min.y, extents.x, extents.y);
}

/// Compress UVs into `Snorm16x2` remapped by their bounding box, writing the
/// decode parameters into `out_meta.uv_min_and_extents`.
///
/// The returned iterator is lazy: nothing is allocated, and a caller can
/// stream the values straight into a GPU buffer.
///
/// # Panics
/// If `uvs` is empty.
pub fn compress_uvs<'a>(
    uvs: &'a [[f32; 2]],
    out_meta: &mut MeshMetadata,
) -> impl Iterator<Item = CompressedUv> + 'a {
    derive_uv_bounds(uvs, out_meta);
    let range = out_meta.uv_min_and_extents;
    let min = glam::Vec2::new(range.x, range.y);
    let extents = glam::Vec2::new(range.z, range.w);
    let scale = encode_scale2(extents);
    uvs.iter().map(move |uv| {
        // Remap [0, 1] to [-1, 1] so the value can use the signed format the
        // shader decodes with `mesh_compression::decode_uv`.
        let encoded = (glam::Vec2::from(*uv) - min) * scale * 2.0 - glam::Vec2::ONE;
        f32_to_snorm16(encoded.to_array())
    })
}

/// The compressed UVs of `uvs`, encoded against `out_meta`'s existing
/// parameters without re-deriving them.
///
/// Useful to reuse one UV range across several meshes; pair it with
/// [`compress_uvs`] on the first mesh to fill in `out_meta`.
pub fn encode_uvs<'a>(
    uvs: &'a [[f32; 2]],
    out_meta: &MeshMetadata,
) -> impl Iterator<Item = CompressedUv> + 'a {
    let range = out_meta.uv_min_and_extents;
    let min = glam::Vec2::new(range.x, range.y);
    let extents = glam::Vec2::new(range.z, range.w);
    let scale = encode_scale2(extents);
    uvs.iter().map(move |uv| {
        let encoded = (glam::Vec2::from(*uv) - min) * scale * 2.0 - glam::Vec2::ONE;
        f32_to_snorm16(encoded.to_array())
    })
}

/// Quantize `[f32; 4]` colors to `Unorm8x4`, clamping out-of-range
/// components.
///
/// Colors are normally already `[u8; 4]` and need no conversion; this is
/// for callers whose colors come from a float source such as a [glam::Vec4].
/// Computed on demand, so nothing is allocated.
pub fn quantize_colors(colors: &[[f32; 4]]) -> impl Iterator<Item = CompressedColor> + '_ {
    colors.iter().map(|color| color.map(f32_to_unorm8))
}

/// Quantize one normalized `f32` to `Unorm8`.
#[inline]
fn f32_to_unorm8(component: f32) -> u8 {
    (component.clamp(0.0, 1.0) * u8::MAX as f32).round() as u8
}

/// Quantize one normalized `f32` to `Unorm16`.
#[inline]
fn f32_to_unorm16(component: f32) -> u16 {
    (component.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16
}

/// Quantize joint weights to `Unorm16x4`, clamping out-of-range components.
///
/// The caller is responsible for weights that already sum to 1; this helper
/// does not renormalize. Computed on demand, so nothing is allocated.
pub fn compress_weights(weights: &[[f32; 4]]) -> impl Iterator<Item = CompressedWeights> + '_ {
    weights.iter().map(|weight| weight.map(f32_to_unorm16))
}

/// Narrow `u32` indices to `u16`.
///
/// `0xFFFF` is the primitive-restart value, so a valid vertex index must stay
/// strictly below it (the largest usable index is `0xFFFE`).
///
/// Unlike the vertex-attribute compressors this returns a [`Result`], because
/// the input can genuinely be too large for the output format; the check runs
/// over the whole input before any value is produced.
pub fn compress_indices(indices: &[u32]) -> Result<impl Iterator<Item = u16> + '_, CompressError> {
    if indices.is_empty() {
        return Err(CompressError::EmptyInput);
    }
    if let Some(&index) = indices.iter().find(|&&index| index >= u16::MAX as u32) {
        return Err(CompressError::TooManyVertices(index));
    }
    Ok(indices.iter().map(|&index| index as u16))
}

/// Byte view of a compressed position stream, ready for upload.
pub fn positions_as_bytes(positions: &[CompressedPosition]) -> &[u8] {
    positions.as_bytes()
}

/// Byte view of a joint-index stream, ready for upload.
pub fn joints_as_bytes(joints: &[CompressedJoints]) -> &[u8] {
    joints.as_bytes()
}

/// Byte view of a joint-weight stream, ready for upload.
pub fn weights_as_bytes(weights: &[CompressedWeights]) -> &[u8] {
    weights.as_bytes()
}

/// Quantize `[-1, 1]` floats to signed 16-bit normalized integers.
#[inline]
fn f32_to_snorm16<const N: usize>(value: [f32; N]) -> [i16; N] {
    value.map(|component| (component.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
}

bitflags::bitflags! {
    /// The optional per-vertex channels of a UV-and-color vertex stream.
    ///
    /// A stream is described by what it carries rather than by which pipeline
    /// consumes it, so packing a stream needs nothing the built-in pipeline
    /// defines: the built-in pipeline derives one of these from its own
    /// variant, and a custom pipeline declares one directly.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct UvColorFlags: u8 {
        /// The stream carries a per-vertex UV.
        const UV = 1 << 0;
        /// The UV is written full precision -- `Float32x2` -- rather than
        /// compressed to `Snorm16x2`.
        ///
        /// An uncompressed UV needs no decode parameters, so writing it
        /// leaves [`crate::mesh::MeshMetadata`] alone.
        const UNCOMPRESSED_UV = 1 << 1;
        /// The stream carries a per-vertex color.
        const COLOR = 1 << 2;
    }
}

/// The optional per-vertex channels of a UV-and-color vertex stream.
///
/// Each channel adds one attribute to [`crate::pipeline::UV_COLOR_SLOT`], in
/// the order UV then color, so the packed stream matches the shader's declared
/// locations for any combination. [`Self::write`] compresses the raw
/// attributes and interleaves them straight into the target buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshVertexStreamWriter {
    /// The channels this stream writes.
    pub flags: UvColorFlags,
}

impl MeshVertexStreamWriter {
    /// Whether the UV attribute is written.
    pub fn uv(&self) -> bool {
        self.flags.contains(UvColorFlags::UV)
    }

    /// Whether the color attribute is written.
    pub fn color(&self) -> bool {
        self.flags.contains(UvColorFlags::COLOR)
    }

    /// Whether the UV is written full precision rather than compressed to
    /// `Snorm16x2`.
    ///
    /// An uncompressed UV needs no decode parameters, so [`Self::write`]
    /// leaves the metadata alone.
    pub fn uncompressed_uv(&self) -> bool {
        self.flags.contains(UvColorFlags::UNCOMPRESSED_UV)
    }

    /// Whether the variant declares no attributes at all, in which case the
    /// slot must be omitted from the vertex-buffer list.
    pub fn is_empty(&self) -> bool {
        !self.uv() && !self.color()
    }

    /// The channels this stream carries, in attribute order.
    ///
    /// The single description the stride, the attribute offsets and the packed
    /// bytes are all derived from.
    pub fn channels(&self) -> StreamChannels {
        let mut channels = StreamChannels::default();
        if self.uv() {
            channels.push((
                crate::pipeline::location::UV,
                if self.uncompressed_uv() {
                    ChannelEncoding::UncompressedUv
                } else {
                    ChannelEncoding::CompressedUv
                },
            ));
        }
        if self.color() {
            channels.push((crate::pipeline::location::COLOR, ChannelEncoding::Color));
        }
        channels
    }

    /// Bytes per vertex of the packed stream.
    pub fn stride(&self) -> u32 {
        self.channels().stride()
    }

    /// Bytes one `vertex_count`-long stream occupies.
    pub fn byte_len(&self, vertex_count: usize) -> usize {
        self.channels().byte_len(vertex_count)
    }

    /// Compress the raw `uvs` and write the interleaved result, together
    /// with the already compressed `colors`, into `out` — which must be
    /// exactly [`Self::byte_len`]`(vertex_count)` bytes, normally a
    /// mapped-at-creation vertex buffer, so the vertices land in GPU memory
    /// without an intermediate byte buffer.
    ///
    /// Colors are taken as [`CompressedColor`] because that is what the
    /// stream stores; float colors from a [glam::Vec4] can be quantized with
    /// [`quantize_colors`] first.
    ///
    /// `metadata` receives the UV decode parameters the compression derives,
    /// exactly as [`compress_uvs`] would.
    ///
    /// Nothing is allocated: the compressed UVs are produced lazily and
    /// written as they are computed. Use [`Self::write_compressed`] to write
    /// channels that are already compressed, for example to share one stream
    /// across several meshes.
    ///
    /// # Panics
    /// If an enabled channel has no matching slice, if the channel lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write(
        &self,
        uvs: &[[f32; 2]],
        colors: &[CompressedColor],
        metadata: &mut MeshMetadata,
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = self.raw_vertex_count(uvs, colors);
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );
        if vertex_count == 0 {
            return;
        }

        // Deriving the UV range is the only pass over the input; the encoding
        // itself streams straight into `out`. An uncompressed channel is
        // copied as it is, so it never derives metadata.
        let (write_uv, write_color) = (self.uv(), self.color());
        let compress_uv = write_uv && !self.uncompressed_uv();
        let stride = self.stride() as usize;
        let mut packed_uvs = compress_uv
            .then(|| compress_uvs(uvs, metadata))
            .into_iter()
            .flatten();

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_UV_COLOR_STRIDE];
            let mut len = 0;
            if write_uv {
                // A compressed UV is produced lazily as a temporary, so it
                // is copied out while it is still alive; an uncompressed one
                // is the caller's `f32` pair verbatim.
                if compress_uv {
                    let uv = packed_uvs.next().expect("one UV per vertex");
                    let bytes = uv.as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                } else {
                    let bytes = uvs[index].as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
            }
            if write_color {
                let bytes = colors[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, stride);
            vertex.into_iter().take(len)
        }));
    }

    /// Write the already compressed `uvs` and `colors` into `out` as the
    /// interleaved vertex stream, which must be exactly
    /// [`Self::byte_len`]`(vertex_count)` bytes.
    ///
    /// Takes the packed form produced by [`compress_uvs`] and
    /// [`quantize_colors`], so one compressed stream can be
    /// uploaded for several meshes without recompressing it.
    ///
    /// # Panics
    /// If an enabled channel has no matching slice, if the channel lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write_compressed(
        &self,
        uvs: &[CompressedUv],
        colors: &[CompressedColor],
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = self.compressed_vertex_count(uvs, colors);
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out, so writing never allocates at all.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_UV_COLOR_STRIDE];
            let mut len = 0;
            if self.uv() {
                let bytes = uvs[index].as_bytes();
                vertex[..bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            if self.color() {
                let bytes = colors[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, self.stride() as usize);
            vertex.into_iter().take(len)
        }));
    }

    /// Number of vertices the raw channel slices describe.
    fn raw_vertex_count(&self, uvs: &[[f32; 2]], colors: &[CompressedColor]) -> usize {
        match (self.uv(), self.color()) {
            (true, true) => {
                assert_eq!(
                    uvs.len(),
                    colors.len(),
                    "the UV and color streams must describe the same vertices"
                );
                uvs.len()
            }
            (true, false) => uvs.len(),
            (false, true) => colors.len(),
            (false, false) => 0,
        }
    }

    /// Number of vertices the compressed channel slices describe.
    fn compressed_vertex_count(&self, uvs: &[CompressedUv], colors: &[CompressedColor]) -> usize {
        match (self.uv(), self.color()) {
            (true, true) => {
                assert_eq!(
                    uvs.len(),
                    colors.len(),
                    "the UV and color streams must describe the same vertices"
                );
                uvs.len()
            }
            (true, false) => uvs.len(),
            (false, true) => colors.len(),
            (false, false) => 0,
        }
    }
}

/// One joint's skinning matrix: the joint's transform times the inverse of
/// the bind-pose transform, applied to a vertex in the mesh's local space.
///
/// This is the element type of the `array<mat4x4<f32>>` a skinned variant
/// binds at [`JOINTS_BINDING`](crate::pipeline::JOINTS_BINDING), so a mesh
/// uploads a `&[JointMatrix]` there and a vertex's joint index selects one.
pub type JointMatrix = glam::Mat4;

/// Bytes per vertex of the widest UV-and-color stream, used to size the
/// per-vertex scratch the writer assembles on the stack.
///
/// The widest encoding the stream can write is an uncompressed UV followed by
/// the color, so the scratch covers those two attribute formats.
const MAX_UV_COLOR_STRIDE: usize =
    wgpu::VertexFormat::Float32x2.size() as usize + wgpu::VertexFormat::Unorm8x4.size() as usize;

/// The channels of the position vertex stream: a position and, for a skinned
/// mesh, the joint indices and weights that deform it.
///
/// Joints and weights share this stream rather than getting one of their own,
/// so a skinned mesh's per-vertex data stays one buffer and one stride. The
/// joint pair is addressed by vertex index rather than by attribute, so
/// nothing else about a draw changes when the stream carries it.
///
/// The order is the attribute order: position, joint indices, joint weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PositionStreamChannels {
    /// How the position itself is encoded, or `None` for a variant that reads
    /// no position at all — its geometry is a single point at the instance
    /// origin, which is what a point or impostor draw wants.
    pub position: Option<ChannelEncoding>,
    /// Whether the stream carries joint indices and weights.
    pub joints: bool,
}

impl PositionStreamChannels {
    /// The channels of the stream, in attribute order.
    pub fn channels(&self) -> StreamChannels {
        let mut channels = StreamChannels::default();
        if let Some(position) = self.position {
            channels.push((crate::pipeline::location::POSITION, position));
        }
        if self.joints {
            channels.push((crate::pipeline::location::JOINTS, ChannelEncoding::Joints));
            channels.push((
                crate::pipeline::location::JOINTS_WEIGHTS,
                ChannelEncoding::Weights,
            ));
        }
        channels
    }
}

/// Packs the position vertex stream: the compressed position and, when the
/// stream declares them, the joint indices and weights that deform it.
///
/// Joints and weights share the position's stream, so a skinned mesh's draw
/// names one buffer for all three channels. Positions are compressed against
/// the mesh's bounding box exactly as [`compress_positions`] does; the joint
/// pair arrives already in its stored widths and is copied through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PositionStreamWriter {
    /// The channels this stream writes.
    pub channels: PositionStreamChannels,
}

impl PositionStreamWriter {
    /// Whether the stream carries joint indices and weights.
    pub fn joints(&self) -> bool {
        self.channels.joints
    }

    /// Whether the position is written full precision rather than compressed
    /// to `Snorm16x4`.
    pub fn uncompressed_position(&self) -> bool {
        matches!(
            self.channels.position,
            Some(ChannelEncoding::UncompressedPosition)
        )
    }

    /// Bytes per vertex of the packed stream.
    pub fn stride(&self) -> u32 {
        self.channels.channels().stride()
    }

    /// Bytes one `vertex_count`-long stream occupies.
    pub fn byte_len(&self, vertex_count: usize) -> usize {
        self.channels.channels().byte_len(vertex_count)
    }

    /// Compress the raw `positions` and write the interleaved result, together
    /// with the already compressed joint pair, into `out` — which must be
    /// exactly [`Self::byte_len`]`(vertex_count)` bytes.
    ///
    /// `metadata` receives the bounding box the position compression derives,
    /// exactly as [`compress_positions`] would.
    ///
    /// `joints` and `weights` are taken as their stored widths, because that
    /// is what the stream stores; float weights can be quantized with
    /// [`compress_weights`] first. A stream that declares no joints ignores
    /// both slices.
    ///
    /// # Panics
    ///
    /// If a declared channel has no matching slice, if the slice lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write(
        &self,
        positions: &[[f32; 3]],
        joints: &[CompressedJoints],
        weights: &[CompressedWeights],
        metadata: &mut MeshMetadata,
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = positions.len();
        if self.joints() {
            assert_eq!(
                joints.len(),
                vertex_count,
                "the joint stream must describe the same vertices as the positions"
            );
            assert_eq!(
                weights.len(),
                vertex_count,
                "the weight stream must describe the same vertices as the positions"
            );
        }
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );
        if vertex_count == 0 {
            return;
        }

        // Deriving the bounding box is the only pass over the input; the
        // encoding itself streams straight into `out`. An uncompressed
        // position is copied as it is, so it never derives metadata.
        let write_position = self.channels.position;
        let uncompressed = self.uncompressed_position();
        let write_joints = self.joints();
        let stride = self.stride() as usize;
        let mut packed_positions = (!uncompressed && write_position.is_some())
            .then(|| compress_positions(positions, metadata))
            .into_iter()
            .flatten();

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_POSITION_STRIDE];
            let mut len = 0;
            match write_position {
                Some(_) if uncompressed => {
                    let bytes = positions[index].as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
                Some(_) => {
                    let position = packed_positions.next().expect("one position per vertex");
                    let bytes = position.as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
                // No position channel: the stream carries whatever follows,
                // which is nothing unless the variant also reads joints.
                None => {}
            }
            if write_joints {
                let bytes = joints[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
                let bytes = weights[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, stride);
            vertex.into_iter().take(len)
        }));
    }
}

/// The joint indices and weights of one vertex, as the position stream stores
/// them.
///
/// The pair shares the position's stream, so this is the layout the shader's
/// `VertexInput` declares for them: `Uint16x4` indices followed by `Unorm16x4`
/// weights. The two are written together and never separately, which is why
/// the stream carries them as one unit.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
)]
pub struct JointPair {
    /// Joint indices into the mesh's joint matrices, `Uint16x4`.
    pub joints: CompressedJoints,
    /// Joint weights, `Unorm16x4`, which should sum to 1.
    pub weights: CompressedWeights,
}

impl JointPair {
    /// Bytes one pair occupies, the sum of the two channels.
    pub const SIZE: u32 = ChannelEncoding::Joints.size() + ChannelEncoding::Weights.size();
}

/// Bytes per vertex of the widest position stream, used to size the per-vertex
/// scratch the writer assembles on the stack.
///
/// The widest encoding is an uncompressed position followed by the joint pair.
const MAX_POSITION_STRIDE: usize =
    wgpu::VertexFormat::Float32x3.size() as usize + JointPair::SIZE as usize;

#[cfg(test)]
mod tests {
    use super::*;

    /// Write raw UVs and compressed colors through the mapped-buffer path
    /// and return the bytes.
    fn write_stream(
        stream: MeshVertexStreamWriter,
        uvs: &[[f32; 2]],
        colors: &[CompressedColor],
        vertex_count: usize,
        metadata: &mut MeshMetadata,
    ) -> Vec<u8> {
        let mut out = vec![0u8; stream.byte_len(vertex_count)];
        stream.write(
            uvs,
            colors,
            metadata,
            WriteOnly::from_mut(out.as_mut_slice()),
        );
        out
    }

    /// Both writers must produce byte-identical streams, since the raw one
    /// compresses its UVs into exactly what the compressed one expects and
    /// both copy the colors verbatim.
    #[test]
    fn raw_and_compressed_writers_agree() {
        let uvs = [[0.0f32, 0.25], [0.5, 1.0], [1.0, 0.0]];
        let colors: Vec<CompressedColor> = quantize_colors(&[
            [0.0f32, 0.25, 0.5, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.2, 0.4, 0.6, 0.8],
        ])
        .collect();

        for stream in [
            MeshVertexStreamWriter {
                flags: UvColorFlags::UV | UvColorFlags::COLOR,
            },
            MeshVertexStreamWriter {
                flags: UvColorFlags::UV,
            },
            MeshVertexStreamWriter {
                flags: UvColorFlags::COLOR,
            },
        ] {
            let mut metadata = MeshMetadata::default();
            let from_raw = write_stream(stream, &uvs, &colors, 3, &mut metadata);

            // Compress separately, exactly as a caller sharing one stream
            // across meshes would.
            let packed_uvs: Vec<_> = if stream.uv() {
                compress_uvs(&uvs, &mut metadata).collect()
            } else {
                Vec::new()
            };
            let packed_colors: Vec<_> = if stream.color() {
                colors.clone()
            } else {
                Vec::new()
            };
            let mut from_compressed = vec![0u8; stream.byte_len(3)];
            stream.write_compressed(
                &packed_uvs,
                &packed_colors,
                WriteOnly::from_mut(from_compressed.as_mut_slice()),
            );

            assert_eq!(from_raw, from_compressed, "stream {stream:?}");
            assert_eq!(from_raw.len(), stream.byte_len(3), "stream {stream:?}");
        }
    }

    #[test]
    fn uv_color_stream_interleaves_in_attribute_order() {
        let uvs = [[0.0f32, 0.0], [1.0, 1.0]];
        // Colors are linear RGBA in [0, 1]; they quantize to Unorm8.
        let colors = [[0u8, 64, 128, 255], [255, 0, 0, 255]];
        let mut metadata = MeshMetadata::default();

        let out = write_stream(
            MeshVertexStreamWriter {
                flags: UvColorFlags::UV | UvColorFlags::COLOR,
            },
            &uvs,
            &colors,
            2,
            &mut metadata,
        );
        assert_eq!(out.len(), 2 * 8);
        // Vertex 0 starts with the UV then the color, matching the shader's
        // location order 1 then 2. The UV remaps [0, 1] to Snorm16 [-1, 1],
        // so 0.0 quantizes to -32767 (little-endian) and 1.0 to 32767.
        let expected_uv = [(-32767i16).to_le_bytes(), (-32767i16).to_le_bytes()].concat();
        assert_eq!(&out[0..4], &expected_uv[..]);
        assert_eq!(&out[4..8], &[0, 64, 128, 255]);
        // The second vertex sits at the far corner of the UV range.
        assert_eq!(
            &out[8..12],
            &[32767i16.to_le_bytes(), 32767i16.to_le_bytes()].concat()
        );

        // The UV compression still produced the decode parameters.
        assert_eq!(
            metadata.uv_min_and_extents,
            glam::Vec4::new(0.0, 0.0, 1.0, 1.0)
        );

        // UV only: four bytes per vertex.
        let out = write_stream(
            MeshVertexStreamWriter {
                flags: UvColorFlags::UV,
            },
            &uvs,
            &[],
            2,
            &mut MeshMetadata::default(),
        );
        assert_eq!(out.len(), 2 * 4);

        // Color only: four bytes per vertex, no UV bytes.
        let out = write_stream(
            MeshVertexStreamWriter {
                flags: UvColorFlags::COLOR,
            },
            &[],
            &colors,
            2,
            &mut MeshMetadata::default(),
        );
        assert_eq!(out.len(), 2 * 4);
        assert_eq!(&out[0..4], &[0, 64, 128, 255]);
    }

    /// An uncompressed UV is copied at full precision: it neither derives
    /// metadata nor quantizes, so the bytes are the input `f32`s.
    #[test]
    fn uncompressed_uv_is_written_at_full_precision() {
        let uvs = [[0.0f32, 0.0], [0.5, 0.75]];
        let stream = MeshVertexStreamWriter {
            flags: UvColorFlags::UV | UvColorFlags::UNCOMPRESSED_UV,
        };
        let mut metadata = MeshMetadata::default();
        let out = write_stream(stream, &uvs, &[], 2, &mut metadata);

        let stride = wgpu::VertexFormat::Float32x2.size() as usize;
        assert_eq!(out.len(), 2 * stride);
        for (vertex, uv) in out.chunks(stride).zip(&uvs) {
            assert_eq!(vertex, uv.as_bytes());
        }
        // No compression happened, so the metadata is untouched.
        assert_eq!(metadata, MeshMetadata::default());
    }

    /// Decode exactly like `mesh_compression.wesl::decode_position`.
    fn decode_position(encoded: CompressedPosition, metadata: &MeshMetadata) -> glam::Vec3 {
        let encode = glam::Vec3::new(
            encoded[0] as f32 / i16::MAX as f32,
            encoded[1] as f32 / i16::MAX as f32,
            encoded[2] as f32 / i16::MAX as f32,
        );
        metadata.aabb_center + encode * metadata.aabb_half_extents
    }

    /// Decode exactly like `mesh_compression.wesl::decode_uv`.
    fn decode_uv(encoded: CompressedUv, metadata: &MeshMetadata) -> glam::Vec2 {
        let encode = glam::Vec2::new(
            encoded[0] as f32 / i16::MAX as f32,
            encoded[1] as f32 / i16::MAX as f32,
        );
        let t = encode * 0.5 + 0.5;
        glam::Vec2::new(metadata.uv_min_and_extents.x, metadata.uv_min_and_extents.y)
            + t * glam::Vec2::new(metadata.uv_min_and_extents.z, metadata.uv_min_and_extents.w)
    }

    #[test]
    fn position_roundtrip_stays_within_quantization_error() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [-1.0, 0.5, 2.0]];
        let mut metadata = MeshMetadata::default();
        let encoded: Vec<_> = compress_positions(&positions, &mut metadata).collect();

        assert_eq!(encoded.len(), positions.len());
        for (position, packed) in positions.iter().zip(&encoded) {
            let decoded = decode_position(*packed, &metadata);
            let expected = glam::Vec3::from(*position);
            // One quantization step of the box's half-extent.
            let tolerance = metadata.aabb_half_extents.max_element() * 2.0 / i16::MAX as f32;
            assert!(
                (decoded - expected).abs().max_element() <= tolerance,
                "{decoded} vs {expected}"
            );
        }
        assert_eq!(metadata.aabb_center, glam::Vec3::new(0.0, 1.0, 1.5));
        assert_eq!(metadata.aabb_half_extents, glam::Vec3::new(1.0, 1.0, 1.5));
    }

    #[test]
    fn degenerate_position_axis_collapses_to_center() {
        // All vertices share z = 4.0, so that axis has zero extent.
        let positions = [[0.0, 0.0, 4.0], [1.0, 1.0, 4.0]];
        let mut metadata = MeshMetadata::default();
        let encoded: Vec<_> = compress_positions(&positions, &mut metadata).collect();

        assert_eq!(metadata.aabb_half_extents.z, 0.0);
        assert!(encoded.iter().all(|packed| packed[2] == 0));
        assert_eq!(decode_position(encoded[0], &metadata).z, 4.0);
    }

    #[test]
    fn uv_roundtrip_stays_within_quantization_error() {
        let uvs = [[0.25, 0.5], [1.0, 0.75], [-0.5, 2.0]];
        let mut metadata = MeshMetadata::default();
        let encoded: Vec<_> = compress_uvs(&uvs, &mut metadata).collect();

        assert_eq!(encoded.len(), uvs.len());
        for (uv, packed) in uvs.iter().zip(&encoded) {
            let decoded = decode_uv(*packed, &metadata);
            let expected = glam::Vec2::from(*uv);
            let extents =
                glam::Vec2::new(metadata.uv_min_and_extents.z, metadata.uv_min_and_extents.w);
            let tolerance = extents.max_element() * 2.0 / i16::MAX as f32;
            assert!(
                (decoded - expected).abs().max_element() <= tolerance,
                "{decoded} vs {expected}"
            );
        }
    }

    #[test]
    fn quantize_colors_clamp_out_of_range_components() {
        let colors: Vec<_> =
            quantize_colors(&[[0.0, 0.5, 1.0, 2.0], [-1.0, 0.0, 0.0, 0.0]]).collect();
        assert_eq!(colors[0], [0, 128, 255, 255]);
        assert_eq!(colors[1], [0, 0, 0, 0]);
    }

    #[test]
    fn weights_clamp_out_of_range_components() {
        let weights: Vec<_> = compress_weights(&[[0.0, 0.5, 1.0, 2.0]]).collect();
        assert_eq!(weights[0], [0, 32_768, 65_535, 65_535]);
    }

    /// The joint pair follows the position in the same stream, in the shader's
    /// attribute order, and a stream that declares no joints stays as narrow as
    /// the position alone.
    #[test]
    fn position_stream_appends_the_joint_pair_after_the_position() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
        let joints = [[1u16, 2, 3, 4], [5, 6, 7, 8]];
        let weights: Vec<_> =
            compress_weights(&[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]]).collect();

        let mut metadata = MeshMetadata::default();
        let plain = PositionStreamWriter {
            channels: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: false,
            },
        };
        assert_eq!(plain.stride(), ChannelEncoding::CompressedPosition.size());

        let skinned = PositionStreamWriter {
            channels: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: true,
            },
        };
        assert_eq!(
            skinned.stride(),
            ChannelEncoding::CompressedPosition.size() + JointPair::SIZE
        );

        let mut out = vec![0u8; skinned.byte_len(2)];
        skinned.write(
            &positions,
            &joints,
            &weights,
            &mut metadata,
            WriteOnly::from_mut(out.as_mut_slice()),
        );

        // The position comes first, exactly as the plain stream writes it, so
        // the joint pair cannot disturb the decode the metadata describes.
        let mut plain_out = vec![0u8; plain.byte_len(2)];
        plain.write(
            &positions,
            &[],
            &[],
            &mut MeshMetadata::default(),
            WriteOnly::from_mut(plain_out.as_mut_slice()),
        );
        let position_size = ChannelEncoding::CompressedPosition.size() as usize;
        let pair_size = JointPair::SIZE as usize;
        let stride = skinned.stride() as usize;
        for vertex in 0..2 {
            assert_eq!(
                &out[vertex * stride..vertex * stride + position_size],
                &plain_out[vertex * position_size..(vertex + 1) * position_size],
                "vertex {vertex} position"
            );
            let pair =
                &out[vertex * stride + position_size..vertex * stride + position_size + pair_size];
            assert_eq!(&pair[..8], joints[vertex].as_bytes());
            assert_eq!(&pair[8..], weights[vertex].as_bytes());
        }

        // A stream with neither a position nor joints writes nothing at all.
        let empty = PositionStreamWriter {
            channels: PositionStreamChannels {
                position: None,
                joints: false,
            },
        };
        assert_eq!(empty.stride(), 0);
        assert_eq!(empty.byte_len(2), 0);
    }

    #[test]
    fn indices_reject_primitive_restart_and_overflow() {
        assert_eq!(
            compress_indices(&[0, 65_536]).err(),
            Some(CompressError::TooManyVertices(65_536))
        );
        // 0xFFFF is the primitive-restart value and cannot name a vertex.
        assert_eq!(
            compress_indices(&[0, u16::MAX as u32]).err(),
            Some(CompressError::TooManyVertices(u16::MAX as u32))
        );
        assert_eq!(compress_indices(&[]).err(), Some(CompressError::EmptyInput));

        let indices: Vec<_> = compress_indices(&[0, u16::MAX as u32 - 1])
            .expect("valid indices")
            .collect();
        assert_eq!(indices, vec![0, u16::MAX - 1]);
    }

    #[test]
    fn empty_input_is_rejected() {
        let mut metadata = MeshMetadata::default();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compress_positions(&[], &mut metadata).count()
            }))
            .is_err(),
            "an empty position stream must be rejected"
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compress_uvs(&[], &mut metadata).count()
            }))
            .is_err(),
            "an empty UV stream must be rejected"
        );
    }
}
