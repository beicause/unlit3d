// array_access -- the frame's shared arrays, and the one way to read them.
//
// Seven arrays reach the built-in shaders: the per-mesh decode parameters, the
// frame's joint matrices, the frame's morph weights and displacements, and --
// for the lit pipeline -- the frame's lights, its cluster cells and its polygon
// vertices. Each is a `var<storage, read>` on a device that has storage buffers,
// and a `texture_2d<f32>` on one that does not — WebGL2 has none at all, so a
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
// (`crate::mesh::MeshMetadata`, `crate::mesh::JointMatrix`, `crate::light`'s
// light records, cells and vertices); the Rust side derives its own layout from
// the same sizes.

import package::light;
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

/// This frame's lights, indexed by the numbers the cluster cells hold.
///
/// One array holds every light of the frame, of every kind, because a cell's
/// records only name indices: one shape per record is what lets a cell mix a
/// point light with a spot and a quad.
@if(!TEXEL_ARRAY && LIT)
@group(0) @binding(6) var<storage, read> lights: array<light::LightGpu>;

/// The frame's cluster cells, each a fixed-size record of its own.
///
/// A cell is `CELL_WORDS` `u32`s at `cell * CELL_WORDS`: its first word is how
/// many lights it holds, the next that many words are their indices, and the
/// rest are unused. Fixed-size records mean a fragment reads one span and knows
/// the whole cell, with no second array to find its end.
@if(!TEXEL_ARRAY && LIT)
@group(0) @binding(7) var<storage, read> light_cells: array<u32>;

/// The polygon vertices of the frame's quad lights, as one `vec4<f32>` each.
///
/// Vertices are pooled rather than stored in the light record because how many
/// a light owns varies while every record is the same size; a record names its
/// slice with `kind.y` and `kind.z`.
@if(!TEXEL_ARRAY && LIT)
@group(0) @binding(8) var<storage, read> light_vertices: array<vec4<f32>>;

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

@if(TEXEL_ARRAY && LIT)
@group(0) @binding(6) var lights_tex: texture_2d<f32>;

@if(TEXEL_ARRAY && LIT)
@group(0) @binding(7) var light_cells_tex: texture_2d<f32>;

@if(TEXEL_ARRAY && LIT)
@group(0) @binding(8) var light_vertices_tex: texture_2d<f32>;

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

/// `Rgba32Float` texels one light record spans: its 80 bytes over a texel's 16.
@if(TEXEL_ARRAY)
const LIGHT_TEXELS: u32 = 5u;

/// `Rgba32Float` texels one cluster cell spans: its 64 bytes over a texel's 16,
/// which is also the `u32` count a cell record holds.
@if(TEXEL_ARRAY)
const LIGHT_CELL_TEXELS: u32 = 4u;

/// `Rgba32Float` texels one polygon vertex spans: its 16 bytes over a texel's 16.
@if(TEXEL_ARRAY)
const LIGHT_VERTEX_TEXELS: u32 = 1u;

/// The `u32`s one cluster cell' record occupies, which is also what its bytes
/// over a `u32` come to. Mirrors `crate::light::CELL_WORDS`: the record size is
/// part of the layout the two languages share, so it is a constant rather than
/// something the frame passes in.
@if(LIT)
const CLUSTER_CELL_WORDS: u32 = 16u;

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
/// Read the light at `index`.
@if(!TEXEL_ARRAY && LIT)
public fn load_light(index: u32) -> light::LightGpu {
    return lights[index];
}

/// Read the light at `index`.
///
/// The five texels are the record's bytes in order. The first four are already
/// `vec4<f32>`s; the kind is four `u32`s the texel holds as `f32` lanes, so it
/// is reinterpreted.
@if(TEXEL_ARRAY && LIT)
public fn load_light(index: u32) -> light::LightGpu {
    let first = index * LIGHT_TEXELS;
    let kind = textureLoad(lights_tex, texel_of(lights_tex, first + 4u), 0);
    return light::LightGpu(
        textureLoad(lights_tex, texel_of(lights_tex, first), 0),
        textureLoad(lights_tex, texel_of(lights_tex, first + 1u), 0),
        textureLoad(lights_tex, texel_of(lights_tex, first + 2u), 0),
        textureLoad(lights_tex, texel_of(lights_tex, first + 3u), 0),
        vec4<u32>(
            bitcast<u32>(kind.x),
            bitcast<u32>(kind.y),
            bitcast<u32>(kind.z),
            bitcast<u32>(kind.w),
        ),
    );
}

/// Read word `word` of cell `cell`.
///
/// Word zero is the cell's light count; the words after it are the indices of
/// that many lights.
@if(!TEXEL_ARRAY && LIT)
public fn load_cell_word(cell: u32, word: u32) -> u32 {
    return light_cells[cell * CLUSTER_CELL_WORDS + word];
}

/// Read word `word` of cell `cell`.
///
/// A cell record is exactly one row of four texels, so its words are the lanes
/// of those texels, reinterpreted from `f32`.
@if(TEXEL_ARRAY && LIT)
public fn load_cell_word(cell: u32, word: u32) -> u32 {
    let first = cell * LIGHT_CELL_TEXELS + word / 4u;
    let texel = textureLoad(light_cells_tex, texel_of(light_cells_tex, first), 0);
    // The lane is picked by name rather than by subscripting the vector: a
    // dynamic subscript is what the word the caller asks for is, but not every
    // backend reads the lane it names -- DX12 returns zero for all of them --
    // so the four candidates are selected and summed instead.
    let lane = word % 4u;
    let lanes = vec4<u32>(
        bitcast<u32>(texel.x),
        bitcast<u32>(texel.y),
        bitcast<u32>(texel.z),
        bitcast<u32>(texel.w),
    );
    return lanes.x * select(0u, 1u, lane == 0u)
        + lanes.y * select(0u, 1u, lane == 1u)
        + lanes.z * select(0u, 1u, lane == 2u)
        + lanes.w * select(0u, 1u, lane == 3u);
}

/// Read the polygon vertex at `index`.
@if(!TEXEL_ARRAY && LIT)
public fn load_light_vertex(index: u32) -> vec3<f32> {
    return light_vertices[index].xyz;
}

/// Read the polygon vertex at `index`.
///
/// A vertex is one `vec4<f32>` and so one texel; its `w` lane is unused.
@if(TEXEL_ARRAY && LIT)
public fn load_light_vertex(index: u32) -> vec3<f32> {
    return textureLoad(
        light_vertices_tex,
        texel_of(light_vertices_tex, index * LIGHT_VERTEX_TEXELS),
        0,
    ).xyz;
}

