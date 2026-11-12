// lit_lighting -- how a lit fragment is shaded.
//
// Every declaration here belongs to the LIT flag: an unlit variant compiles none
// of them, and none of the resources they read.
//
// The terms are separated by what they vary with, which is also what lets a
// caller use them one at a time. The ambient term and the directional lights
// reach every fragment alike and are frame-wide; a bounded light is reached only
// through the cluster cell the fragment lands in. `lit_color` is the built-in
// combination of all of them, and the pieces it builds on are exported for a
// stage that composites its own result -- a debug view of the light grid reads
// the cell and decides the color itself.

import package::array_access;
import package::lit_bindings;
import package::lit_vertex;
import package::light;
import package::polygon;

/// The world-space normal a lit fragment shades with.
///
/// A mesh that carries normals hands one over per vertex, already interpolated
/// and transformed; a mesh without them is shaded by the geometric normal of
/// the triangle it belongs to, taken from the world positions across the
/// primitive's screen-space derivatives. The geometric normal has no side of
/// its own -- a triangle and its reverse are the same triangle -- so it is
/// turned toward the eye, which is the side a back-face culled draw presents.
@if(LIT)
public fn fragment_normal(in: lit_vertex::VertexOut) -> vec3<f32> {
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
    let to_eye = lit_bindings::camera.camera_position.xyz - in.world;
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
public fn fragment_ndc(world: vec3<f32>) -> vec2<f32> {
    let clip = lit_bindings::camera.clip_from_world * vec4<f32>(world, 1.0);
    return clip.xy / clip.w;
}

/// The linear depth of a world-space point: its distance along the camera's
/// view direction, which is what the depth slices are cut by.
@if(LIT)
public fn view_depth(world: vec3<f32>) -> f32 {
    let view = lit_bindings::camera.view_from_world * vec4<f32>(world, 1.0);
    return -view.z;
}

/// The depth slice containing `depth`, before clamping to the grid.
///
/// Slice zero absorbs the near end. Perspective slices are logarithmic after
/// that boundary; orthographic slices are linear. The continuous coordinate is
/// returned as well so a debug view can draw the exact boundary the lookup
/// floors, without maintaining a second mapping.
@if(LIT)
public fn cluster_slice_coordinate(depth: f32) -> f32 {
    if depth < lit_bindings::cluster.depth.w {
        return 0.0;
    }
    if lit_bindings::cluster.perspective.x != 0u {
        return (log(depth) - lit_bindings::cluster.depth.y) * lit_bindings::cluster.depth.x * lit_bindings::cluster.depth.z + 1.0;
    }
    return (depth - lit_bindings::cluster.depth.y) * lit_bindings::cluster.depth.x * lit_bindings::cluster.depth.z;
}

@if(LIT)
public fn cluster_depth_slice(depth: f32) -> u32 {
    return u32(clamp(floor(cluster_slice_coordinate(depth)), 0.0, f32(lit_bindings::cluster.dims.z - 1u)));
}

/// The cluster cell a fragment at `ndc` and `depth` falls in.
///
/// `ndc` is the fragment's own place on screen, which [`fragment_ndc`] derives
/// from the world position it interpolates.
@if(LIT)
public fn fragment_cluster_index(ndc: vec2<f32>, depth: f32) -> u32 {
    let dims = lit_bindings::cluster.dims.xyz;
    let x = clamp(floor((ndc.x * 0.5 + 0.5) * f32(dims.x)), 0.0, f32(dims.x - 1u));
    let y = clamp(floor((ndc.y * 0.5 + 0.5) * f32(dims.y)), 0.0, f32(dims.y - 1u));
    return (cluster_depth_slice(depth) * dims.y + u32(y)) * dims.x + u32(x);
}

/// A light at infinity: one direction, no falloff.
@if(LIT)
public fn directional_radiance(light: light::DirectionalGpu, n: vec3<f32>) -> vec3<f32> {
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
public fn directional_irradiance(n: vec3<f32>) -> vec3<f32> {
    var irradiance = vec3<f32>(0.0);
    for (var i = 0u; i < light::MAX_DIRECTIONAL_LIGHTS; i += 1u) {
        irradiance += directional_radiance(lit_bindings::lighting.directionals[i], n);
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
public fn range_attenuation(distance_squared: f32, range: f32) -> f32 {
    if range <= 0.0 {
        return 0.0;
    }
    let ratio = distance_squared / (range * range);
    let window = max(1.0 - ratio * ratio, 0.0);
    return window * window / max(distance_squared, 1.0e-8);
}

/// A point light, or the point-light part of a spot light.
@if(LIT)
public fn point_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
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
public fn spot_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
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
public fn quad_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let count = light.kind.z;
    if count < 3u {
        return vec3<f32>(0.0);
    }
    var points: array<vec3<f32>, polygon::QUAD_LIGHT_CORNERS>;
    for (var i = 0u; i < count && i < polygon::QUAD_LIGHT_CORNERS; i += 1u) {
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
public fn light_radiance(light: light::LightGpu, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
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
public fn lit_color(in: lit_vertex::VertexOut, base: vec4<f32>) -> vec4<f32> {
    let n = fragment_normal(in);
    var irradiance = lit_bindings::lighting.ambient.rgb + directional_irradiance(n);
    let cell = fragment_cluster_index(fragment_ndc(in.world), view_depth(in.world));
    let count = min(array_access::load_cell_word(cell, 0u), lit_bindings::cluster.dims.w);
    for (var i = 0u; i < count; i += 1u) {
        let index = array_access::load_cell_word(cell, i + 1u);
        irradiance += light_radiance(array_access::load_light(index), in.world, n);
    }
    return vec4<f32>(base.rgb * irradiance, base.a);
}
