//! The tests that judge the frame as a whole: clear color, depth order and coverage.

use super::fixture::*;
use super::*;

pub(super) async fn renders_a_cube_over_the_clear_color() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx), 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0, 0.85, 0.4, 1.0])]);

    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);

    // The clear color survives in a corner: nothing was drawn there. The
    // target stores sRGB-encoded values, so decode back to linear before
    // comparing against the linear clear color.
    let corner = frame.pixel_u8(0, 0);
    for (channel, want) in corner[..3].iter().zip(CLEAR) {
        let got = srgb_to_linear_u8(*channel) as f64;
        assert!(
            (got - want).abs() < 0.01,
            "corner {corner:?} decodes to {got}, expected the clear color {want}"
        );
    }

    // The cube covers a meaningful part of the frame.
    let covered = count_pixels_off_background(&frame, CLEAR, 12);
    let total = (WIDTH * HEIGHT) as usize;
    assert!(
        covered > total / 8,
        "the cube should cover a meaningful area, got {covered}/{total} pixels"
    );
}

pub(super) async fn base_color_reaches_the_frame() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx), 4);

    let brightest = |base_color: [f32; 4]| {
        let frame = render(&ctx, &fixture, &[placed_cube(base_color)]);
        frame
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0] as u16 + p[1] as u16 + p[2] as u16)
            .max()
            .expect("a pixel")
    };

    let bright = brightest([1.0, 1.0, 1.0, 1.0]);
    let dim = brightest([0.1, 0.1, 0.1, 1.0]);
    assert!(
        bright > dim + 60,
        "a white base color should be much brighter than a dark one: {bright} vs {dim}"
    );
}

pub(super) async fn depth_ordering_hides_the_far_instance() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx), 4);

    // A far red cube and a near green one, drawn far-first so a missing depth
    // test would let the far cube show through.
    let far = placed(
        1.0,
        0.0,
        glam::Vec3::new(0.0, 0.2, -0.6),
        [1.0, 0.0, 0.0, 1.0],
    );
    let near = placed(
        1.0,
        0.0,
        glam::Vec3::new(0.0, 0.2, 0.6),
        [0.0, 1.0, 0.0, 1.0],
    );
    let frame = render(&ctx, &fixture, &[far, near]);

    let centre = frame.pixel_u8(WIDTH / 2, HEIGHT / 2);
    assert!(
        centre[1] > centre[0],
        "the near green cube should win the depth test, got {centre:?}"
    );
}

pub(super) async fn msaa_produces_more_partial_coverage_than_no_msaa() {
    let ctx = Ctx::headless().await;
    let instance = placed(0.9, 0.6, glam::Vec3::ZERO, [1.0, 1.0, 1.0, 1.0]);

    // Pixels that are neither fully clear nor fully covered: the
    // antialiased silhouette, which a single-sample render cannot produce.
    let partial = |sample_count: u32| {
        let fixture = fixture(&ctx, &vertex_color_variant(&ctx), sample_count);
        let frame = render(&ctx, &fixture, std::slice::from_ref(&instance));
        frame
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| {
                let luma = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                (30.0..200.0).contains(&luma)
            })
            .count()
    };

    let without = partial(1);
    let with = partial(4);
    assert!(
        with > without,
        "MSAA should produce more partial-coverage pixels: {with} vs {without}"
    );
}
