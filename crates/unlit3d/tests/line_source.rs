//! GPU coverage for the screen-space line source.
//!
//! A [`LineSource`] draws its segments as screen-space quads: each one is
//! expanded to a constant width in physical pixels, whatever its distance from
//! the camera, and its ends are either squared off or rounded.
//!
//! These tests draw one segment at a time and measure the result, because that
//! is what the source promises and what a stored frame cannot say: that the
//! width is measured in pixels rather than in world units, that a segment keeps
//! its width when the camera moves away, that a rounded end reaches further
//! than a square one, and that what the segment passes behind hides it.

use crate::common::*;
use unlit_ecs::bundle;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::prelude::*;

/// The colour a segment is drawn in, as the segment stream carries it.
///
/// Nearly white, so a pixel is either part of the stroke or the clear colour
/// and nothing in between has to be classified.
const INK: [u8; 4] = [255, 255, 255, 255];

/// The camera looks at the origin from here, down its own Z axis.
///
/// Looking straight down -Z means a segment along X lands square across the
/// frame at a known number of pixels per world unit, which is what turns a
/// measured width back into the pixel width the source was asked for.
const EYE: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 4.0);

/// The width the strokes below are drawn at, in physical pixels.
const HALF_WIDTH: f32 = 4.0;

/// A world-space length the segment spans, from -LENGTH to +LENGTH on X.
///
/// It is long enough to reach both edges of the frame, so the measurement
/// never runs into a segment's own ends.
const LENGTH: f32 = 8.0;

/// Whether `pixel` holds anything the clear did not put there.
fn drawn(pixel: [u8; 4]) -> bool {
    // The stroke is near-white and the clear colour is black, so the red
    // channel alone separates them.
    pixel[0] > 128
}

/// The rows of a frame that hold a drawn pixel, as a first and last index.
fn drawn_rows(frame: &Frame) -> Option<(u32, u32)> {
    let mut first = None;
    let mut last = None;
    for y in 0..frame.height {
        if (0..frame.width).any(|x| drawn(frame.pixel_u8(x, y))) {
            first.get_or_insert(y);
            last = Some(y);
        }
    }
    first.zip(last)
}

/// The rows of `column` that hold a drawn pixel, counted.
fn column_height(frame: &Frame, column: u32) -> Option<u32> {
    let covered = (0..frame.height)
        .filter(|&y| drawn(frame.pixel_u8(column, y)))
        .count();
    (covered > 0).then_some(covered as u32)
}

/// The frame `target` was rendered into, read back from the GPU.
fn read_frame(ctx: &Ctx, target: &wgpu::Texture) -> Frame {
    Frame {
        rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, target),
        width: WIDTH,
        height: HEIGHT,
    }
}

/// A world holding one stroke and a camera looking down -Z at it.
///
/// The camera sits at `eye`, so the same segment drawn from further away has
/// to cover fewer pixels — unless its width is in pixels, which is the point.
fn segment_world(ctx: &Ctx, eye: glam::Vec3, stroke: Line) -> (World, TestGpu, wgpu::Texture) {
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, ctx);

    let source_entity = gpu.spawn_source(&mut world, LineSource::new());
    gpu.spawn_in(&mut world, source_entity, bundle![stroke]);

    let camera = camera_looking_at(eye, glam::Vec3::ZERO, WIDTH as f32 / HEIGHT as f32);
    gpu.spawn_in(&mut world, source_entity, bundle![camera]);

    let target = gpu.bind_offscreen_target(&world, "test::line_source");
    (world, gpu, target)
}

/// A segment lying along X at the origin, drawn at the default width.
fn along_x() -> LineSegment {
    LineSegment::new([-LENGTH, 0.0, 0.0], [LENGTH, 0.0, 0.0])
        .with_color(INK)
        .with_half_width(HALF_WIDTH)
}

