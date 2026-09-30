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
//   VERTEX_JOINTS        read joint indices and weights from the position
//                        stream and deform the vertex by the joint matrices
//   MORPH_POSITIONS      read a per-vertex morph position offset and add it
//                        to the deformed position
//   TEXEL_ARRAY          the arrays that would otherwise be `var<storage,
//                        read>` are read from 2D textures instead, one `f32`
//                        lane per texel component; for devices with no storage
//                        buffers at all, WebGL2 in particular
//
//   group 0  binding 0  camera        `view::View` uniform
//   group 0  binding 1  frame         `globals::Globals` uniform
//   group 0  binding 2  mesh_meta     `array<MeshMetadata>` decode parameters
//                      and addressing            [compressed channel | morph]
//   group 0  binding 3  joint_matrices `array<mat4x4<f32>>`  [VERTEX_JOINTS]
//   group 0  binding 4  morph_weights `array<f32>`           [MORPH_POSITIONS]
//   group 1  binding 0  base_color    base-color texture   [BASE_COLOR_TEXTURE]
//   group 1  binding 1  base_color_sampler                 [BASE_COLOR_TEXTURE]
//   group 2  binding 1  morph_deltas  `array<f32>`           [MORPH_POSITIONS]
//
//   slot 0  position   Float32x3 [UNCOMPRESSED_POSITION] or Snorm16x4,
//                      decoded through `mesh_meta`, followed by the joint pair
//                      Uint16x4 indices + Unorm16x4 weights [VERTEX_JOINTS]
//   slot 1  uv         Float32x2 [UNCOMPRESSED_UV] or Snorm16x2,
//                      plus color Unorm8x4 [VERTEX_COLOR]
//   slot 2  instance   Float32x4x3 model matrix + Unorm8x4 base color
//                      + Uint32x2 pose base + Uint32 metadata index, stepped
//                      per instance                   [VERTEX_INSTANCE]
//
// The metadata array exists when a channel is compressed or a morph target is
// read: it carries both the decode parameters and the addressing a draw needs.
// A variant that reads every channel uncompressed and morphs nothing declares
// no metadata binding at all, and the mesh group exists only for the morph
// displacements. The pose lives in the global group rather than a mesh's own
// because it is per-instance state: one array serves every draw of the frame,
// and each instance slices into it at the base its own record carries.
//
// The metadata index is per-instance data like the pose base, so the instance
// stream carries it: `@builtin(instance_index)` addresses the same record, and
// a variant that reads metadata therefore requires [VERTEX_INSTANCE]. One
// record layout then serves every variant of a frame, which is what lets a
// single instance buffer hold the instances of meshes drawn by different
// variants.
//
// The four arrays under [TEXEL_ARRAY] keep their binding numbers but change
// resource type: a `texture_2d<f32>` replaces the `var<storage, read>`, holding
// the same bytes a row at a time. Every read goes through one of the
// `load_*` accessors below, so the rest of the shader is written once and does
// not know which of the two it is running on.

import package::array_access;
import package::globals;
import package::mesh_compression;
import package::mesh_metadata;
import package::view;

@group(0) @binding(0) var<uniform> camera: view::View;
@group(0) @binding(1) var<uniform> frame: globals::Globals;

@if(BASE_COLOR_TEXTURE) @group(1) @binding(0) var base_color_tex: texture_2d<f32>;
@if(BASE_COLOR_TEXTURE) @group(1) @binding(1) var base_color_sampler: sampler;

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
    /// Per-instance base color, Unorm8x4, unpacked to `0..=1` by the vertex
    /// format.
    @if(VERTEX_INSTANCE) @location(6) base_color: vec4<f32>,

    /// Joint indices, Uint16x4. They address data rather than interpolate, so
    /// they are never normalized, and they are part of the position stream
    /// rather than a stream of their own.
    @if(VERTEX_JOINTS) @location(7) joints: vec4<u32>,
    /// Joint weights, Unorm16x4, summing to 1.
    @if(VERTEX_JOINTS) @location(8) weights: vec4<f32>,

    /// Per-instance pose base, Uint32x2: `.x` is the instance's first joint
    /// matrix and `.y` its first morph weight in the frame's shared arrays.
    @if(VERTEX_INSTANCE) @location(9) pose: vec2<u32>,

    /// Per-instance index of the mesh's entry in the frame's metadata array,
    /// which carries the decode parameters and the draw addressing.
    ///
    /// Declared whenever the variant reads the instance stream, whether or not
    /// it reads metadata: every instance record then has one layout, so one
    /// buffer holds the instances of meshes drawn by different variants.
    @if(VERTEX_INSTANCE) @location(10) metadata_index: u32,
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

