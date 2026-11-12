// lit_vertex -- the vertex stage: what a vertex is built from, and where it goes.
//
// The stage runs in two halves with the world-space vertex between them:
//
// * `vertex_world` decodes the vertex's own channels, deforms it, places it
//   with the instance transform and widens it into a hull. It returns a
//   `VertexStage`: the world-space vertex, together with the mesh metadata the
//   second half reads, so the metadata is loaded once.
// * `vertex_out` finishes the stage from the vertex it is handed: the clip
//   position, the color, the UV and, for a lit variant, the world position and
//   normal the fragment stage shades with.
//
// A caller displaces `VertexStage::world` between the two calls. That is the
// one place the geometry can be reshaped without standing in for the whole
// stage: the clip position, the world position the lights address and the
// normal all follow whatever the caller leaves there.
//
// `vs_stage` is the two calls with nothing in between, which is what the
// built-in `lit.wesl` runs.
//
// The structs are exported because they are the stage's interface: a caller's
// own entry point names them to declare its parameter and return type.

import package::array_access;
import package::lit_bindings;
import package::mesh_compression;
import package::mesh_metadata;

/// What one vertex carries: its own channels, from the vertex streams, and the
/// per-instance record that places it.
///
/// The per-instance fields are unconditional: every built-in variant declares and
/// binds the instance stream, because the family always has an instance buffer to
/// bind -- a draw with no instance state of its own binds the identity record. A
/// variant that reads two of the record's fields still declares the whole record
/// at its own offsets, which costs nothing but a declared attribute.
public struct VertexInput {
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

    /// Per-instance hull width, Float32: how far the vertex is pushed out
    /// along its normal, in world units.
    @if(EXPANDED) @location(14) hull_width: f32,
}

public struct VertexOut {
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
public fn skin_position(
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
public fn morph_position(
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

/// Where `vertex_world` placed a vertex, and what `vertex_out` still needs.
///
/// The metadata member exists exactly where something reads it -- a compressed
/// channel to decode or a morph target to address -- and is absent otherwise, so
/// a variant that reads neither carries only the vertex.
public struct VertexStage {
    /// The world-space vertex the instance transform placed, after any hull
    /// expansion. A caller displaces it here, and `vertex_out` follows.
    world: vec3<f32>,
    /// The mesh's decode parameters and addressing, loaded once for this vertex
    /// and handed to the UV decode in `vertex_out`.
    @if(COMPRESSED_CHANNEL_OR_MORPH) metadata: mesh_metadata::MeshMetadata,
}

/// Place a vertex in the world: decode it, deform it, transform it and widen it
/// into a hull where the variant draws one.
///
/// Deformation happens in the mesh's own space, before the instance transform
/// places the vertex in the world: a joint matrix is relative to the mesh, and a
/// morph displacement is a local offset.
///
/// `vertex_index` is the draw's vertex ordinal in the stream's pool --
/// `firstVertex + i`, or an index plus `baseVertex` -- so the morph
/// displacements, which are stored per mesh, are addressed relative to the
/// mesh's own first vertex.
public fn vertex_world(in: VertexInput, vertex_index: u32) -> VertexStage {
    // The block form would scope `metadata` to the block, so the declaration
    // itself carries the condition.
    //
    // The struct carries both the decode parameters and the addressing, and
    // the index comes from the instance stream, so anything that reads either
    // loads it once here.
    @if(COMPRESSED_CHANNEL_OR_MORPH)
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
    // The pose the instance deforms by is addressed through the instance
    // record's two bases: the joint matrices and the morph weights are
    // frame-wide arrays, and every instance names its own slice of them. That
    // is what lets two instances of one mesh hold different poses.
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
    // Mutable rather than a `let` because it is handed back: a caller may
    // displace it between this call and `vertex_out`, and everything after that
    // point -- the clip position, the world position the lights are addressed
    // from and the normal -- follows the displaced vertex.
    var world = transform_point(in.model_0, in.model_1, in.model_2, local);

    // A hull is the same mesh, a little larger, so that what pokes out past the
    // mesh drawn over it reads as a line around it. The widening is along the
    // world-space normal and measured in world units, which keeps an outline
    // the same thickness however the instance scales its mesh.
    @if(EXPANDED) {
        let outward = transform_vector(
            in.model_0,
            in.model_1,
            in.model_2,
            mesh_compression::decode_normal(in.normal),
        );
        // An instance scaled to nothing along an axis has no direction left
        // to spread along, and normalizing the zero vector it hands back would
        // put a NaN into every vertex of the shell, which the rasterizer then
        // drops. A shell that cannot be spread stays where its body is, which
        // is what a width of zero draws anyway.
        let reach = length(outward);
        if (reach > 1.0e-6) {
            world += outward / reach * in.hull_width;
        }
    }

    var stage: VertexStage;
    stage.world = world;
    @if(COMPRESSED_CHANNEL_OR_MORPH) {
        stage.metadata = metadata;
    }
    return stage;
}

/// Finish the vertex stage from the vertex `vertex_world` placed.
///
/// The instance transform always places the vertex, so a screen-space draw such
/// as a UI pass binds the identity record and the transform is a no-op.
public fn vertex_out(in: VertexInput, stage: VertexStage) -> VertexOut {
    var out: VertexOut;
    out.clip = lit_bindings::camera.clip_from_world * vec4<f32>(stage.world, 1.0);
    out.color = in.base_color;
    @if(ALPHA_CUTOFF) {
        out.cutoff = in.cutoff;
    }
    @if(VERTEX_UV && UNCOMPRESSED_UV) {
        out.uv = in.uv;
    }
    @if(VERTEX_UV && !UNCOMPRESSED_UV) {
        out.uv = mesh_compression::decode_uv(in.uv, stage.metadata.uv_min_and_extents);
    }
    @if(VERTEX_COLOR) {
        out.color = out.color * in.color;
    }
    // A lit fragment addresses the frame's world-space lights and asks which
    // cell it lands in, so the vertex stage hands over the world position and
    // the normal it shades with.
    @if(LIT) {
        out.world = stage.world;
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

/// The built-in vertex stage: place the vertex and finish it, with no
/// displacement in between.
public fn vs_stage(in: VertexInput, vertex_index: u32) -> VertexOut {
    let stage = vertex_world(in, vertex_index);
    return vertex_out(in, stage);
}