/// A stroke is as many pixels wide as it was asked for, whatever the camera's
/// distance.
///
/// The segment lies along X and the camera looks down -Z, so the stroke lands
/// as a horizontal band whose height is its width. The same segment drawn from
/// twice as far away covers half as many world units per pixel, so a width in
/// world units would halve; a width in pixels stays put, which is the promise.
async fn a_stroke_keeps_its_pixel_width_at_any_distance() {
    let ctx = Ctx::headless().await;

    let mut heights = Vec::new();
    for eye in [EYE, EYE * 2.0] {
        let (world, gpu, target) = segment_world(&ctx, eye, Line::from(along_x()));
        gpu.render(&world);
        let frame = read_frame(&ctx, &target);
        let (first, last) = drawn_rows(&frame).expect("the stroke is drawn");
        heights.push((eye.z, last - first + 1));
    }

    let (near, far) = (heights[0].1, heights[1].1);
    // The stroke is `2 * HALF_WIDTH` pixels across; the two distances may
    // differ by the pixel a rasterizer rounds the edge either way.
    let expected = (2.0 * HALF_WIDTH) as u32;
    for (distance, height) in &heights {
        assert!(
            height.abs_diff(expected) <= 1,
            "a {expected}-pixel stroke at distance {distance} covered {height} rows"
        );
    }
    assert_eq!(
        near, far,
        "a stroke twice as far away should keep its width, {near} then {far}"
    );
}

/// A wider stroke covers more pixels, in proportion to the width asked for.
async fn a_wider_stroke_covers_more_pixels() {
    let ctx = Ctx::headless().await;

    let mut covered = Vec::new();
    for half_width in [2.0, 6.0] {
        let segment = along_x().with_half_width(half_width);
        let (world, gpu, target) = segment_world(&ctx, EYE, Line::from(segment));
        gpu.render(&world);
        let frame = read_frame(&ctx, &target);
        let (first, last) = drawn_rows(&frame).expect("the stroke is drawn");
        covered.push((half_width, last - first + 1));
    }

    let (thin, thick) = (covered[0].1, covered[1].1);
    // Three times the half width is three times the covered height, less the
    // pixel the edges round.
    let growth = thick as i64 - thin as i64;
    assert!(
        (5..=11).contains(&growth),
        "tripling the half width should add about eight rows, got {growth} ({thin} then {thick})"
    );
}

/// A rounded end reaches past the station, and a square one stops at it.
///
/// Both ends of the segment are inside the frame, so their extent is visible:
/// a round cap covers a half-disc beyond the station, which is half a width of
/// extra reach, while a flat cap ends flat.
async fn a_round_cap_reaches_past_its_station_and_a_flat_one_does_not() {
    let ctx = Ctx::headless().await;

    // A short segment well inside the frame, along X, so both ends' shapes are
    // visible against the clear colour.
    let half = 1.0;
    let ends = [
        (0, "flat"),
        (LineSegment::ROUND_A | LineSegment::ROUND_B, "round"),
    ];
    let mut widths = Vec::new();
    for (caps, label) in ends {
        let mut segment = LineSegment::new([-half, 0.0, 0.0], [half, 0.0, 0.0])
            .with_color(INK)
            .with_half_width(HALF_WIDTH);
        segment.caps = caps;
        let (world, gpu, target) = segment_world(&ctx, EYE, Line::from(segment));
        gpu.render(&world);
        let frame = read_frame(&ctx, &target);
        let (first, last) =
            columns(&frame).unwrap_or_else(|| panic!("the {label} stroke is drawn"));
        widths.push((label, last - first + 1));
    }

    let (_, flat) = widths[0];
    let (_, round) = widths[1];
    assert!(
        round > flat,
        "a round cap should reach further than a flat one, {flat} then {round}"
    );
    // Each round end adds half a width of reach, so the two differ by about a
    // width in total; the pixel edges round either side.
    let extra = round as i64 - flat as i64;
    assert!(
        (6..=11).contains(&extra),
        "rounding both ends of a {}-pixel stroke should add about eight pixels, got {extra}",
        2.0 * HALF_WIDTH
    );
}

/// The columns of a frame that hold a drawn pixel, as a first and last index.
fn columns(frame: &Frame) -> Option<(u32, u32)> {
    let mut first = None;
    let mut last = None;
    for x in 0..frame.width {
        if (0..frame.height).any(|y| drawn(frame.pixel_u8(x, y))) {
            first.get_or_insert(x);
            last = Some(x);
        }
    }
    first.zip(last)
}

