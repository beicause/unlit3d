// lit -- the built-in lit vertex/fragment pair, lit or unlit by one flag.
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
//   UNCOMPRESSED_POSITION  the position is full precision and needs no decode;
//                        otherwise it is a compressed `vec4<f32>` decoded
//                        through `mesh_meta`
//   PLANAR_POSITION       the position is a full-precision `vec2<f32>`: the
//                        geometry already lies in the target's plane, so the
//                        zero third is supplied here; implies
//                        `UNCOMPRESSED_POSITION`
//   VERTEX_UV            read a per-vertex UV
//   UNCOMPRESSED_UV      the UV is a full-precision `vec2<f32>` and needs no
//                        decode; otherwise it is compressed and remapped by
//                        the mesh metadata
//   VERTEX_COLOR         read a per-vertex color and multiply it into the
//                        base color
//   VERTEX_NORMAL        read a per-vertex normal, octahedral-encoded into
//                        two `snorm16` lanes; a lit variant shades with it,
//                        the unlit color does not
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
//   BASE_COLOR_LUMINANCE  the base-color texture stores one luminance channel;
//                        sample it into every color channel and decode the
//                        result from sRGB
//   BASE_COLOR_LUMINANCE_ALPHA  the base-color texture stores luminance and
//                        alpha; sample them into RGB and alpha and decode the
//                        luminance from sRGB
//   ALPHA_CUTOFF         discard a fragment whose alpha is below the
//                        instance's cutoff
//   LIT                  shade the fragment with the frame's lights: read the
//                        cell the fragment's NDC and view depth identify,
//                        walk its cell and add each light to the ambient term
//
// The `VERTEX_*` names are the geometry's answer rather than a variant's: a
// pipeline reads them off the vertex buffers its draws are recorded with, so
// every draw of a mesh carries the channels that mesh's buffers hold. The
// per-instance record is unconditional: every built-in variant declares and
// binds the instance stream, because the family always has an instance buffer
// to bind — a draw with no instance state of its own binds the identity record.
//
//   group 0  binding 0  camera        `view::View` uniform
//   group 0  binding 1  frame         `globals::Globals` uniform
//   group 0  binding 2  mesh_meta     `array<MeshMetadata>` decode parameters
//                      and addressing            [compressed channel | morph]
//   group 0  binding 3  joint_matrices `array<mat4x4<f32>>`  [VERTEX_JOINTS]
//   group 0  binding 4  morph_weights `array<f32>`           [MORPH_POSITIONS]
//   group 0  binding 5  morph_deltas  `array<f32>`           [MORPH_POSITIONS]
//   group 0  binding 6  lights        `array<LightGpu>`      [LIT]
//   group 0  binding 7  light_cells   `array<u32>`            [LIT]
//   group 0  binding 8  light_vertices `array<vec4<f32>>`     [LIT]
//   group 0  binding 9  cluster       `light::ClusterUniform` [LIT]
//   group 0  binding 10 lighting      `light::LightingUniform` [LIT]
//   group 1  binding 0  base_color    base-color texture   [BASE_COLOR_TEXTURE]
//   group 1  binding 1  base_color_sampler                 [BASE_COLOR_TEXTURE]
//
//   slot 0  position   Float32x2 [PLANAR_POSITION], Float32x3
//                      [UNCOMPRESSED_POSITION] or Snorm16x4, decoded through
//                      `mesh_meta`, followed by the joint pair
//                      Uint16x4 indices + Unorm16x4 weights [VERTEX_JOINTS]
//   slot 1  uv         Float32x2 [UNCOMPRESSED_UV] or Snorm16x2,
//                      plus color Unorm8x4 [VERTEX_COLOR],
//                      plus normal Snorm16x2 [VERTEX_NORMAL] decoded by
//                      `mesh_compression::decode_normal`
//                      -- the secondary stream, in attribute order
//   slot 2  instance   Float32x4x3 model matrix
//                      + Unorm8x4 base color
//                      + Uint32 joint base
//                      + Uint32 metadata index
//                      + Float32 alpha cutoff         [ALPHA_CUTOFF]
//                      + Uint32 morph base
//                      stepped per instance
//
// Every array the shader reads is frame-wide and lives in the global group, so
// a draw binds no group of its own: the metadata array carries each mesh's
// decode parameters and its addressing, the pose arrays carry every instance's
// joints and morph weights, and the morph displacements carry every mesh's
// geometry. The last is a mesh's own data conceptually, but one array holds it
// for the frame and a mesh names its slice through the metadata entry like
// everything else, which is what leaves the mesh group nothing to bind.
//
// The metadata array exists when a channel is compressed or a morph target is
// read: it carries both the decode parameters and the addressing a draw needs.
// A variant that reads every channel uncompressed and morphs nothing declares
// none of the optional bindings at all.
//
// The instance stream is one record layout whose attributes each serve one
// function, and every variant declares the whole record: the frame holds one
// buffer of records for every visible instance, whatever variant draws it, so
// the stride is the whole record and the fields a variant does not read cost
// nothing but a declared attribute. The record is a struct rather than a loose
// bag of attributes for that reason — a variant that reads two fields still
// declares them at their own offsets.
//
// The metadata index is per-instance data like the pose bases, so the instance
// stream carries it: `@builtin(instance_index)` addresses the same record. The
// same holds for the alpha cutoff and the two pose bases: they are per-instance
// state, so they cannot live in the mesh's or the material's bind group, which
// one draw can share with differently-posed and differently-cut instances.
//
// The seven arrays under [TEXEL_ARRAY] keep their binding numbers but change
// resource type: a `texture_2d<f32>` replaces the `var<storage, read>`, holding
// the same bytes a row at a time. Every read goes through one of the
// `load_*` accessors below, so the rest of the shader is written once and does
// not know which of the two it is running on.

