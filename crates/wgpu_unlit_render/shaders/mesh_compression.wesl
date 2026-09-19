// mesh_compression -- vertex-compression decode pure functions.
//
// WGSL twin of the Rust `crate::mesh` compression helpers; the per-mesh
// DECODE PARAMETERS struct lives in `mesh_metadata.wesl` (fixed layout kept
// in sync with the Rust side).

import package::mesh_metadata;

/// Decode an AABB-quantized position: `aabb_center + encode * aabb_half_extents`
/// with `encode` in [-1, 1] (Snorm16 space).
public fn decode_position(encode: vec3<f32>, md: mesh_metadata::MeshMetadata) -> vec3<f32> {
    return md.aabb_center + encode * md.aabb_half_extents;
}

/// Decode a min/extent-remapped UV channel. The Snorm16 `encode` in [-1, 1]
/// is remapped to [0, 1] and then to `min + t * extent`.
public fn decode_uv(encode: vec2<f32>, min_and_extent: vec4<f32>) -> vec2<f32> {
    let t = encode * 0.5 + 0.5;
    return min_and_extent.xy + t * min_and_extent.zw;
}
