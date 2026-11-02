// line -- screen-space line segments of constant pixel width.
//
// One segment is one instance: the vertex stage derives the quad that covers
// it from `@builtin(vertex_index)`, so the stream carries segments rather than
// vertices and a caller never expands a segment on the CPU. Both ends are
// projected and measured in pixels against `view::View::viewport_size`, which
// is what makes the width a pixel count rather than a world distance: a stroke
// keeps its thickness at any distance from the camera and across any viewport
// size.
//
// The fragment measures the distance from its own pixel to the segment's centre
// line and turns it into coverage, so a line is antialiased without the
// post-processing pass a rasterized edge would need. The two ends carry their
// own half width and their own end shape, which is how a stroke tapers and how
// a hand-drawn line gets round ends.

import package::camera as camera_uniform;
import package::view;

@group(0) @binding(0) var<uniform> camera: camera_uniform::Camera;
// The pass's viewport. A stroke's width is a pixel count, so this is the one
// uniform besides the camera that the vertex stage reads.
@group(0) @binding(2) var<uniform> viewport: view::View;

/// How far past the half width the quad reaches, in pixels.
///
/// Coverage falls off across one pixel either side of the line's edge, so the
/// quad has to be wider than the line by that pixel; a fragment outside the
/// quad is never rasterized and therefore never covered.
const AA_RADIUS: f32 = 1.0;

/// The six corners of the quad, as (station, side).
///
/// `station` runs from the `a` end (zero) to the `b` end (one), `side` runs
/// from one edge of the line to the other. Two triangles rather than a strip:
/// the quad is the same six vertices either way, and a list keeps each segment
/// independent of its neighbours.
const CORNERS: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2<f32>(0.0, -1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, 1.0),
);

/// One segment, as the instance stream carries it.
///
/// Mirrors `crate::line::LineSegment` on the Rust side.
struct VertexInput {
    /// Start and end of the segment, in world space.
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    /// Linear colour at each end; alpha is coverage.
    @location(2) color_a: vec4<f32>,
    @location(3) color_b: vec4<f32>,
    /// Half the line's width at each end, in physical pixels.
    @location(4) half_width: vec2<f32>,
    /// Bit 0 rounds the `a` end, bit 1 the `b` end.
    @location(5) caps: u32,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    /// Pixel offset from the `a` end, interpolated across the quad.
    ///
    /// The quad is emitted with `w` of one, so the rasterizer interpolates
    /// this affinely and a fragment reads its true pixel offset; a
    /// perspective-correct interpolation would bend the distance the fragment
    /// is about to measure.
    @location(0) offset: vec2<f32>,
    /// Unit direction from `a` to `b`, in pixels.
    @location(1) @interpolate(flat) direction: vec2<f32>,
    /// The segment's length in pixels; zero for a segment that collapses to a
    /// point, which draws as a dot.
    @location(2) @interpolate(flat) span: f32,
    /// Half width at each end, in pixels.
    @location(3) @interpolate(flat) half_width_a: f32,
    @location(4) @interpolate(flat) half_width_b: f32,
    /// The end shapes, as `VertexInput::caps`.
    @location(5) @interpolate(flat) caps: u32,
    /// The colour, interpolated along the segment.
    @location(6) color: vec4<f32>,
};

/// A segment after clipping, and whether any of it survived.
struct Clipped {
    a: vec4<f32>,
    b: vec4<f32>,
    /// Where the kept part begins and ends on the segment the caller stated,
    /// from the `a` end (zero) to the `b` end (one).
    ///
    /// The width and the colour are stated at the caller's ends, while the
    /// geometry runs between the kept ends: the fragment measures its own
    /// pixel against the quad, which the near plane shortened. Remapping these
    /// back into the caller's parameter is what keeps the two in step after a
    /// cut, so the colour a fragment reads is the colour at the point it
    /// covers.
    t0: f32,
    t1: f32,
    visible: bool,
};

/// Clip the segment against the near plane, where clip `w` has to stay
/// positive.
///
/// The projection is infinite, so a point level with or behind the eye has no
/// screen position: dividing by its `w` would put its pixel coordinates at
/// infinity and the quad would sweep across the whole target. Cutting the
/// segment where `w` reaches zero keeps the visible part, and dropping a
/// segment whose both ends are behind the eye keeps the frame free of it.
fn clip_near(a: vec4<f32>, b: vec4<f32>) -> Clipped {
    let epsilon = 1.0e-5;
    if (a.w > epsilon && b.w > epsilon) {
        return Clipped(a, b, 0.0, 1.0, true);
    }
    if (a.w <= epsilon && b.w <= epsilon) {
        // Nothing is drawn, so the stations say nothing and are left whole.
        return Clipped(a, b, 0.0, 1.0, false);
    }
    let t = (epsilon - a.w) / (b.w - a.w);
    let cut = mix(a, b, t);
    if (a.w <= epsilon) {
        return Clipped(cut, b, t, 1.0, true);
    }
    return Clipped(a, cut, 0.0, t, true);
}

