//! End-to-end GPU tests for fitting a scene to a render target whose shape
//! differs from the scene's own.
//!
//! A scene declares the region of the target it draws into — see
//! [`Scene::set_viewport`], which a frame loop states through `FrameViewport` —
//! and a frame loop that keeps content's aspect uses it to letterbox. These
//! tests draw one camera into targets of different shapes and check where the
//! content lands, which is the property no unit test of the arithmetic can
//! show: that the region actually reaches the rasterizer.
//!
//! The frame is cleared to black by the renderer's default load ops, which is
//! what "no content here" looks like in these frames; the cube is drawn in its
//! own vertex colours, so any pixel that is not black is content.

pub mod common;

use common::*;
use unlit_wgpu_test_util::{gpu_test_main, gpu_tests};
use unlit3d::prelude::*;

/// The baseline every target here is shaped around: 4:3, the shape the
/// example's scenes were captured at.
const BASELINE: (u32, u32) = (256, 192);

/// A target wider than the baseline, so bars belong at the left and right.
const WIDE: (u32, u32) = (384, 192);

/// A target taller than the baseline, so bars belong above and below.
const TALL: (u32, u32) = (256, 384);

/// Whether `pixel` holds anything the clear did not put there.
fn is_content(pixel: &[u8]) -> bool {
    // The cube's darkest face still stands well clear of black, while a
    // letterbox bar is exactly the clear colour.
    pixel[..3].iter().any(|&channel| channel > 16)
}

/// The largest region of `baseline`'s aspect that fits a `target`-pixel frame.
///
/// The same fitting the example's frame loop does through
/// [`ViewportRect::fit_aspect`], restated here so the test does not reach into
/// another crate for it.
fn baseline_viewport(baseline: (u32, u32), target: (u32, u32)) -> ViewportRect {
    ViewportRect::fit_aspect(baseline.0 as f32 / baseline.1 as f32, target.0, target.1)
}

/// The size of that region, which is what a camera must be built for.
fn content_size(baseline: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    let viewport = baseline_viewport(baseline, target);
    (viewport.width as u32, viewport.height as u32)
}

/// The columns of a frame that hold content, as fractions of its width.
fn content_columns(frame: &Frame) -> Option<(f64, f64)> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let mut first = None;
    let mut last = None;
    for x in 0..width {
        let column = (0..height).any(|y| is_content(&frame.rgba[(y * width + x) * 4..][..4]));
        if column {
            first.get_or_insert(x);
            last = Some(x);
        }
    }
    first.zip(last).map(|(first, last)| {
        (
            first as f64 / width as f64,
            (last + 1) as f64 / width as f64,
        )
    })
}

/// The rows of a frame that hold content, as fractions of its height.
fn content_rows(frame: &Frame) -> Option<(f64, f64)> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let mut first = None;
    let mut last = None;
    for y in 0..height {
        let row = (0..width).any(|x| is_content(&frame.rgba[(y * width + x) * 4..][..4]));
        if row {
            first.get_or_insert(y);
            last = Some(y);
        }
    }
    first.zip(last).map(|(first, last)| {
        (
            first as f64 / height as f64,
            (last + 1) as f64 / height as f64,
        )
    })
}

/// A world drawing one unit cube from a camera built for `size`'s aspect, with
/// the frame's content region stated as `viewport`.
///
/// `size` is the region the content is drawn into — for a letterboxed frame,
/// the fitted region rather than the target — so the projection matches what
/// the viewport scales into place.
fn cube_world(ctx: &Ctx, size: (u32, u32), viewport: Option<ViewportRect>) -> (World, TestGpu) {
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, ctx);
    let key = gpu.key.clone();

    let mesh = gpu.allocate_cube_mesh(&world);
    world.spawn((camera_view(size.0 as f32 / size.1.max(1) as f32),));
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        mesh,
        UnlitPipeline::new(key),
    ));

    set_frame_viewport(&world, viewport.map(FrameViewport));
    (world, gpu)
}