/// Deform a local-space vertex by the joints `indices`/`weights` name.
///
/// Each joint contributes its skinning matrix — the joint's animated transform
/// times the inverse bind pose — applied to the vertex. The weights of a vertex
/// are a partition of unity, so the weighted sum needs no division.
///
/// `joint_base` is the offset of this instance's matrices in the frame's shared
/// array, which the accessors read from whichever resource holds it. Four lanes
/// are unrolled rather than looped, because indexing the array with a variable
/// would need one, and the attribute is fixed at four joints.
@if(VERTEX_JOINTS)
fn skin_position(
    position: vec3<f32>,
    indices: vec4<u32>,
    weights: vec4<f32>,
    joint_base: u32,
) -> vec3<f32> {
    let point = vec4<f32>(position, 1.0);
    var skinned = vec3<f32>(0.0, 0.0, 0.0);
    skinned += weights.x * (array_access::load_joint_matrix(joint_base + indices.x) * point).xyz;
    skinned += weights.y * (array_access::load_joint_matrix(joint_base + indices.y) * point).xyz;
    skinned += weights.z * (array_access::load_joint_matrix(joint_base + indices.z) * point).xyz;
    skinned += weights.w * (array_access::load_joint_matrix(joint_base + indices.w) * point).xyz;
    return skinned;
}

/// Add the weighted displacement of every morph target to a vertex.
///
/// The displacements are flat and tightly packed: for each vertex,
/// `morph_count` consecutive targets in target order, three components each.
/// `delta_base` is the offset of this vertex's block, and `weight_base` the
/// offset of this instance's weights in the frame's shared array.
///
/// Only the first-order — linear — displacement is applied, which is all a
/// position-only morph target needs.
@if(MORPH_POSITIONS)
fn morph_position(
    position: vec3<f32>,
    delta_base: u32,
    weight_base: u32,
    morph_count: u32,
) -> vec3<f32> {
    var morphed = position;
    var morph = 0u;
    loop {
        if morph >= morph_count {
            break;
        }
        let weight = array_access::load_morph_weight(weight_base + morph);
        // An inactive target is the common case: it costs a load and a compare
        // rather than three more loads.
        if weight != 0.0 {
            let base = delta_base + morph * 3u;
            morphed += weight * vec3<f32>(
                array_access::load_morph_delta(base),
                array_access::load_morph_delta(base + 1u),
                array_access::load_morph_delta(base + 2u),
            );
        }
        morph += 1u;
    }
    return morphed;
}

@vertex
public fn vs_main(in: VertexInput, @builtin(vertex_index) vertex_index: u32) -> VertexOut {
    // The block form would scope `metadata` to the block, so the declaration
    // itself carries the condition.
    //
    // The struct carries both the decode parameters and the addressing, and
    // the index comes from the instance stream, so anything that reads either
    // loads it once here.
    @if((VERTEX_POSITION && !UNCOMPRESSED_POSITION) || (VERTEX_UV && !UNCOMPRESSED_UV) || MORPH_POSITIONS)
    let metadata = array_access::load_mesh_metadata(in.metadata_index);

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

    // Deformation happens in the mesh's own space, before the instance
    // transform places the vertex in the world: a joint matrix is relative to
    // the mesh, and a morph displacement is a local offset.
    //
    // `@builtin(vertex_index)` is the draw's vertex ordinal in the stream's
    // pool — `firstVertex + i`, or an index plus `baseVertex` — so the morph
    // deltas, which are stored per mesh, are addressed relative to the mesh's
    // own first vertex.
    //
    // The pose the instance deforms by is addressed through `in.pose`: the
    // joint matrices and morph weights are frame-wide arrays, and every
    // instance names its own slice of them. That is what lets two instances of
    // one mesh hold different poses.
    @if(MORPH_POSITIONS) {
        let local_vertex = vertex_index - metadata.vertex_offset;
        local = morph_position(
            local,
            local_vertex * metadata.morph_count * 3u,
            in.pose.y,
            metadata.morph_count,
        );
    }
    @if(VERTEX_JOINTS) {
        local = skin_position(local, in.joints, in.weights, in.pose.x);
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