/// The colour the occlusion test draws its stroke in.
///
/// Pure red, which nothing else in the frame produces: the cube's own faces
/// carry green and blue as well, so a pixel that is red and nothing else is
/// the stroke.
const STROKE: [u8; 4] = [255, 0, 0, 255];

/// Whether `pixel` is the stroke's own colour.
fn is_stroke(pixel: [u8; 4]) -> bool {
    pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60
}

/// A stroke that passes behind a body is hidden by it.
///
/// The frame draws a cube and a stroke that runs across it, further from the
/// camera than the cube's front face. Depth testing is on and the stroke
/// interpolates its own depth, so the cube covers the middle of the stroke —
/// which is what lets a separate source draw strokes over a scene at all.
async fn a_stroke_behind_a_body_is_hidden_by_it() {
    let ctx = Ctx::headless().await;

    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = gpu.key.clone();
    let mesh = gpu.allocate_cube_mesh(&world);

    // The stroke runs along X behind the cube's front face, so the cube stands
    // in front of its middle and leaves its two ends in the open.
    let lines_entity = gpu.spawn_source(&mut world, LineSource::new());
    gpu.spawn_in(
        &mut world,
        lines_entity,
        bundle![Line::from(
            LineSegment::new([-LENGTH, 0.0, -0.5], [LENGTH, 0.0, -0.5])
                .with_color(STROKE)
                .with_half_width(HALF_WIDTH)
        )],
    );

    // Two sources draw this frame and a source only sees its own subtree, so the
    // camera goes in the shared subtree both of them read.
    let camera = camera_looking_at(EYE, glam::Vec3::ZERO, WIDTH as f32 / HEIGHT as f32);
    world
        .scope_mut(world.shared())
        .expect("the shared subtree is its own scope")
        .spawn(bundle![camera]);
    gpu.spawn(&mut world, bundle![mesh, GpuRenderPipeline::new(key)]);

    let target = gpu.bind_offscreen_target(&world, "test::line_source::occlusion");
    gpu.render(&world);
    let frame = read_frame(&ctx, &target);

    // The stroke's row: the one the cube does not cover at the frame's edge.
    let edge = (0..frame.height)
        .find(|&y| is_stroke(frame.pixel_u8(0, y)))
        .expect("the stroke should reach the left edge of the frame");
    assert!(
        (0..frame.height).any(|y| is_stroke(frame.pixel_u8(frame.width - 1, y))),
        "the stroke should reach the right edge of the frame too"
    );

    assert!(
        !is_stroke(frame.pixel_u8(frame.width / 2, edge)),
        "the cube should hide the middle of the stroke, but column {} of row {edge} is stroke",
        frame.width / 2
    );
    // The cube is still there, so the pixel the stroke would have been is the
    // cube's: something was drawn, and it was not the stroke.
    assert!(
        drawn(frame.pixel_u8(frame.width / 2, edge)),
        "the cube should be drawn where it covers the stroke"
    );
}

