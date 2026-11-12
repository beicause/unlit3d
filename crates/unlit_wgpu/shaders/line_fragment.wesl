// line_fragment -- how much of a pixel a stroke covers, and what it writes there.
//
// The fragment measures the distance from its own pixel to the segment's centre
// line and turns it into coverage, so a line is antialiased without the
// post-processing pass a rasterized edge would need. The two ends carry their own
// half width and their own end shape, which is how a stroke tapers and how a
// hand-drawn line gets round ends.
//
// With the `HARD_EDGE` feature set the coverage is applied as a threshold
// instead: an antialiased edge on a target with no multisampling is a veil of
// partial pixels, and an opaque stroke has no colour to veil the background
// with, so it cuts cleanly at the pixel it covers and writes its depth there.
// A multisampled target sets the feature aside rather than jags the edge: its
// alpha-to-coverage mask spreads the same coverage, so the variant only sets
// `HARD_EDGE` on a single-sampled one.
//
// A caller that composites over the stroke, or replaces its colour, writes a
// module of its own: `coverage` is the whole measurement, and what to do with
// the pixels it covers is the caller's business rather than this file's.

import package::line_vertex;

/// The coverage of the pixel `in` describes, from zero to one.
public fn coverage(in: line_vertex::VertexOutput) -> f32 {
    let direction = in.direction;
    let along = dot(in.offset, direction);
    // Distance from the infinite line the segment lies on.
    let perpendicular = abs(in.offset.x * direction.y - in.offset.y * direction.x);
    let half_width = in.half_width;

    // The stroke is a signed distance field: `d` is zero at its edge,
    // negative inside it, and the coverage ramps over the one pixel either
    // side of the edge. The body is a band half a width either side of the
    // axis.
    var d = perpendicular - half_width;
    if (in.span > line_vertex::POINT_SPAN) {
        // Past an end the shape of the end decides what the edge is: a
        // round one keeps the disc the body's band rolls into, and a flat
        // one keeps the band but stops it at the station, which is the box
        // the band and the end face bound. A pixel past both the face and
        // the band measures to the corner they cut, so the end face and the
        // side edge join smoothly.
        if (along < 0.0) {
            let round_a = (in.caps & 1u) != 0u;
            let q = vec2<f32>(-along, perpendicular - half_width);
            d = select(
                length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0),
                length(in.offset) - half_width,
                round_a,
            );
        } else if (along > in.span) {
            let round_b = (in.caps & 2u) != 0u;
            let q = vec2<f32>(along - in.span, perpendicular - half_width);
            d = select(
                length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0),
                length(in.offset - direction * in.span) - half_width,
                round_b,
            );
        }
    } else {
        // The ends project to the same pixel, so there is no along
        // direction to bound them with: the stroke draws as a dot of the
        // width the stage stated for it -- a dot's own, or the widest end
        // a near-plane cut leaves in sight.
        d = length(in.offset) - half_width;
    }

    // One pixel of coverage either side of the edge: a line is analytic rather
    // than rasterized, so its own alpha is what antialiases it.
    var coverage = clamp(0.5 - d, 0.0, 1.0);
    // A line thinner than a pixel keeps its energy rather than flickering
    // between covered and missed as it crosses the pixel grid.
    if (half_width < 0.5) {
        coverage = coverage * clamp(half_width * 2.0, 0.0, 1.0);
    }
    return coverage;
}

/// The hard-edge cut: a pixel covered less than half is dropped, and one the
/// stroke covers writes its own alpha rather than the coverage-scaled one.
@if(HARD_EDGE) public fn cut_at_edge(in: line_vertex::VertexOutput, coverage: f32, color: vec4<f32>) -> vec4<f32> {
    // An opaque stroke on a target with no multisampling: a partial pixel
    // would blend the stroke's own colour over the background as if it
    // were translucent, so the stroke cuts at the pixel it covers instead.
    if (coverage < 0.5) {
        discard;
    }
    return vec4<f32>(color.rgb, in.color.a);
}

/// The fragment stage: the colour `in`'s pixel receives.
public fn fs_stage(in: line_vertex::VertexOutput) -> vec4<f32> {
    let covered = coverage(in);
    let color = vec4<f32>(in.color.rgb, in.color.a * covered);
    @if(HARD_EDGE) {
        return cut_at_edge(in, covered, color);
    }
    return color;
}