import package::array_access;
import package::globals;
import package::light;
import package::mesh_compression;
import package::mesh_metadata;
import package::polygon;
import package::view;

@group(0) @binding(0) var<uniform> camera: view::View;
@group(0) @binding(1) var<uniform> frame: globals::Globals;

@if(LIT) @group(0) @binding(9) var<uniform> cluster: light::ClusterUniform;
@if(LIT) @group(0) @binding(10) var<uniform> lighting: light::LightingUniform;

@if(BASE_COLOR_TEXTURE) @group(1) @binding(0) var base_color_tex: texture_2d<f32>;
@if(BASE_COLOR_TEXTURE) @group(1) @binding(1) var base_color_sampler: sampler;

// unlit_wgpu::hook module

struct VertexInput {
    /// Per-vertex position: full precision in two or three components, or
    /// compressed to Snorm16x4 relative to the mesh AABB.
    @if(VERTEX_POSITION && PLANAR_POSITION) @location(0) position: vec2<f32>,
    @if(VERTEX_POSITION && UNCOMPRESSED_POSITION && !PLANAR_POSITION) @location(0) position: vec3<f32>,
    @if(VERTEX_POSITION && !UNCOMPRESSED_POSITION) @location(0) position: vec4<f32>,

    /// Per-vertex UV: full precision, or remapped by the mesh metadata.
    @if(VERTEX_UV) @location(1) uv: vec2<f32>,
    /// Per-vertex color, Unorm8x4.
    @if(VERTEX_COLOR) @location(2) color: vec4<f32>,
    /// Per-vertex normal, Snorm16x2, octahedral-encoded and so needing no
    /// decode parameters. A lit variant shades with it; the unlit color never
    /// reads it.
    @if(VERTEX_NORMAL) @location(13) normal: vec2<f32>,

    /// Per-instance affine model matrix: one column per location, with each
    /// column's `.w` holding the translation component.
    @location(3) model_0: vec4<f32>,
    @location(4) model_1: vec4<f32>,
    @location(5) model_2: vec4<f32>,
    /// Per-instance base color, Unorm8x4, unpacked to `0..=1` by the vertex
    /// format.
    @location(6) base_color: vec4<f32>,

    /// Joint indices, Uint16x4. They address data rather than interpolate, so
    /// they are never normalized, and they are part of the position stream
    /// rather than a stream of their own.
    @if(VERTEX_JOINTS) @location(7) joints: vec4<u32>,
    /// Joint weights, Unorm16x4, summing to 1.
    @if(VERTEX_JOINTS) @location(8) weights: vec4<f32>,

    /// Per-instance joint-matrix base, Uint32: the instance's first joint
    /// matrix in the frame's shared array.
    @location(9) joints_base: u32,

    /// Per-instance index of the mesh's entry in the frame's metadata array,
    /// which carries the decode parameters and the draw addressing.
    @location(10) metadata_index: u32,

    /// Per-instance alpha cutoff, Float32: a fragment below it is discarded.
    @if(ALPHA_CUTOFF) @location(11) cutoff: f32,

