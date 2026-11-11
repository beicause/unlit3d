//! The vertex-attribute compressors: bounding-box quantization, octahedral
//! normals and the index narrowing that decides an index buffer width.

use super::{
    CompressError, CompressedColor, CompressedNormal, CompressedPosition, CompressedUv,
    CompressedWeights, MeshMetadata,
};

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

/// Octahedral-encode a unit normal into two `[-1, 1]` lanes.
///
/// The octahedral map folds the sphere onto a square: a direction is
/// projected along `z` onto the octahedron's upper half, and the lower half
/// is folded over by mirroring about the diagonal. Every direction lands on a
/// distinct point of the square with no pole singularity, which is why two
/// lanes suffice for a normal where a plain `xy` projection would lose the
/// hemisphere it folds.
#[inline]
fn octahedral_encode(normal: glam::Vec3) -> glam::Vec2 {
    let normal = normal.normalize_or_zero();
    let l1 = normal.x.abs() + normal.y.abs() + normal.z.abs();
    // A zero normal has no direction to encode; it collapses to the center of
    // the square, which decodes back to `+z`, rather than dividing by zero.
    let projected = normal.truncate() / if l1 == 0.0 { 1.0 } else { l1 };
    if normal.z < 0.0 {
        (glam::Vec2::ONE - glam::Vec2::new(projected.y.abs(), projected.x.abs()))
            * projected.signum()
    } else {
        projected
    }
}

/// Octahedral-encode `normals` into `Snorm16x2` lanes.
///
/// The encoding is absolute, so it needs no per-mesh range and leaves
/// [`MeshMetadata`] alone. The returned iterator is lazy: nothing is
/// allocated, and a caller can stream the values straight into a GPU buffer.
pub fn compress_normals(normals: &[[f32; 3]]) -> impl Iterator<Item = CompressedNormal> + '_ {
    normals
        .iter()
        .map(|normal| f32_to_snorm16(octahedral_encode(glam::Vec3::from(*normal)).to_array()))
}

/// Quantize one normalized `f32` to `Unorm8`.
#[inline]
pub(super) fn f32_to_unorm8(component: f32) -> u8 {
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
pub fn compress_weights(
    weights: &[[f32; 4]],
) -> impl ExactSizeIterator<Item = CompressedWeights> + '_ {
    weights.iter().map(|weight| weight.map(f32_to_unorm16))
}

/// Whether a vertex index can be named by a `Uint16` index buffer.
///
/// `0xFFFF` is the primitive-restart value rather than a vertex index, so a
/// `Uint16` buffer can name at most `0xFFFE` and an index at or above
/// `u16::MAX` needs a `Uint32` one.
pub const fn index_fits_u16(index: u32) -> bool {
    index < u16::MAX as u32
}

/// Narrow `u32` indices to `u16`.
///
/// Which indices can be narrowed is [`index_fits_u16`]'s rule. Unlike the
/// vertex-attribute compressors this returns a [`Result`], because the input
/// can genuinely be too large for the output format; the check runs over the
/// whole input before any value is produced.
pub fn compress_indices(indices: &[u32]) -> Result<impl Iterator<Item = u16> + '_, CompressError> {
    if indices.is_empty() {
        return Err(CompressError::EmptyInput);
    }
    if let Some(&index) = indices.iter().find(|&&index| !index_fits_u16(index)) {
        return Err(CompressError::TooManyVertices(index));
    }
    Ok(indices.iter().map(|&index| index as u16))
}

/// Quantize `[-1, 1]` floats to signed 16-bit normalized integers.
#[inline]
fn f32_to_snorm16<const N: usize>(value: [f32; N]) -> [i16; N] {
    value.map(|component| (component.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
}
