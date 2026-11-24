//! Drawing the page itself: what its markup paints, the images and stylesheets
//! it reaches for, and what replaces a page that is unloaded or re-marked.

use super::{
    BlitzDocument, Ctx, GREY, GREY_PAGE, RED_PAGE, RED_PNG, RED_PNG_BASE64, TestGpu, World,
    centre_pixel, pixel, render_page,
};

/// A page is rasterized in sRGB and composited into an sRGB target, so its own
/// colours survive: the two conversions cancel instead of compounding.
pub(super) async fn a_page_paints_its_own_colours() {
    let ctx = Ctx::headless().await;
    let (world, gpu, target, _source, _page) = render_page(&ctx, GREY_PAGE);

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
pub(super) async fn a_page_draws_its_content() {
    let ctx = Ctx::headless().await;
    const RED: [u8; 4] = [255, 0, 0, 255];
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div style="width: 64px; height: 64px; background: #ff0000"></div>
    </body></html>"#;
    let (world, gpu, target, _source, _page) = render_page(&ctx, html);

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
pub(super) async fn a_transparent_page_leaves_the_frame_alone() {
    let ctx = Ctx::headless().await;
    let mut empty = World::new();
    let empty_gpu = TestGpu::frame_only(&mut empty, &ctx);
    let empty_target = empty_gpu.bind_offscreen_target_with(&empty, 1, false);
    empty_gpu.render(&empty);
    let without_page =
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &empty_target).await;

    let html = r##"<html><body style="margin: 0"></body></html>"##;
    let (world, gpu, target, _source, _page) = render_page(&ctx, html);
    gpu.render(&world);
    let with_page = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    assert_eq!(
        with_page, without_page,
        "a transparent page should composite nothing onto the frame"
    );
}

/// Replacing the markup rebuilds and repaints the document, which is what makes
/// the page a live document rather than a fixed picture.
pub(super) async fn new_markup_replaces_the_page() {
    let ctx = Ctx::headless().await;
    let (world, gpu, target, _source, page) = render_page(&ctx, GREY_PAGE);

    gpu.render(&world);
    let grey = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(grey[0].abs_diff(GREY) <= 2, "the grey page should paint");

    // A page that changed nothing above it is not repainted, so only the new
    // markup can turn the centre red.
    {
        let mut row = world.root_scope().query_one::<&mut BlitzDocument>(page);
        row.get().set_markup(RED_PAGE);
    }
    gpu.render(&world);

    let red = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(
        red[0].abs_diff(255) <= 2 && red[1] <= 2 && red[2] <= 2,
        "the replacement page should paint, got {red:?}"
    );
}

/// A page draws an image it references by path from the bytes the document
/// serves for it: nothing reaches the network, and the host's assets are not
/// looked up.
pub(super) async fn a_page_draws_an_image_from_a_registered_path() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <img src="solid.png" style="display: block; width: 64px; height: 64px">
    </body></html>"#;
    let (world, gpu, target, _source, page) = render_page(&ctx, html);
    {
        let mut row = world.root_scope().query_one::<&BlitzDocument>(page);
        row.get().insert_resource("solid.png", RED_PNG);
    }

    gpu.render_frames(&world, 2);

    let inside = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        inside[0].abs_diff(255) <= 2 && inside[1] <= 2 && inside[2] <= 2,
        "the served image should paint its own colours, got {inside:?}"
    );
}

/// A page draws an inline `data:` image without anything serving it.
pub(super) async fn a_page_draws_an_image_from_a_data_url() {
    let ctx = Ctx::headless().await;
    let html = format!(
        r#"<html style="background: #808080"><body style="margin: 0">
        <img src="data:image/png;base64,{RED_PNG_BASE64}"
             style="display: block; width: 64px; height: 64px">
    </body></html>"#
    );
    let (world, gpu, target, _source, _page) = render_page(&ctx, &html);

    gpu.render_frames(&world, 2);

    let inside = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        inside[0].abs_diff(255) <= 2 && inside[1] <= 2 && inside[2] <= 2,
        "the inline image should paint its own colours, got {inside:?}"
    );
}

/// A page blocks on a stylesheet until the answer to it arrives, so a request
/// nothing serves has to be answered too: the page then loads it as an empty
/// sheet and paints, instead of waiting for a reply that never comes.
pub(super) async fn a_page_loads_a_stylesheet_nobody_serves() {
    let ctx = Ctx::headless().await;
    let html = r#"<html><head><link rel="stylesheet" href="missing.css"></head>
        <body style="margin: 0; background: #808080"></body></html>"#;
    let (world, gpu, target, _source, _page) = render_page(&ctx, html);

    gpu.render_frames(&world, 2);

    let centre = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(
        centre[0].abs_diff(GREY) <= 2,
        "a page whose stylesheet nobody serves should still paint, got {centre:?}"
    );
}

/// A page that asks for a stylesheet the host serves loads it: the sheet is one
/// of the bytes the document answers with, not something it reaches for.
pub(super) async fn a_page_loads_a_stylesheet_it_serves() {
    let ctx = Ctx::headless().await;
    let html = r#"<html><head><link rel="stylesheet" href="page.css"></head>
        <body style="margin: 0"></body></html>"#;
    let (world, gpu, target, _source, page) = render_page(&ctx, html);
    {
        let mut row = world.root_scope().query_one::<&BlitzDocument>(page);
        row.get()
            .insert_resource("page.css", "html { background: #808080 }");
    }

    gpu.render_frames(&world, 2);

    let centre = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(
        centre[0].abs_diff(GREY) <= 2 && centre[1].abs_diff(GREY) <= 2,
        "the served stylesheet should paint the page grey, got {centre:?}"
    );
}

/// A path is served at the address the base gives it, so changing the base
/// serves the same bytes where the new markup looks for them.
pub(super) async fn a_new_base_still_serves_the_registered_bytes() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <img src="solid.png" style="display: block; width: 64px; height: 64px">
    </body></html>"#;
    let (world, gpu, target, _source, page) = render_page(&ctx, html);
    {
        let mut row = world.root_scope().query_one::<&mut BlitzDocument>(page);
        let page = row.get();
        page.insert_resource("solid.png", RED_PNG);
        page.set_base_url("https://example.test/images/")
            .expect("the base is an absolute URL");
    }

    gpu.render_frames(&world, 2);

    let inside = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        inside[0].abs_diff(255) <= 2 && inside[1] <= 2 && inside[2] <= 2,
        "the image should load at its address under the new base, got {inside:?}"
    );
}

/// A page that replaced its markup does not keep showing the one it replaced:
/// the older document does not paint the newer page.
pub(super) async fn a_replaced_page_is_not_drawn_from_the_old_pixels() {
    let ctx = Ctx::headless().await;
    let mut empty = World::new();
    let empty_gpu = TestGpu::frame_only(&mut empty, &ctx);
    let empty_target = empty_gpu.bind_offscreen_target_with(&empty, 1, false);
    empty_gpu.render(&empty);
    let without_page =
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &empty_target).await;

    let (world, gpu, target, _source, page) = render_page(&ctx, GREY_PAGE);
    gpu.render_frames(&world, 2);
    {
        let mut row = world.root_scope().query_one::<&mut BlitzDocument>(page);
        row.get()
            .set_markup(r#"<html><head><link rel="stylesheet" href="later.css"></head></html>"#);
    }

    gpu.render_frames(&world, 2);

    let with_page = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;
    assert_eq!(
        with_page, without_page,
        "a page that has replaced its markup should leave nothing of the old one"
    );
}
