// mesh_metadata -- per-mesh vertex-decode parameters and per-mesh draw
// addressing.
//
// Type-declaration module only: it carries the fixed layouts that must stay
// in sync with the Rust mirrors in `crate::mesh`. The decode FUNCTIONS live
// in `mesh_compression.wesl`.

/// Per-mesh decode parameters, read through the `array<MeshMetadata>` storage
/// buffer bound with the global group and indexed by
/// [`MeshInfo::metadata_index`].
///
/// The layout must stay a valid WGSL array stride: the explicit `pad` fields
/// materialize the padding that `vec3<f32>` members require.
public struct MeshMetadata {
    /// `position = aabb_center + encode * aabb_half_extents`, with `encode`
    /// in [-1, 1] (Snorm16 space).
    aabb_center: vec3<f32>,
    pad0: u32,
    /// Half-extents of the position AABB.
    aabb_half_extents: vec3<f32>,
    pad1: u32,
    /// `uv = uv_min_and_extents.xy + t * uv_min_and_extents.zw`, where `t`
    /// remaps the Snorm16 encode from [-1, 1] to [0, 1].
    uv_min_and_extents: vec4<f32>,
}

/// Per-mesh addressing into the shared mesh-metadata array.
///
/// Bound as a small uniform at the mesh group, so a draw selects its decode
/// parameters without an extra per-instance attribute.
public struct MeshInfo {
    /// Index into the `array<MeshMetadata>` bound with the global group.
    metadata_index: u32,
    /// Explicit tail padding.
    pad0: u32,
    /// Explicit tail padding.
    pad1: u32,
    /// Explicit tail padding.
    pad2: u32,
}
