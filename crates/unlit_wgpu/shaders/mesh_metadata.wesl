// mesh_metadata -- per-mesh vertex-decode parameters and per-mesh draw
// addressing.
//
// Type-declaration module only: it carries the fixed layout that must stay in
// sync with the Rust mirror in `crate::mesh`. The decode FUNCTIONS live in
// `mesh_compression.wesl`.

/// Per-mesh decode parameters and draw addressing, read through the
/// `array<MeshMetadata>` storage buffer bound with the global group and indexed
/// by the metadata index the instance stream carries.
///
/// The layout must stay a valid WGSL array stride. The scalars are not padding
/// but the mesh's addressing: two fill what the `vec3<f32>` members would
/// otherwise leave empty, so the rows the vectors occupy carry four members
/// rather than two and two discarded lanes, and one more costs only the tail
/// the `vec4<f32>`'s 16-byte alignment leaves anyway.
public struct MeshMetadata {
    /// `position = aabb_center + encode * aabb_half_extents`, with `encode`
    /// in [-1, 1] (Snorm16 space).
    aabb_center: vec3<f32>,
    /// First element this mesh's vertices occupy in the pool its streams live
    /// in, which is the draw's `firstVertex` or `baseVertex`.
    ///
    /// A shader that maps a vertex back to its mesh-local ordinal — the morph
    /// deltas, which are stored per vertex of a mesh — subtracts it from
    /// `@builtin(vertex_index)`.
    vertex_offset: u32,
    /// Half-extents of the position AABB.
    aabb_half_extents: vec3<f32>,
    /// Number of morph targets whose deltas follow each vertex. Zero without
    /// morph targets.
    morph_count: u32,
    /// `uv = uv_min_and_extents.xy + t * uv_min_and_extents.zw`, where `t`
    /// remaps the Snorm16 encode from [-1, 1] to [0, 1].
    uv_min_and_extents: vec4<f32>,
    /// First element this mesh's morph displacements occupy in the frame's
    /// shared `array<f32>`.
    ///
    /// The displacements are the mesh's own geometry, but they live in one
    /// frame-wide array rather than a per-mesh buffer, so a draw names its
    /// slice by an offset like this one instead of binding a resource.
    morph_deltas_offset: u32,
}
