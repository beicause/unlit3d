// unlit -- the built-in unlit vertex/fragment pair.
//
// One entry shader covers every built-in variant: the differences are
// orthogonal WESL conditional-compilation flags, so each composed variant
// contains exactly one code path and only the bindings and attributes it
// uses. Locations, binding slots and vertex-buffer slots are stable across
// variants.
//
// Feature flags:
//   VERTEX_POSITION      read a per-vertex position; without it the geometry
//                        is a single point at the instance origin
//   UNCOMPRESSED_POSITION  the position is a full-precision `vec3<f32>` and
//                        needs no decode; otherwise it is a compressed
//                        `vec4<f32>` decoded through `mesh_meta`
//   VERTEX_UV            read a per-vertex UV
//   UNCOMPRESSED_UV      the UV is a full-precision `vec2<f32>` and needs no
//                        decode; otherwise it is compressed and remapped by
//                        the mesh metadata
//   VERTEX_COLOR         read a per-vertex color and multiply it into the
//                        base color
//   VERTEX_INSTANCE      read the per-instance model matrix and base color;
//                        without it every vertex is modeled at the origin
//                        and the base color is white
//   BASE_COLOR_TEXTURE   sample the material base-color texture
//   SRGB_TO_LINEAR_OUTPUT  the fragment's color values are sRGB-encoded and
//                        the target expects linear values, so convert the
//                        fragment's RGB from sRGB to linear before writing;
//                        alpha is coverage and never converts
//
//   group 0  binding 0  camera        `view::View` uniform
//   group 0  binding 1  frame         `globals::Globals` uniform
//   group 0  binding 2  mesh_meta     `array<MeshMetadata>` decode parameters
//   group 1  binding 0  base_color    base-color texture   [BASE_COLOR_TEXTURE]
//   group 1  binding 1  base_color_sampler                 [BASE_COLOR_TEXTURE]
//   group 2  binding 0  mesh_info     `MeshInfo` metadata index
//
//   slot 0  position   Float32x3 [UNCOMPRESSED_POSITION] or Snorm16x4,
//                      decoded through `mesh_meta`
//   slot 1  uv         Float32x2 [UNCOMPRESSED_UV] or Snorm16x2,
//                      plus color Unorm8x4 [VERTEX_COLOR]
//   slot 2  instance   Float32x4x3 model matrix + Float32x4 base color,
//                      stepped per instance [VERTEX_INSTANCE]
//
// The mesh metadata bindings exist only when a channel is compressed: a
// variant that reads every channel uncompressed declares no metadata binding
// at all.

import package::globals;
import package::mesh_compression;
import package::mesh_metadata;
import package::view;

@group(0) @binding(0) var<uniform> camera: view::View;
@group(0) @binding(1) var<uniform> frame: globals::Globals;

@if((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV))
@group(0) @binding(2) var<storage, read> mesh_meta: array<mesh_metadata::MeshMetadata>;

@if(BASE_COLOR_TEXTURE) @group(1) @binding(0) var base_color_tex: texture_2d<f32>;
@if(BASE_COLOR_TEXTURE) @group(1) @binding(1) var base_color_sampler: sampler;

@if((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV))
@group(2) @binding(0) var<uniform> mesh_info: mesh_metadata::MeshInfo;

struct VertexInput {
    /// Per-vertex position: full precision, or compressed to Snorm16x4
    /// relative to the mesh AABB.
    @if(VERTEX_POSITION && UNCOMPRESSED_POSITION) @location(0) position: vec3<f32>,
    @if(VERTEX_POSITION && !UNCOMPRESSED_POSITION) @location(0) position: vec4<f32>,

    /// Per-vertex UV: full precision, or remapped by the mesh metadata.
    @if(VERTEX_UV) @location(1) uv: vec2<f32>,
    /// Per-vertex color, Unorm8x4.
    @if(VERTEX_COLOR) @location(2) color: vec4<f32>,

    /// Per-instance affine model matrix: one column per location, with each
    /// column's `.w` holding the translation component.
    @if(VERTEX_INSTANCE) @location(3) model_0: vec4<f32>,
    @if(VERTEX_INSTANCE) @location(4) model_1: vec4<f32>,
    @if(VERTEX_INSTANCE) @location(5) model_2: vec4<f32>,
    /// Per-instance base color.
    @if(VERTEX_INSTANCE) @location(6) base_color: vec4<f32>,
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
    // The block form would scope `metadata` to the block, so the declaration
    // itself carries the condition.
    @if((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV))
    let metadata = mesh_meta[mesh_info.metadata_index];

    // Without a position stream the geometry is a single point at the
    // instance origin, so the vertex position is the model matrix's
    // translation. This is what point, particle and impostor draws want: the
    // per-instance data alone places the vertex.
    var local = vec3<f32>(0.0, 0.0, 0.0);
    @if(VERTEX_POSITION && UNCOMPRESSED_POSITION) {
        local = in.position;
    }
    @if(VERTEX_POSITION && !UNCOMPRESSED_POSITION) {
        local = mesh_compression::decode_position(in.position.xyz, metadata);
    }

    // Without an instance stream nothing transforms the vertex: the
    // positions are already in world space, which is what screen-space draws
    // such as a UI pass want.
    var world = local;
    @if(VERTEX_INSTANCE) {
        world = transform_point(in.model_0, in.model_1, in.model_2, local);
    }

    var out: VertexOut;
    out.clip = camera.clip_from_world * vec4<f32>(world, 1.0);
    out.color = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    @if(VERTEX_INSTANCE) {
        out.color = in.base_color;
    }
    @if(VERTEX_UV && UNCOMPRESSED_UV) {
        out.uv = in.uv;
    }
    @if(VERTEX_UV && !UNCOMPRESSED_UV) {
        out.uv = mesh_compression::decode_uv(in.uv, metadata.uv_min_and_extents);
    }
    @if(VERTEX_COLOR) {
        out.color = out.color * in.color;
    }
    return out;
}

/// The premultiplied color of one fragment, before the target's encoding.
///
/// `in.color` and the base-color texel are already premultiplied and in the
/// space the caller supplied, so they multiply directly.
fn fragment_color(in: VertexOut) -> vec4<f32> {
    var color = in.color;
    @if(BASE_COLOR_TEXTURE) {
        color = color * textureSample(base_color_tex, base_color_sampler, in.uv);
    }
    return color;
}

/// Convert a channel from sRGB (gamma) to linear light.
fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        return channel / 12.92;
    }
    return pow((channel + 0.055) / 1.055, 2.4);
}

@fragment
public fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let color = fragment_color(in);
    // An sRGB-aware target encodes whatever it is given, so hand it linear
    // light and let its own encode produce the color the caller asked for.
    // Only RGB converts: alpha is coverage, not color.
    @if(SRGB_TO_LINEAR_OUTPUT) {
        return vec4<f32>(
            srgb_to_linear(color.r),
            srgb_to_linear(color.g),
            srgb_to_linear(color.b),
            color.a,
        );
    }
    return color;
}
