//! Vertex compression and the per-mesh decode metadata.
//!
//! The built-in pipeline consumes a compact vertex layout: positions and UVs
//! are 16-bit normalized integers relative to a per-mesh bounding box, so the
//! shader needs the parameters that reverse that encoding. [`MeshMetadata`]
//! carries those parameters; [`compress_positions`] and [`compress_uvs`]
//! produce them together with the packed attribute streams.
//!
//! The Rust side and the WGSL side (`mesh_metadata.wesl`) are kept in sync by
//! the crate.

use zerocopy::IntoBytes;

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

/// Result type used by the compression helpers.
pub type CompressResult = Result<(), CompressError>;

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
    /// Explicit tail padding.
    pub pad0: u32,
    /// Explicit tail padding.
    pub pad1: u32,
    /// Explicit tail padding.
    pub pad2: u32,
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

    /// An instance at the origin with the given base color.
    pub fn translated(translation: glam::Vec3, base_color: glam::Vec4) -> Self {
        Self::new(glam::Affine3A::from_translation(translation), base_color)
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
/// [`mesh_compression.wesl`](../../shaders/mesh_compression.wesl) reverses.
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

/// Quantize colors to `Unorm8x4`, clamping out-of-range components.
///
/// Computed on demand, so nothing is allocated.
pub fn compress_colors(colors: &[[f32; 4]]) -> impl Iterator<Item = CompressedColor> + '_ {
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

/// Byte view of a compressed UV stream, ready for upload.
pub fn uvs_as_bytes(uvs: &[CompressedUv]) -> &[u8] {
    uvs.as_bytes()
}

/// Byte view of a color stream, ready for upload.
pub fn colors_as_bytes(colors: &[CompressedColor]) -> &[u8] {
    colors.as_bytes()
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn colors_clamp_out_of_range_components() {
        let colors: Vec<_> =
            compress_colors(&[[0.0, 0.5, 1.0, 2.0], [-1.0, 0.0, 0.0, 0.0]]).collect();
        assert_eq!(colors[0], [0, 128, 255, 255]);
        assert_eq!(colors[1], [0, 0, 0, 0]);
    }

    #[test]
    fn weights_clamp_out_of_range_components() {
        let weights: Vec<_> = compress_weights(&[[0.0, 0.5, 1.0, 2.0]]).collect();
        assert_eq!(weights[0], [0, 32_768, 65_535, 65_535]);
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