    /// Per-instance morph-weight base, Uint32: the instance's first weight in
    /// the frame's shared array.
    @location(12) morph_base: u32,
}

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @if(VERTEX_UV) @location(0) uv: vec2<f32>,
    /// Base color multiplied by the per-vertex color when present.
    @location(1) color: vec4<f32>,
    /// The instance's alpha cutoff, constant across the primitive; the
    /// fragment stage compares it against the color's alpha.
    @if(ALPHA_CUTOFF) @location(2) cutoff: f32,
    /// World-space normal, or zero for a mesh with no normal channel; the
    /// fragment stage then derives the geometric normal from screen-space
    /// derivatives.
    @if(LIT) @location(3) normal: vec3<f32>,
    /// World-space position, which the lights' world-space records address and
    /// which the cluster cell is derived from.
    @if(LIT) @location(4) world: vec3<f32>,
}

/// Transform a local-space point by an affine matrix packed as three columns
/// with the translation in each column's `.w` lane.
public fn transform_point(m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>, p: vec3<f32>) -> vec3<f32> {
    let v = vec4<f32>(p, 1.0);
    return vec3<f32>(dot(m0, v), dot(m1, v), dot(m2, v));
}

/// Transform a local-space direction by the linear part of an affine matrix
/// packed the same way.
///
/// The packing puts the translation in each column's `.w` lane, so the
/// direction uses the three-component part alone: a normal is a difference of
/// positions and must not pick up where the instance sits.
///
/// A non-uniform scale shears a normal, and the exact transform is the inverse
/// transpose, which the stream does not carry; a uniformly scaled instance —
/// and every unscaled one — is exact this way.
public fn transform_vector(m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(m0.xyz, v), dot(m1.xyz, v), dot(m2.xyz, v));
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
/// `delta_base` offsets the vertex's own block inside the frame's shared
/// displacement array — the mesh's `morph_deltas_offset` plus the vertex's
/// ordinal times the target count — and `weight_base` the instance's weights in
/// the frame's shared weight array.
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
    @if(VERTEX_POSITION && PLANAR_POSITION) {
        local = vec3<f32>(in.position, 0.0);
    }
    @if(VERTEX_POSITION && UNCOMPRESSED_POSITION && !PLANAR_POSITION) {
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
        // The mesh's displacements are its own, so they are addressed in
        // mesh-local ordinals: the pool-global `vertex_index` has to be mapped
        // back through the mesh's own vertex offset first.
        let local_vertex = vertex_index - metadata.vertex_offset;
        local = morph_position(
            local,
            metadata.morph_deltas_offset + local_vertex * metadata.morph_count * 3u,
            in.morph_base,
            metadata.morph_count,
        );
    }
    @if(VERTEX_JOINTS) {
        local = skin_position(local, in.joints, in.weights, in.joints_base);
    }

    // The instance transform always places the vertex, so a screen-space draw
    // such as a UI pass binds the identity record and the transform is a no-op.
    //
    // Mutable rather than a `let` because of the hook below: a caller's snippet
    // displaces the vertex here, and everything after this point -- the clip
    // position, the world position the lights are addressed from, the normal and
    // the cluster cell -- follows the displaced vertex.
    var world = transform_point(in.model_0, in.model_1, in.model_2, local);
    // unlit_wgpu::hook vertex

    var out: VertexOut;
    out.clip = camera.clip_from_world * vec4<f32>(world, 1.0);
    out.color = in.base_color;
    @if(ALPHA_CUTOFF) {
        out.cutoff = in.cutoff;
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
    // A lit fragment addresses the frame's world-space lights and asks which
    // cell it lands in, so the vertex stage hands over the world position and
    // the normal it shades with.
    @if(LIT) {
        out.world = world;
    }
    @if(LIT && VERTEX_NORMAL) {
        out.normal = transform_vector(
            in.model_0,
            in.model_1,
            in.model_2,
            mesh_compression::decode_normal(in.normal),
        );
    }
    @if(LIT && !VERTEX_NORMAL) {
        // A mesh with no normal channel is lit by its geometric normal, which
        // the fragment stage derives from the world positions it interpolates.
        out.normal = vec3<f32>(0.0);
    }
    return out;
}

