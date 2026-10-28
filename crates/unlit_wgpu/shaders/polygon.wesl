// polygon -- the diffuse form factor of a convex planar polygon.
//
// A quad light radiates from one side of its plane. The irradiance it delivers
// to a surface point is the integral of the cosine at the receiver times the
// cosine at the emitter over the polygon's area, divided by the squared
// distance -- the same quantity a point light's `intensity / d²` approximates,
// but integrated exactly. For a Lambert surface the integral has a closed form
// (Lambert 1760, rediscovered by Baumgart): sum each edge's spherical excess
// around the point, which leaves only a handful of `acos` calls and no lookup
// table, no matrix and no view vector -- a diffuse form factor does not depend
// on where the eye is.
//
// The same polygon appears as a quad light's four corners or, once a horizon
// clip has cut it, as up to `MAX_POLYGON_VERTICES` points. One extra vertex is
// all a convex polygon needs: clipping a convex polygon by one half-space adds
// at most one corner.

/// The most corners a clipped polygon can have: a quad cut by the receiver's
/// horizon plane.
public const MAX_POLYGON_VERTICES: u32 = 5u;

/// One edge's contribution to the polygon's form factor.
///
/// `v1` and `v2` are unit vectors from the receiving point to two consecutive
/// corners. The edge contributes the angle they subtend, weighted by the
/// component of their cross product along the receiving normal. The angle is
/// divided by the cross product's length -- `θ / sin θ` -- rather than written
/// as `θ * normalize(cross)`, because the cross product vanishes as the two
/// directions meet and an explicit division stays defined there; the limit is
/// one, so a vanishing edge contributes its angle and no more.
fn polygon_edge_contribution(n: vec3<f32>, v1: vec3<f32>, v2: vec3<f32>) -> f32 {
    let edge = cross(v1, v2);
    let sine = length(edge);
    if sine < 1.0e-6 {
        // The two directions coincide: the edge subtends no area, so its angle
        // is already zero and nothing has to be divided.
        return 0.0;
    }
    let angle = acos(clamp(dot(v1, v2), -1.0, 1.0));
    return dot(n, edge) * (angle / sine);
}

/// The form factor of `count` of `points` seen from `p` through a surface with
/// normal `n`.
///
/// The result is the irradiance a polygon of unit radiance delivers, so a light
/// of colour `c` and intensity `i` adds `c * i * form_factor` -- the same shape
/// as a point light's `c * i / d²`, and the reason a quad light needs no
/// attenuation term of its own.
///
/// Only the part of the polygon above the receiver's plane contributes. The
/// clip runs before the sum because the closed form assumes every corner lies
/// in the upper hemisphere: a corner below the plane would make an edge cross
/// the horizon and the angle sum would count area the surface cannot see.
///
/// `points` is passed by value and its length is fixed at
/// [`MAX_POLYGON_VERTICES`]; `count` says how many entries are live.
public fn polygon_form_factor(
    n: vec3<f32>,
    p: vec3<f32>,
    points: array<vec3<f32>, MAX_POLYGON_VERTICES>,
    count: u32,
) -> f32 {
    // Sutherland-Hodgman against the plane through `p`: keep what a surface
    // facing `n` can see. A convex polygon crosses that plane at most twice,
    // so the output has at most one corner more than the input.
    var clipped: array<vec3<f32>, MAX_POLYGON_VERTICES>;
    var clipped_count = 0u;
    for (var i = 0u; i < count; i += 1u) {
        let current = points[i] - p;
        let next = points[(i + 1u) % count] - p;
        let current_above = dot(current, n);
        let next_above = dot(next, n);
        if current_above >= 0.0 && clipped_count < MAX_POLYGON_VERTICES {
            clipped[clipped_count] = current;
            clipped_count += 1u;
        }
        // The edge crosses the horizon: interpolate the point where it does.
        if (current_above >= 0.0) != (next_above >= 0.0) && clipped_count < MAX_POLYGON_VERTICES {
            let crossing = current_above / (current_above - next_above);
            clipped[clipped_count] = current + crossing * (next - current);
            clipped_count += 1u;
        }
    }
    if clipped_count < 3u {
        // A corner, an edge, or nothing: no area, no irradiance.
        return 0.0;
    }

    var sum = 0.0;
    for (var i = 0u; i < clipped_count; i += 1u) {
        let v1 = clipped[i] * inverseSqrt(max(dot(clipped[i], clipped[i]), 1.0e-20));
        let next = clipped[(i + 1u) % clipped_count];
        let v2 = next * inverseSqrt(max(dot(next, next), 1.0e-20));
        sum += polygon_edge_contribution(n, v1, v2);
    }
    // Each edge contributes one angle of the spherical polygon; the half
    // converts the angle sum into the solid angle the polygon covers.
    //
    // The sum comes out negative here because a quad light winds its corners
    // so that their cross product points along the direction it radiates --
    // toward the receiver, the opposite of the receiver's normal. The winding
    // has to face the receiver that way: the back-face test rejects a fragment
    // behind the patch by comparing against the same cross product. A polygon
    // closing the other way would need no negation, and the form factor itself
    // is unsigned.
    return -0.5 * sum;
}
