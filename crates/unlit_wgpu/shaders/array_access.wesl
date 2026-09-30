// array_access -- the frame's shared arrays, and the one way to read them.
//
// Four arrays reach the built-in shader: the per-mesh decode parameters, the
// frame's joint matrices, and the frame's morph weights and displacements.
// Each is a `var<storage, read>` on a device that has storage buffers, and a
// `texture_2d<f32>` on one that does not — WebGL2 has none at all, so a
// bind-group layout naming a storage buffer is rejected there.
//
// The two paths are hidden behind the `load_*` accessors below, which every
// caller uses regardless of the path. That is what keeps the shader body
// written once: only this module knows whether an array arrived as a buffer or
// as texels.
//
// Each accessor exists only where its array does — the same condition that
// declares the binding also guards the accessor — so a variant that reads no
// joints declares neither the joint binding nor a function naming it.
//
// # The texel path
//
// A texel array holds the same bytes the buffer held, as `f32` lanes in a 2D
// texture. The row width is never a shader constant: it is read back with
// `textureDimensions`, so the CPU chooses it freely. An element's texels are
// contiguous — the CPU never lets one straddle a row — so an element is
// addressed by multiplying its index by the texels it spans.
//
// The `*_TEXELS` constants are the element sizes divided by the 16 bytes of an
// `Rgba32Float` texel. They mirror the Rust types whose bytes the arrays hold
// (`crate::mesh::MeshMetadata`, `crate::mesh::JointMatrix`); the Rust side
// derives its own layout from the same sizes.

import package::mesh_metadata;

// The condition under which the metadata array is read: when a channel is
// compressed, or when a morph target's displacements have to be addressed. The
// struct carries both the decode parameters and the addressing, so a morph
// reads it even when every channel is uncompressed.
@if(!TEXEL_ARRAY && ((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV) || MORPH_POSITIONS))
@group(0) @binding(2) var<storage, read> mesh_meta: array<mesh_metadata::MeshMetadata>;

/// This frame's joint matrices, indexed by the joint pair in the position
/// stream. Each one is a joint's animated transform times its inverse bind
/// pose, applied in the mesh's own space.
///
/// The array holds the pose of every visible instance, and an instance reads
/// its own joints from its pose base; that is what lets two instances of one
/// mesh deform differently.
@if(!TEXEL_ARRAY && VERTEX_JOINTS)
@group(0) @binding(3) var<storage, read> joint_matrices: array<mat4x4<f32>>;

/// This frame's morph weights, one per target of every morphed instance.
///
/// Like the joint matrices this is per-instance data in one frame-wide array,
/// and an instance reads its own weights from its pose base.
@if(!TEXEL_ARRAY && MORPH_POSITIONS)
@group(0) @binding(4) var<storage, read> morph_weights: array<f32>;

/// Every morph target's per-vertex position displacement, flat and tightly
/// packed: for each vertex, `mesh_metadata::MeshMetadata::morph_count` targets
/// in order, three components each.
///
/// A target's displacement is geometry — every vertex of the mesh has its own —
/// but one array holds the displacements of every mesh in the frame and a draw
/// names its slice with the metadata entry's `morph_deltas_offset`. Pooling
/// them here rather than in a per-mesh buffer is what leaves the mesh group
/// with nothing to bind.
@if(!TEXEL_ARRAY && MORPH_POSITIONS)
@group(0) @binding(5) var<storage, read> morph_deltas: array<f32>;

// The same arrays as textures. Each keeps its binding number and group, so the
// two paths declare the same interface and a variant differs only in which
// resource type it binds.
@if(TEXEL_ARRAY && ((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV) || MORPH_POSITIONS))
@group(0) @binding(2) var mesh_meta_tex: texture_2d<f32>;

@if(TEXEL_ARRAY && VERTEX_JOINTS)
@group(0) @binding(3) var joint_matrices_tex: texture_2d<f32>;

@if(TEXEL_ARRAY && MORPH_POSITIONS)
@group(0) @binding(4) var morph_weights_tex: texture_2d<f32>;

@if(TEXEL_ARRAY && MORPH_POSITIONS)
@group(0) @binding(5) var morph_deltas_tex: texture_2d<f32>;