/// Content lands where the baseline's own shape puts it, and the bars are the
/// only thing the target's extra size adds.
///
/// The cube does not fill its own view, so its edges say nothing on their own;
/// what the letterbox promises is that the content sits at the same place
/// *within its region* whatever the target's shape, and that everything outside
/// that region is untouched clear colour.
async fn a_letterboxed_frame_keeps_the_baseline_picture_and_bars_the_rest() {
    let ctx = Ctx::headless().await;

    // The reference: the baseline drawn into a target of its own shape, so its
    // content region is the whole frame and nothing is letterboxed.
    let (world, gpu) = cube_world(&ctx, BASELINE, None);
    let reference =
        gpu.render_to_offscreen_sized(&ctx, &world, BASELINE, "test::letterbox::reference");
    let (ref_left, ref_right) = content_columns(&reference).expect("the cube is visible");
    let (ref_top, ref_bottom) = content_rows(&reference).expect("the cube is visible");

    for (target, label) in [
        (WIDE, "test::letterbox::wide"),
        (TALL, "test::letterbox::tall"),
    ] {
        let viewport = baseline_viewport(BASELINE, target);
        let content = content_size(BASELINE, target);
        let (world, gpu) = cube_world(&ctx, content, Some(viewport));
        let frame = gpu.render_to_offscreen_sized(&ctx, &world, target, label);

        // Map the baseline's content box into this target through the region
        // the letterbox fitted, which is where the picture should have gone.
        let region_left = viewport.x as f64 / target.0 as f64;
        let region_top = viewport.y as f64 / target.1 as f64;
        let region_width = viewport.width as f64 / target.0 as f64;
        let region_height = viewport.height as f64 / target.1 as f64;
        let expected = [
            region_left + ref_left * region_width,
            region_left + ref_right * region_width,
            region_top + ref_top * region_height,
            region_top + ref_bottom * region_height,
        ];

        let (left, right) = content_columns(&frame).expect("the cube is visible");
        let (top, bottom) = content_rows(&frame).expect("the cube is visible");
        let actual = [left, right, top, bottom];
        for (edge, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() < 0.02,
                "{label}: edge {edge} should be at {expected:.3}, got {actual:.3}"
            );
        }

        // Everything the region does not cover is the clear colour, which is
        // what makes this a letterbox rather than a stretch.
        assert!(
            is_bare_outside(&frame, viewport),
            "{label}: the bars around {viewport:?} should hold no content"
        );
    }
}

/// Whether every pixel outside `viewport` is clear colour.
fn is_bare_outside(frame: &Frame, viewport: ViewportRect) -> bool {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let inside = |x: usize, y: usize| {
        let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
        x >= viewport.x
            && x < viewport.x + viewport.width
            && y >= viewport.y
            && y < viewport.y + viewport.height
    };
    (0..height).all(|y| {
        (0..width).all(|x| inside(x, y) || !is_content(&frame.rgba[(y * width + x) * 4..][..4]))
    })
}

/// The picture inside the letterbox does not depend on the target's shape: the
/// same baseline content in a wide and a tall target lands in the same place
/// relative to its own region.
///
/// This is the property the letterbox exists for — a wide screen and a narrow
/// one showing the same view rather than one showing more.
async fn the_same_baseline_content_fills_its_region_on_any_target_shape() {
    let ctx = Ctx::headless().await;

    let mut relative = Vec::new();
    for (target, label) in [
        (WIDE, "test::letterbox::shape-wide"),
        (TALL, "test::letterbox::shape-tall"),
    ] {
        // Each target gets a camera for its own fitted region, so the
        // projection matches what the viewport scales into place — which is
        // what makes the two comparable at all.
        let viewport = baseline_viewport(BASELINE, target);
        let content = content_size(BASELINE, target);
        let (world, gpu) = cube_world(&ctx, content, Some(viewport));
        let frame = gpu.render_to_offscreen_sized(&ctx, &world, target, label);

        let (left, right) = content_columns(&frame).expect("the cube is visible");
        let (top, bottom) = content_rows(&frame).expect("the cube is visible");
        // Where the content sits as a fraction of the fitted region, with the
        // frame's own coordinates scaled back by the letterbox.
        let region_left = viewport.x as f64 / target.0 as f64;
        let region_top = viewport.y as f64 / target.1 as f64;
        let region_width = viewport.width as f64 / target.0 as f64;
        let region_height = viewport.height as f64 / target.1 as f64;
        relative.push([
            (left - region_left) / region_width,
            (right - region_left) / region_width,
            (top - region_top) / region_height,
            (bottom - region_top) / region_height,
        ]);
    }

    let [wide, tall] = [relative[0], relative[1]];
    for (edge, (a, b)) in wide.into_iter().zip(tall).enumerate() {
        assert!(
            (a - b).abs() < 0.03,
            "edge {edge} differs between the two target shapes: {a:.3} versus {b:.3}"
        );
    }

    // Each target really does have a bar, which is what makes the agreement
    // above mean something.
    for target in [WIDE, TALL] {
        let viewport = baseline_viewport(BASELINE, target);
        assert!(
            viewport.width < target.0 as f32 || viewport.height < target.1 as f32,
            "a target of {target:?} should have a bar somewhere, got {viewport:?}"
        );
    }
}

// The registry both runners drive: `cargo nextest` natively, and a
// browser through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    a_letterboxed_frame_keeps_the_baseline_picture_and_bars_the_rest,
    the_same_baseline_content_fills_its_region_on_any_target_shape,
}

gpu_test_main!(all_tests());