/// A stroke drawn at two widths is narrower at its narrow end, and narrows
/// the whole way along.
///
/// Each end of a segment carries its own half width and the width is
/// interpolated between them, so one segment drawn at one width and then at two
/// is enough to say that the second number is read at all — and that it is read
/// along the segment rather than at one end only.
async fn a_tapering_stroke_is_narrower_at_its_narrow_end() {
    let ctx = Ctx::headless().await;

    // A segment well inside the frame at both ends, so each end measures the
    // width it states rather than the edge of the target.
    let half = 3.0;
    let mut profiles = Vec::new();
    for (ends, label) in [
        ([HALF_WIDTH; 2], "one width"),
        ([3.0 * HALF_WIDTH, HALF_WIDTH / 2.0], "tapered"),
    ] {
        let mut segment = LineSegment::new([-half, 0.0, 0.0], [half, 0.0, 0.0]).with_color(INK);
        segment.half_width = ends;
        let (world, gpu, target) = segment_world(&ctx, EYE, Line::from(segment));
        gpu.render(&world);
        let frame = read_frame(&ctx, &target);

        let (first, last) = columns(&frame).expect("the stroke is drawn");
        // Three columns in from each end: the outermost columns carry the
        // antialiasing fringe, which is a fraction of a pixel narrower.
        let heights: Vec<u32> = (first + 3..=last - 3)
            .filter_map(|column| column_height(&frame, column))
            .collect();
        assert!(
            heights.len() > WIDTH as usize / 2,
            "the {label} stroke should reach across the frame, but covered {} columns",
            heights.len()
        );
        profiles.push((label, heights));
    }

    let expected = (2.0 * HALF_WIDTH) as u32;
    let (one_label, one) = &profiles[0];
    let strays = one
        .iter()
        .filter(|height| height.abs_diff(expected) > 1)
        .count();
    assert_eq!(
        strays, 0,
        "a {one_label} stroke should be {expected} rows tall along its whole length, but {} columns disagree: {one:?}",
        strays
    );

    // The wide end is the `a` one, which lies at the left of the frame.
    let (taper_label, taper) = &profiles[1];
    let (wide, narrow) = (taper[0], taper[taper.len() - 1]);
    assert!(
        wide > 3 * narrow,
        "the {taper_label} stroke should be far wider at its wide end, {wide} rows then {narrow}"
    );
    assert!(
        taper.windows(2).all(|pair| pair[0] >= pair[1]),
        "the {taper_label} stroke should narrow along its whole length, but it reads {taper:?}"
    );
}

/// A stroke that tapers to a point behind the eye is cut at the near plane and
/// still drawn where it shows.
///
/// An infinite projection has no screen position for a point level with the eye,
/// so the near plane cut is what keeps a segment reaching past the camera from
/// sweeping across the target. The fragment then has to read the width of the
/// point it covers: the quad starts at the cut, where the taper has already
/// widened past the point the caller stated there, and a fragment that reads
/// the caller's numbers by a station measured on the shortened quad reads the
/// pointed end and draws nothing at all.
async fn a_stroke_the_near_plane_cuts_keeps_its_width() {
    let ctx = Ctx::headless().await;

    // The segment lies along the view axis and passes through the eye: the `a`
    // end is behind the camera and tapers to a point, the cut lands at the
    // frame's centre, and the `b` end is four units in front of the camera with
    // the full width.
    let mut segment = LineSegment::new([0.0, 0.0, 8.0], [0.0, 0.0, -4.0]).with_color(INK);
    segment.half_width = [0.0, HALF_WIDTH];
    let (world, gpu, target) = segment_world(&ctx, EYE, Line::from(segment));
    gpu.render(&world);
    let frame = read_frame(&ctx, &target);

    // A segment drawn along the view axis adds no height of its own, so the
    // rows it covers are the widening its width adds around that line.
    let rows: Vec<u32> = (0..frame.height)
        .filter(|&y| (0..frame.width).any(|x| drawn(frame.pixel_u8(x, y))))
        .collect();
    let height = rows
        .last()
        .zip(rows.first())
        .map_or(0, |(last, first)| last - first + 1);
    let expected = 2.0 * HALF_WIDTH;
    assert!(
        height.abs_diff(expected as u32) <= 1,
        "the cut stroke should show about {expected} rows across, but covered {height}"
    );
}

/// An opaque stroke cut at half coverage leaves no fringe: a pixel is either
/// stroke or background.
///
/// The cut is what lets an opaque stroke sit on a target that does not blend,
/// and it is also a promise about the pixels it does draw — none of them is a
/// fraction of the stroke's colour, because the fragment either discards or
/// writes its colour whole. A blended stroke of the same shape, drawn beside
/// it, keeps its fringe: the same geometry resolves the two paths differently.
async fn an_opaque_stroke_cuts_sharp_at_half_coverage() {
    let ctx = Ctx::headless().await;

    // A tapering stroke: its edge runs diagonally, so its edge pixels sit at
    // every coverage in between, where a pixel-aligned band would land its
    // edge on whole pixels and say nothing about the cut.
    let mut shape = LineSegment::new([-LENGTH, 0.0, 0.0], [LENGTH, 0.0, 0.0]).with_color(INK);
    shape.half_width = [HALF_WIDTH, HALF_WIDTH / 2.0];
    let stroke = Line::opaque(shape);
    let blended = Line::blended(stroke.segment);
    for (stroke, label, sharp) in [(stroke, "opaque", true), (blended, "blended", false)] {
        let (world, gpu, target) = segment_world(&ctx, EYE, stroke);
        gpu.render(&world);
        let frame = read_frame(&ctx, &target);

        // Every pixel the stroke reaches is its own colour or a part-way
        // shade of the background; the last kind is what an antialiased edge
        // leaves behind, and the cut exists to leave none of it.
        let mut fringe = 0usize;
        for y in 0..frame.height {
            for x in 0..frame.width {
                let pixel = frame.pixel_u8(x, y);
                if drawn(pixel) && pixel != INK {
                    fringe += 1;
                }
            }
        }
        if sharp {
            assert_eq!(
                fringe, 0,
                "the {label} stroke should own every pixel it covers"
            );
        } else {
            assert!(
                fringe > 0,
                "the {label} stroke should shade its edge pixels part-way"
            );
        }
    }
}

