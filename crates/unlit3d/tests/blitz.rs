//! End-to-end GPU tests for [`BlitzSource`], a frame source that lays out an
//! HTML document and paints it over the frame.
//!
//! What these pin down is the composite rather than the layout engine: the page
//! is rasterized on the CPU into premultiplied sRGB-encoded texels, so a frame is
//! only right if those bytes reach an sRGB target through exactly one decode. An
//! opaque page therefore shows its own colours, and a transparent one leaves the
//! frame exactly as it was.

use crate::common::*;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::prelude::*;

/// The grey the test pages paint their background with.
const GREY: u8 = 128;

/// A page whose whole canvas is grey.
const GREY_PAGE: &str =
    r#"<html style="background: #808080"><body style="margin: 0"></body></html>"#;

/// A page whose whole canvas is red.
const RED_PAGE: &str =
    r#"<html style="background: #ff0000"><body style="margin: 0"></body></html>"#;

/// The pixel at `x`, `y` of the rendered target, as RGBA.
async fn pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    x: u32,
    y: u32,
) -> [u8; 4] {
    let rgba = unlit_wgpu::readback::readback_texture(device, queue, target).await;
    let offset = ((y as usize) * WIDTH as usize + x as usize) * 4;
    rgba[offset..offset + 4].try_into().expect("four channels")
}

/// The RGBA of the target's centre pixel.
async fn centre_pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
) -> [u8; 4] {
    pixel(device, queue, target, WIDTH / 2, HEIGHT / 2).await
}

/// Render `html` alone into a fresh target and return the frame's bytes.
fn render_page(ctx: &Ctx, html: &str) -> (World, TestGpu, wgpu::Texture, Entity) {
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, ctx);
    let source = gpu.spawn_source(&mut world, BlitzSource::new(html));
    // A page declares no depth state, so its target carries no depth
    // attachment: wgpu requires the pass and the pipeline to agree.
    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    (world, gpu, target, source)
}

/// A page is rasterized in sRGB and composited into an sRGB target, so its own
/// colours survive: the two conversions cancel instead of compounding.
async fn a_page_paints_its_own_colours() {
    let ctx = Ctx::headless().await;
    let (world, gpu, target, _source) = render_page(&ctx, GREY_PAGE);

    gpu.render(&world);

    let centre = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    for (channel, &value) in centre[..3].iter().enumerate() {
        assert!(
            value.abs_diff(GREY) <= 2,
            "channel {channel} should be the page's grey {GREY}, not {value}"
        );
    }
    assert_eq!(centre[3], 255, "an opaque page covers the target");
}

/// The page's boxes land where its layout puts them, so the frame shows the
/// document rather than one background colour.
async fn a_page_draws_its_content() {
    let ctx = Ctx::headless().await;
    const RED: [u8; 4] = [255, 0, 0, 255];
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div style="width: 64px; height: 64px; background: #ff0000"></div>
    </body></html>"#;
    let (world, gpu, target, _source) = render_page(&ctx, html);

    gpu.render(&world);

    let inside = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        inside[0].abs_diff(RED[0]) <= 2 && inside[1] <= 2 && inside[2] <= 2,
        "the red box should be at the page's top left corner, got {inside:?}"
    );
    let outside = pixel(&ctx.device, &ctx.queue, &target, 200, 150).await;
    for &value in &outside[..3] {
        assert!(
            value.abs_diff(GREY) <= 2,
            "the page's background should surround the box, got {outside:?}"
        );
    }
}

/// A page whose canvas is transparent leaves the frame exactly as it was: the
/// composite is premultiplied source-over rather than a copy.
///
/// The empty frame is rendered by a world of its own, so the comparison covers
/// every pixel rather than the centre one.
async fn a_transparent_page_leaves_the_frame_alone() {
    let ctx = Ctx::headless().await;
    let mut empty = World::new();
    let empty_gpu = TestGpu::frame_only(&mut empty, &ctx);
    let empty_target = empty_gpu.bind_offscreen_target_with(&empty, 1, false);
    empty_gpu.render(&empty);
    let without_page =
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &empty_target).await;

    let html = r##"<html><body style="margin: 0"></body></html>"##;
    let (world, gpu, target, _source) = render_page(&ctx, html);
    gpu.render(&world);
    let with_page = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    assert_eq!(
        with_page, without_page,
        "a transparent page should composite nothing onto the frame"
    );
}

/// Replacing the markup rebuilds and repaints the document, which is what makes
/// the source a live page rather than a fixed picture.
async fn new_markup_replaces_the_page() {
    let ctx = Ctx::headless().await;
    let (world, gpu, target, source) = render_page(&ctx, GREY_PAGE);

    gpu.render(&world);
    let grey = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(grey[0].abs_diff(GREY) <= 2, "the grey page should paint");

    // A page that changed nothing above it is not repainted, so only the new
    // markup can turn the centre red.
    world
        .root_scope()
        .query_one::<&mut Source>(source)
        .map(|mut source| {
            source
                .get()
                .as_mut::<BlitzSource>()
                .expect("the source is a BlitzSource")
                .set_html(RED_PAGE);
        })
        .expect("the source entity exists");
    gpu.render(&world);

    let red = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(
        red[0].abs_diff(255) <= 2 && red[1] <= 2 && red[2] <= 2,
        "the replacement page should paint, got {red:?}"
    );
}

// This file's share of the suite's registry: the suite root concatenates
// it with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
gpu_tests! {
    a_page_paints_its_own_colours,
    a_page_draws_its_content,
    a_transparent_page_leaves_the_frame_alone,
    new_markup_replaces_the_page,
}