/// The texel holding element `index`'s first lane.
///
/// The row width comes from the texture itself rather than from a constant the
/// CPU and the shader would have to agree on.
@if(TEXEL_ARRAY)
fn texel_of(array: texture_2d<f32>, index: u32) -> vec2<i32> {
    let width = textureDimensions(array).x;
    return vec2<i32>(i32(index % width), i32(index / width));
}

/// `Rgba32Float` texels one `MeshMetadata` spans: its 64 bytes over a texel's
/// 16.
@if(TEXEL_ARRAY)
const MESH_METADATA_TEXELS: u32 = 4u;

/// `Rgba32Float` texels one joint matrix spans: its 64 bytes over a texel's 16.
@if(TEXEL_ARRAY)
const JOINT_MATRIX_TEXELS: u32 = 4u;

/// Read the decode parameters and addressing of the mesh at `index`.
@if(!TEXEL_ARRAY && ((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV) || MORPH_POSITIONS))
public fn load_mesh_metadata(index: u32) -> mesh_metadata::MeshMetadata {
    return mesh_meta[index];
}

/// Read the decode parameters and addressing of the mesh at `index`.
///
/// The four texels are the struct's bytes in order: the AABB center and the
/// vertex offset, the half-extents and the morph count, then the UV bounds,
/// then the morph displacement offset. The integer members are read as `f32`
/// lanes and reinterpreted, because that is what the texel holds.
@if(TEXEL_ARRAY && ((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV) || MORPH_POSITIONS))
public fn load_mesh_metadata(index: u32) -> mesh_metadata::MeshMetadata {
    let first = index * MESH_METADATA_TEXELS;
    let center = textureLoad(mesh_meta_tex, texel_of(mesh_meta_tex, first), 0);
    let extents = textureLoad(mesh_meta_tex, texel_of(mesh_meta_tex, first + 1u), 0);
    let uv = textureLoad(mesh_meta_tex, texel_of(mesh_meta_tex, first + 2u), 0);
    let deltas = textureLoad(mesh_meta_tex, texel_of(mesh_meta_tex, first + 3u), 0);
    return mesh_metadata::MeshMetadata(
        center.xyz,
        bitcast<u32>(center.w),
        extents.xyz,
        bitcast<u32>(extents.w),
        uv,
        bitcast<u32>(deltas.x),
    );
}

/// Read the joint matrix at `index`.
@if(!TEXEL_ARRAY && VERTEX_JOINTS)
public fn load_joint_matrix(index: u32) -> mat4x4<f32> {
    return joint_matrices[index];
}

/// Read the joint matrix at `index`.
///
/// A matrix is column-major, and each column is 16 bytes, so its four texels
/// are its four columns in order.
@if(TEXEL_ARRAY && VERTEX_JOINTS)
public fn load_joint_matrix(index: u32) -> mat4x4<f32> {
    let first = index * JOINT_MATRIX_TEXELS;
    return mat4x4<f32>(
        textureLoad(joint_matrices_tex, texel_of(joint_matrices_tex, first), 0),
        textureLoad(joint_matrices_tex, texel_of(joint_matrices_tex, first + 1u), 0),
        textureLoad(joint_matrices_tex, texel_of(joint_matrices_tex, first + 2u), 0),
        textureLoad(joint_matrices_tex, texel_of(joint_matrices_tex, first + 3u), 0),
    );
}

/// Read the morph weight at `index`.
@if(!TEXEL_ARRAY && MORPH_POSITIONS)
public fn load_morph_weight(index: u32) -> f32 {
    return morph_weights[index];
}

/// Read the morph weight at `index`.
///
/// A weight is a single `f32`, so it is one `R32Float` texel and its value is
/// the texel's first lane.
@if(TEXEL_ARRAY && MORPH_POSITIONS)
public fn load_morph_weight(index: u32) -> f32 {
    return textureLoad(morph_weights_tex, texel_of(morph_weights_tex, index), 0).x;
}

/// Read the morph displacement at `index`.
@if(!TEXEL_ARRAY && MORPH_POSITIONS)
public fn load_morph_delta(index: u32) -> f32 {
    return morph_deltas[index];
}

/// Read the morph displacement at `index`.
///
/// Displacements are a flat `f32` array, one scalar per texel, exactly as the
/// weights are.
@if(TEXEL_ARRAY && MORPH_POSITIONS)
public fn load_morph_delta(index: u32) -> f32 {
    return textureLoad(morph_deltas_tex, texel_of(morph_deltas_tex, index), 0).x;
}