/// The world-space normal a lit fragment shades with.
///
/// A mesh that carries normals hands one over per vertex, already interpolated
/// and transformed; a mesh without them is shaded by the geometric normal of
/// the triangle it belongs to, taken from the world positions across the
/// primitive's screen-space derivatives. The geometric normal has no side of
/// its own -- a triangle and its reverse are the same triangle -- so it is
/// turned toward the eye, which is the side a back-face culled draw presents.
@if(LIT)
fn fragment_normal(in: VertexOut) -> vec3<f32> {
    @if(VERTEX_NORMAL) {
        // A zero-length normal is a mesh that had no valid normal to give:
        // fall through to the geometric one rather than dividing by nothing.
        let length_squared = dot(in.normal, in.normal);
        if length_squared > 1.0e-8 {
            return in.normal * inverseSqrt(length_squared);
        }
    }
    var geometric = cross(dpdx(in.world), dpdy(in.world));
    // Named apart from the vertex normal's length above: an inline WESL
    // conditional lays its declarations into the surrounding scope rather
    // than opening one of its own.
    let geometric_length_squared = dot(geometric, geometric);
    if geometric_length_squared <= 1.0e-16 {
        return vec3<f32>(0.0, 0.0, 1.0);
    }
    geometric = geometric * inverseSqrt(geometric_length_squared);
    let to_eye = camera.camera_position.xyz - in.world;
    return select(-geometric, geometric, dot(geometric, to_eye) >= 0.0);
}

/// The normalized device coordinates of a world-space point.
///
/// A fragment derives the coordinates of its own cell from the world position
/// rather than from a varying. Perspective-correct interpolation reproduces the
/// surface point exactly, so projecting it lands on the fragment's own place on
/// screen. Handing the coordinates over as a varying would instead need linear
/// interpolation, which the WebGL2 baseline cannot express.
@if(LIT)
fn fragment_ndc(world: vec3<f32>) -> vec2<f32> {
    let clip = camera.clip_from_world * vec4<f32>(world, 1.0);
    return clip.xy / clip.w;
}

/// The linear depth of a world-space point: its distance along the camera's
/// view direction, which is what the depth slices are cut by.
@if(LIT)
fn view_depth(world: vec3<f32>) -> f32 {
    let view = camera.view_from_world * vec4<f32>(world, 1.0);
    return -view.z;
}

/// The depth slice containing `depth`, before clamping to the grid.
///
/// Slice zero absorbs the near end. Perspective slices are logarithmic after
/// that boundary; orthographic slices are linear. The continuous coordinate is
/// returned as well so a debug view can draw the exact boundary the lookup
/// floors, without maintaining a second mapping.
@if(LIT)
fn cluster_slice_coordinate(depth: f32) -> f32 {
    if depth < cluster.depth.w {
        return 0.0;
    }
    if cluster.perspective.x != 0u {
        return (log(depth) - cluster.depth.y) * cluster.depth.x * cluster.depth.z + 1.0;
    }
    return (depth - cluster.depth.y) * cluster.depth.x * cluster.depth.z;
}

@if(LIT)
fn cluster_depth_slice(depth: f32) -> u32 {
    return u32(clamp(floor(cluster_slice_coordinate(depth)), 0.0, f32(cluster.dims.z - 1u)));
}

/// The cluster cell a fragment at `ndc` and `depth` falls in.
///
/// `ndc` is the fragment's own place on screen, which [`fragment_ndc`] derives
/// from the world position it interpolates.
@if(LIT)
fn fragment_cluster_index(ndc: vec2<f32>, depth: f32) -> u32 {
    let dims = cluster.dims.xyz;
    let x = clamp(floor((ndc.x * 0.5 + 0.5) * f32(dims.x)), 0.0, f32(dims.x - 1u));
    let y = clamp(floor((ndc.y * 0.5 + 0.5) * f32(dims.y)), 0.0, f32(dims.y - 1u));
    return (cluster_depth_slice(depth) * dims.y + u32(y)) * dims.x + u32(x);
}

/// A light at infinity: one direction, no falloff.
@if(LIT)
fn directional_radiance(light: light::DirectionalGpu, n: vec3<f32>) -> vec3<f32> {
    // `direction.xyz` is where the light travels, so the direction to it is the
    // negation.
    let cosine = max(dot(n, -light.direction.xyz), 0.0);
    return light.color_intensity.rgb * (light.color_intensity.w * cosine);
}