/// The pixel position of a clip-space point, in the viewport's own pixels.
///
/// WebGPU's clip space has `y` up and its pixel space has `y` down, so the
/// vertical axis flips here.
fn pixel_of(clip: vec4<f32>, size: vec2<f32>) -> vec2<f32> {
    let ndc = clip.xy / clip.w;
    return vec2<f32>((ndc.x * 0.5 + 0.5) * size.x, (0.5 - ndc.y * 0.5) * size.y);
}

@vertex
fn vs_main(input: VertexInput, @builtin(vertex_index) index: u32) -> VertexOutput {
    let corner = CORNERS[index];
    let station = corner.x;
    let side = corner.y;

    let clipped = clip_near(
        camera.clip_from_world * vec4<f32>(input.a, 1.0),
        camera.clip_from_world * vec4<f32>(input.b, 1.0),
    );

    let size = max(viewport.viewport_size, vec2<f32>(1.0, 1.0));
    let start = pixel_of(clipped.a, size);
    let end = pixel_of(clipped.b, size);
    let delta = end - start;
    let span = length(delta);
    // A segment whose ends coincide has no direction of its own; one is named
    // so the quad still spans the dot the cap draws.
    let direction = select(
        vec2<f32>(1.0, 0.0),
        delta / max(span, 1.0e-6),
        span > 1.0e-6,
    );
    let normal = vec2<f32>(-direction.y, direction.x);

    let round_a = (input.caps & 1u) != 0u;
    let round_b = (input.caps & 2u) != 0u;
    let half_width_a = max(input.half_width.x, 0.0);
    let half_width_b = max(input.half_width.y, 0.0);
    // The quad is built between the kept ends, but the width and the colour are
    // stated at the caller's own ends: a near-plane cut moves the ends inward,
    // and taking the corner's station straight would read the width and the
    // colour of a point the quad no longer reaches.
    let at = mix(clipped.t0, clipped.t1, station);
    let half_width = mix(half_width_a, half_width_b, at);
    // A round end reaches past its station by the cap's own radius; a flat one
    // stops there and needs only the antialiasing margin. Only an end the near
    // plane kept is a visible end, so a cut end draws no cap.
    let cap_a = select(0.0, 1.0, round_a && clipped.t0 <= 0.0);
    let cap_b = select(0.0, 1.0, round_b && clipped.t1 >= 1.0);
    let reach_a = cap_a * half_width_a + AA_RADIUS;
    let reach_b = cap_b * half_width_b + AA_RADIUS;

    let centre = mix(start - direction * reach_a, end + direction * reach_b, station);
    let offset = centre + normal * side * (half_width + AA_RADIUS);
    let ndc = vec2<f32>(
        offset.x / size.x * 2.0 - 1.0,
        1.0 - offset.y / size.y * 2.0,
    );
    // Depth is mixed rather than divided: the quad is emitted with `w` of one
    // so its varyings stay affine in pixel space, which leaves the depth
    // interpolating linearly across the segment instead of in perspective. The
    // error is bounded by the segment's own depth range, and a stroke is cut
    // into segments short enough for it to vanish.
    let depth = mix(clipped.a.z / clipped.a.w, clipped.b.z / clipped.b.w, station);

    var out: VertexOutput;
    // A segment the near plane removed entirely is put outside the clip volume,
    // where the rasterizer drops it without a draw of its own.
    out.clip = select(vec4<f32>(0.0, 0.0, -1.0, 1.0), vec4<f32>(ndc, clamp(depth, 0.0, 1.0), 1.0), clipped.visible);
    out.offset = offset - start;
    out.direction = direction;
    out.span = span;
    out.half_width_a = half_width_a;
    out.half_width_b = half_width_b;
    out.caps = input.caps;
    out.color = mix(input.color_a, input.color_b, at);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let direction = in.direction;
    let along = dot(in.offset, direction);
    // Distance from the infinite line the segment lies on.
    let perpendicular = abs(in.offset.x * direction.y - in.offset.y * direction.x);
    let station = clamp(along, 0.0, in.span) / max(in.span, 1.0e-4);
    let half_width = mix(in.half_width_a, in.half_width_b, station);

    // Past an end the shape of the end decides how the distance is measured: a
    // round one measures to the end point, so the cap is a disc, and a flat one
    // measures to the corner the segment cuts at, so the line simply stops.
    var distance = perpendicular;
    if (along < 0.0) {
        let round_a = (in.caps & 1u) != 0u;
        distance = select(length(vec2<f32>(-along, perpendicular)), length(in.offset), round_a);
    } else if (along > in.span) {
        let round_b = (in.caps & 2u) != 0u;
        let overshoot = along - in.span;
        distance = select(
            length(vec2<f32>(overshoot, perpendicular)),
            length(in.offset - direction * in.span),
            round_b,
        );
    }

    // One pixel of coverage either side of the edge: a line is analytic rather
    // than rasterized, so its own alpha is what antialiases it.
    var coverage = clamp(half_width + 0.5 - distance, 0.0, 1.0);
    // A line thinner than a pixel keeps its energy rather than flickering
    // between covered and missed as it crosses the pixel grid.
    if (half_width < 0.5) {
        coverage = coverage * clamp(half_width * 2.0, 0.0, 1.0);
    }
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