/// A blended stroke passing behind an opaque one is hidden where the opaque
/// one stands.
///
/// The two strokes cross in one frame: the opaque one is drawn first, writes
/// its depth, and the blended one — which tests depth and writes none — loses
/// the crossing to it. That is the hiding a stroke order alone cannot give,
/// and the reason a source draws its opaque strokes with depth writes on.
async fn a_blended_stroke_passing_behind_an_opaque_one_is_hidden_by_it() {
    let ctx = Ctx::headless().await;

    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let source_entity = gpu.spawn_source(&mut world, LineSource::new());

    // The opaque stroke stands along X through the origin; the blended one
    // runs down Y one unit behind it, so the two cross at the frame's centre
    // — where the opaque stroke's depth is what decides — and the blended one
    // still shows everywhere the opaque one does not reach.
    gpu.spawn_in(
        &mut world,
        source_entity,
        bundle![Line::opaque(
            LineSegment::new([-LENGTH, 0.0, 0.0], [LENGTH, 0.0, 0.0])
                .with_color(INK)
                .with_half_width(HALF_WIDTH)
        )],
    );
    gpu.spawn_in(
        &mut world,
        source_entity,
        bundle![Line::blended(
            LineSegment::new([0.0, -LENGTH, -1.0], [0.0, LENGTH, -1.0])
                .with_color(STROKE)
                .with_half_width(HALF_WIDTH)
        )],
    );
    let camera = camera_looking_at(EYE, glam::Vec3::ZERO, WIDTH as f32 / HEIGHT as f32);
    gpu.spawn_in(&mut world, source_entity, bundle![camera]);

    let target = gpu.bind_offscreen_target(&world, "test::line_source::opaque_hides");
    gpu.render(&world);
    let frame = read_frame(&ctx, &target);

    // The crossing sits at the frame's centre, where the blended stroke
    // passed behind the opaque one: the pixel there is the opaque stroke's
    // colour, not the blended stroke's red.
    let centre = frame.pixel_u8(frame.width / 2, frame.height / 2);
    assert!(
        !is_stroke(centre),
        "the blended stroke should lose the crossing to the opaque one, but the centre pixel is {centre:?}"
    );
    assert_eq!(
        centre, INK,
        "the crossing should hold the opaque stroke's own colour"
    );
    // The blended stroke still shows away from the crossing: it was hidden
    // where the opaque stroke stands, not everywhere.
    let near_edge = frame.pixel_u8(frame.width / 2, 2);
    assert!(
        is_stroke(near_edge),
        "the blended stroke should still show past the opaque one's reach, but the pixel near the top edge is {near_edge:?}"
    );
}

gpu_tests! {
    a_stroke_keeps_its_pixel_width_at_any_distance,
    a_wider_stroke_covers_more_pixels,
    a_round_cap_reaches_past_its_station_and_a_flat_one_does_not,
    a_tapering_stroke_is_narrower_at_its_narrow_end,
    a_stroke_the_near_plane_cuts_keeps_its_width,
    a_stroke_behind_a_body_is_hidden_by_it,
    an_opaque_stroke_cuts_sharp_at_half_coverage,
    a_blended_stroke_passing_behind_an_opaque_one_is_hidden_by_it,
}
