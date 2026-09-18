// unlit -- the built-in unlit vertex/fragment pair.
//
// One entry shader covers every built-in variant: the differences are
// orthogonal WESL conditional-compilation flags, so each composed variant
// contains exactly one code path and only the bindings and attributes it
// uses. Locations, binding slots and vertex-buffer slots are stable across
// variants.
//
// Feature flags:
//   VERTEX_POSITION      read the compressed per-vertex position; without it
//                        the geometry is a single point at the instance origin
//   VERTEX_UV            read and decode the per-vertex UV
//   VERTEX_COLOR         read the per-vertex color and multiply it into the
//                        base color
//   BASE_COLOR_TEXTURE   sample the material base-color texture
//
//   group 0  binding 0  camera        `view::View` uniform
//   group 0  binding 1  frame         `globals::Globals` uniform
//   group 0  binding 2  mesh_meta     `array<MeshMetadata>` decode parameters
//   group 1  binding 0  base_color    base-color texture   [BASE_COLOR_TEXTURE]
//   group 1  binding 1  base_color_sampler                 [BASE_COLOR_TEXTURE]
//   group 2  binding 0  mesh_info     `MeshInfo` metadata index
//
//   slot 0  position   Snorm16x4, decoded through `mesh_meta` [VERTEX_POSITION]
//   slot 1  uv         Snorm16x2 [VERTEX_UV] + color Unorm8x4 [VERTEX_COLOR]
//   slot 2  instance   Float32x4x3 model matrix + Float32x4 base color,
//                      stepped per instance

import package::globals;
import package::mesh_compression;
import package::mesh_metadata;
import package::view;

@group(0) @binding(0) var<uniform> camera: view::View;
@group(0) @binding(1) var<uniform> frame: globals::Globals;
@group(0) @binding(2) var<storage, read> mesh_meta: array<mesh_metadata::MeshMetadata>;

@if(BASE_COLOR_TEXTURE) @group(1) @binding(0) var base_color_tex: texture_2d<f32>;
@if(BASE_COLOR_TEXTURE) @group(1) @binding(1) var base_color_sampler: sampler;

@group(2) @binding(0) var<uniform> mesh_info: mesh_metadata::MeshInfo;

struct VertexInput {
    /// Per-vertex position, Snorm16x4 relative to the mesh AABB.
    @if(VERTEX_POSITION) @location(0) position: vec4<f32>,

    /// Per-vertex UV, Snorm16x2 remapped by the mesh metadata.
    @if(VERTEX_UV) @location(1) uv: vec2<f32>,
    /// Per-vertex color, Unorm8x4 (straight linear RGBA).
    @if(VERTEX_COLOR) @location(2) color: vec4<f32>,

    /// Per-instance affine model matrix: one column per location, with each
    /// column's `.w` holding the translation component.
    @location(3) model_0: vec4<f32>,
    @location(4) model_1: vec4<f32>,
    @location(5) model_2: vec4<f32>,
    /// Per-instance base color (linear RGBA).
    @location(6) base_color: vec4<f32>,
}

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @if(VERTEX_UV) @location(0) uv: vec2<f32>,
    /// Base color multiplied by the per-vertex color when present.
    @location(1) color: vec4<f32>,
}

/// Transform a local-space point by an affine matrix packed as three columns
/// with the translation in each column's `.w` lane.
public fn transform_point(m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>, p: vec3<f32>) -> vec3<f32> {
    let v = vec4<f32>(p, 1.0);
    return vec3<f32>(dot(m0, v), dot(m1, v), dot(m2, v));
}

@vertex
public fn vs_main(in: VertexInput) -> VertexOut {
    let metadata = mesh_meta[mesh_info.metadata_index];

    // Without a position stream the geometry is a single point at the
    // instance origin, so the vertex position is the model matrix's
    // translation. This is what point, particle and impostor draws want: the
    // per-instance data alone places the vertex.
    var local = vec3<f32>(0.0, 0.0, 0.0);
    @if(VERTEX_POSITION) {
        local = mesh_compression::decode_position(in.position.xyz, metadata);
    }
    let world = transform_point(in.model_0, in.model_1, in.model_2, local);

    var out: VertexOut;
    out.clip = camera.clip_from_world * vec4<f32>(world, 1.0);
    out.color = in.base_color;
    @if(VERTEX_UV) {
        out.uv = mesh_compression::decode_uv(in.uv, metadata.uv_min_and_extents);
    }
    @if(VERTEX_COLOR) {
        out.color = out.color * in.color;
    }
    return out;
}

@fragment
public fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    var color = in.color;
    @if(BASE_COLOR_TEXTURE) {
        color = color * textureSample(base_color_tex, base_color_sampler, in.uv);
    }
    return color;
}