/// Every directional light's contribution, which reaches every fragment.
///
/// The slots an unused light left at zero contribute nothing, so the fragment
/// reads all of them and the frame's count never arrives. That makes the term
/// a fixed `MAX_DIRECTIONAL_LIGHTS` contributions per fragment whatever the
/// frame holds: the count is a constant rather than a per-frame value, so a
/// scene with fewer directional lights pays for the slots it leaves empty.
@if(LIT)
fn directional_irradiance(n: vec3<f32>) -> vec3<f32> {
    var irradiance = vec3<f32>(0.0);
    for (var i = 0u; i < light::MAX_DIRECTIONAL_LIGHTS; i += 1u) {
        irradiance += directional_radiance(lighting.directionals[i], n);
    }
    return irradiance;
}

/// A point light's inverse-square falloff, windowed to reach nothing at its
/// range.
///
/// The window is the smooth `(1 - (d / range)^4)^2` one rather than a hard
/// cutoff, so a surface crossing the range does not step in brightness. A range
/// of zero -- a light that reaches nowhere -- contributes nothing.
@if(LIT)
fn range_attenuation(distance_squared: f32, range: f32) -> f32 {
    if range <= 0.0 {
        return 0.0;
    }
    let ratio = distance_squared / (range * range);
    let window = max(1.0 - ratio * ratio, 0.0);
    return window * window / max(distance_squared, 1.0e-8);
}

/// A point light, or the point-light part of a spot light.
@if(LIT)
fn point_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let offset = light.position_radius.xyz - p;
    let distance_squared = dot(offset, offset);
    let attenuation = range_attenuation(distance_squared, light.position_radius.w);
    if attenuation <= 0.0 {
        return vec3<f32>(0.0);
    }
    let cosine = max(dot(n, offset * inverseSqrt(max(distance_squared, 1.0e-8))), 0.0);
    return light.color_intensity.rgb * (light.color_intensity.w * attenuation * cosine);
}

/// A point light narrowed to a cone: the point term, times a falloff from the
/// inner angle to the outer one.
///
/// `direction_outer.w` is the cosine of the outer half-angle and `cones.x` the
/// reciprocal of the gap between the inner and outer cosines, which the CPU
/// precomputes so the fragment does not divide by a cone that may be
/// degenerate. A cone with no gap -- a hard edge -- carries a zero there
/// instead, which the step below handles: the light still shines, it just does
/// not fade.
@if(LIT)
fn spot_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let offset = p - light.position_radius.xyz;
    let distance_squared = dot(offset, offset);
    let attenuation = range_attenuation(distance_squared, light.position_radius.w);
    if attenuation <= 0.0 {
        return vec3<f32>(0.0);
    }
    let toward_fragment = offset * inverseSqrt(max(distance_squared, 1.0e-8));
    let cosine = dot(toward_fragment, light.direction_outer.xyz);
    // The cone term is the smooth ramp between the two cosines, or -- when the
    // gap is too narrow to divide by -- the step at the outer one.
    var cone = clamp(
        (cosine - light.direction_outer.w) * light.cones.x,
        0.0,
        1.0,
    );
    if light.cones.x <= 0.0 {
        cone = select(0.0, 1.0, cosine >= light.direction_outer.w);
    }
    if cone <= 0.0 {
        return vec3<f32>(0.0);
    }
    let lambert = max(-dot(n, toward_fragment), 0.0);
    return light.color_intensity.rgb * (light.color_intensity.w * attenuation * (cone * cone) * lambert);
}

/// A quad light: a convex planar polygon radiating from one side of its plane.
///
/// Its irradiance is the exact diffuse form factor of the polygon, which already
/// carries the inverse-square falloff of every point of it, so the light has no
/// attenuation term of its own. The polygon radiates from one side only: a
/// fragment on the back of its plane gets nothing, which is what keeps a patch
/// from lighting what it is attached to.
@if(LIT)
fn quad_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let count = light.kind.z;
    if count < 3u {
        return vec3<f32>(0.0);
    }
    var points: array<vec3<f32>, polygon::MAX_POLYGON_VERTICES>;
    for (var i = 0u; i < count && i < polygon::MAX_POLYGON_VERTICES; i += 1u) {
        points[i] = array_access::load_light_vertex(light.kind.y + i);
    }
    // The polygon's own normal comes from its winding, which the CPU normalized
    // to face the side the light radiates from.
    let light_normal = cross(points[1] - points[0], points[2] - points[0]);
    if dot(light_normal, p - points[0]) <= 0.0 {
        return vec3<f32>(0.0);
    }
    let form_factor = polygon::polygon_form_factor(n, p, points, count);
    return light.color_intensity.rgb * (light.color_intensity.w * form_factor);
}

/// One bounded light's contribution to a fragment's irradiance, whatever kind it
/// is.
@if(LIT)
fn light_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let kind = light.kind.x;
    if kind == light::LIGHT_KIND_POINT {
        return point_radiance(light, p, n);
    }
    if kind == light::LIGHT_KIND_SPOT {
        return spot_radiance(light, p, n);
    }
    if kind == light::LIGHT_KIND_QUAD {
        return quad_radiance(light, p, n);
    }
    return vec3<f32>(0.0);
}

/// The color a lit fragment shows: its base color modulated by the ambient term,
/// every directional light, and every light its cluster cell names.
///
/// The frame-wide work does not depend on the cell: the ambient term and the
/// directional lights reach every fragment alike. The cell is then the only
/// bounded light data a fragment reads, so its cost does not grow with the number
/// of lights in the frame -- just with the number assigned to its own cell, which
/// the budget caps.
@if(LIT)
fn lit_color(in: VertexOut, base: vec4<f32>) -> vec4<f32> {
    let n = fragment_normal(in);
    var irradiance = lighting.ambient.rgb + directional_irradiance(n);
    let cell = fragment_cluster_index(fragment_ndc(in.world), view_depth(in.world));
    let count = min(array_access::load_cell_word(cell, 0u), cluster.dims.w);
    for (var i = 0u; i < count; i += 1u) {
        let index = array_access::load_cell_word(cell, i + 1u);
        irradiance += light_radiance(array_access::load_light(index), in.world, n);
    }
    return vec4<f32>(base.rgb * irradiance, base.a);
}

/// Convert a channel from sRGB (gamma) to linear light.
///
/// A base-color texture's RGB values are sRGB-encoded, whatever else the
/// variant does with them. A four-channel one is uploaded in an sRGB format
/// and so is decoded by the sampler; a luminance one cannot be, because
/// WebGPU offers the transfer function only for four-channel formats, so the
/// fragment decodes it here instead.
fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        return channel / 12.92;
    }
    return pow((channel + 0.055) / 1.055, 2.4);
}

/// The color of one fragment, before the target's encoding.
///
/// `in.color` and the base-color texel carry whatever alpha convention the
/// caller's blend state expects — premultiplied for the UI, straight for
/// meshes — and multiplying two RGBA values preserves it, so they combine
/// directly.
///
/// The texel is decoded to linear light before it is combined: `in.color` is
/// a linear factor, and a factor times an sRGB-encoded value would not be
/// either. A luminance alpha is coverage, like any other alpha, so it is
/// taken as it is.
fn fragment_color(in: VertexOut) -> vec4<f32> {
    var color = in.color;
    @if(BASE_COLOR_TEXTURE) {
        let texel = textureSample(base_color_tex, base_color_sampler, in.uv);
        // A one- or two-channel texture carries its value in `r`, and its
        // alpha — if it has one — in `g`; the channels past the last one read
        // as `0` and `1`, so an expansion has to name the ones it wants.
        @if(BASE_COLOR_LUMINANCE) {
            color = color * vec4<f32>(vec3<f32>(srgb_to_linear(texel.r)), texel.a);
        }
        @if(BASE_COLOR_LUMINANCE_ALPHA) {
            color = color * vec4<f32>(vec3<f32>(srgb_to_linear(texel.r)), texel.g);
        }
        @if(!BASE_COLOR_LUMINANCE && !BASE_COLOR_LUMINANCE_ALPHA) {
            color = color * texel;
        }
    }
    return color;
}

/// The fragment stage: the base color, lit when the variant lights, then the
/// caller's hook, then the target's encoding.
///
/// There is one exit rather than one per branch: the hook runs at the end of the
/// stage and rewrites the color that leaves it, so there has to be a single
/// color for it to rewrite.
@fragment
public fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    var color = fragment_color(in);
    // A cut-off instance draws binary coverage: it writes depth and color like
    // an opaque one wherever the fragment is opaque enough, and writes nothing
    // at all elsewhere, so it needs no blending and no back-to-front sort. The
    // cutoff is the instance's own, so two instances of one mesh can cut at
    // different alphas. Coverage is decided before the lights run, so a cut
    // fragment costs no shading at all.
    @if(ALPHA_CUTOFF) {
        if color.a < in.cutoff {
            discard;
        }
    }
    // Lighting multiplies the base color and leaves its alpha alone: alpha is
    // coverage, and a surface does not become more transparent in the dark.
    @if(LIT) {
        color = lit_color(in, color);
    }
    // unlit_wgpu::hook fragment
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
