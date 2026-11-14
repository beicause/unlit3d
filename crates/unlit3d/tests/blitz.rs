//! End-to-end GPU tests for [`BlitzSource`], a frame source that lays out an
//! HTML document and paints it over the frame.
//!
//! What these pin down is the composite and the input path rather than the
//! layout engine itself: the page is rasterized on the CPU into premultiplied
//! sRGB-encoded texels, so a frame is only right if those bytes reach an sRGB
//! target through exactly one decode, and a page is only live if what the frame
//! feeds it reaches the document.

use std::sync::{Arc, Mutex};

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

/// A sixteen-pixel red square as a PNG, which is what the image tests load.
const RED_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 16, 0, 0, 0, 16, 8, 2,
    0, 0, 0, 144, 145, 104, 54, 0, 0, 0, 22, 73, 68, 65, 84, 120, 218, 99, 248, 207, 192, 64, 18,
    98, 24, 213, 48, 170, 97, 248, 106, 0, 0, 144, 249, 255, 1, 242, 238, 232, 87, 0, 0, 0, 0, 73,
    69, 78, 68, 174, 66, 96, 130,
];

/// The same PNG as a `data:` URL body.
const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mP4z8BAEmIY1TCqYfhqAACQ+f8B8u7oVwAAAABJRU5ErkJggg==";

/// One DOM event a test page dispatched, as the callback saw it.
#[derive(Clone, Debug)]
struct Seen {
    /// The event's DOM name.
    name: String,
    /// The value an `input` event carried, empty for anything else.
    value: String,
    /// Whether `#box` was checked when the event arrived, when the page has one.
    checked: Option<bool>,
}

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

/// Render `html` alone into a fresh target.
///
/// The returned page entity is the one the source draws, which is how a test
/// changes the markup or asks what the document holds.
fn render_page(ctx: &Ctx, html: &str) -> (World, TestGpu, wgpu::Texture, Entity, Entity) {
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, ctx);
    let source = gpu.spawn_source(&mut world, BlitzSource::new());
    // A page declares no depth state, so its target carries no depth
    // attachment: wgpu requires the pass and the pipeline to agree.
    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    let page = gpu.spawn_in(&mut world, source, bundle![BlitzDocument::new(html)]);
    (world, gpu, target, source, page)
}

/// A page, the frame it draws into and everything needed to feed and watch it.
type WatchedPage = (
    World,
    TestGpu,
    wgpu::Texture,
    Entity,
    Arc<Mutex<Vec<Seen>>>,
    InputWriter,
    InputEvents,
);

/// Spawn a page and a callback that records every event it dispatches.
fn render_watched_page(ctx: &Ctx, html: &str) -> WatchedPage {
    let (mut world, gpu, target, source, _page) = render_page(ctx, html);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    gpu.spawn_in(
        &mut world,
        source,
        bundle![OnBlitzEvent::new(move |_scope, _entity, event| {
            let value = match event.data() {
                blitz_traits::events::DomEventData::Input(input) => input.value.clone(),
                _ => String::new(),
            };
            let checked = event
                .document()
                .get_element_by_id("box")
                .and_then(|id| event.document().get_node(id))
                .and_then(|node| node.element_data())
                .and_then(|element| element.checkbox_input_checked());
            log.lock().expect("the log is not poisoned").push(Seen {
                name: event.name().to_owned(),
                value,
                checked,
            });
        })],
    );
    let input = spawn_input(&mut world);
    (world, gpu, target, source, seen, input.0, input.1)
}

/// Spawn an input buffer at the target's size and a density of one.
fn spawn_input(world: &mut World) -> (InputWriter, InputEvents) {
    let input = InputWriter::new();
    let events = input.subscribe();
    // Input is shared by every view, so it belongs to the shared subtree: a
    // source reads it from its own scope, which cannot see a sibling's.
    world
        .scope_mut(world.shared())
        .expect("the shared scope exists")
        .spawn(bundle![events.clone()]);
    input.push(InputEvent::Resized {
        size_px: (WIDTH, HEIGHT),
    });
    input.push(InputEvent::ScaleFactorChanged { scale_factor: 1.0 });
    events.refresh();
    (input, events)
}

/// Feed `batch` as the next frame's input.
fn feed(input: &InputWriter, events: &InputEvents, batch: impl IntoIterator<Item = InputEvent>) {
    input.push_all(batch);
    events.refresh();
}

/// A press and a release at `position`, which is the pair a document reads as a
/// click.
fn click(position: [f32; 2]) -> [InputEvent; 2] {
    [
        InputEvent::Mouse(MouseEvent::Button {
            position,
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        }),
        InputEvent::Mouse(MouseEvent::Button {
            position,
            button: MouseButton::Primary,
            pressed: false,
            modifiers: Modifiers::default(),
        }),
    ]
}

/// A page is rasterized in sRGB and composited into an sRGB target, so its own
/// colours survive: the two conversions cancel instead of compounding.
async fn a_page_paints_its_own_colours() {
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
async fn a_page_draws_its_content() {
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
async fn a_transparent_page_leaves_the_frame_alone() {
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
async fn new_markup_replaces_the_page() {
    let ctx = Ctx::headless().await;
    let (world, gpu, target, _source, page) = render_page(&ctx, GREY_PAGE);

    gpu.render(&world);
    let grey = centre_pixel(&ctx.device, &ctx.queue, &target).await;
    assert!(grey[0].abs_diff(GREY) <= 2, "the grey page should paint");

    // A page that changed nothing above it is not repainted, so only the new
    // markup can turn the centre red.
    world
        .root_scope()
        .query_one::<&mut BlitzDocument>(page)
        .map(|mut row| row.get().set_markup(RED_PAGE))
        .expect("the page entity exists");
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
async fn a_page_draws_an_image_from_a_registered_path() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <img src="solid.png" style="display: block; width: 64px; height: 64px">
    </body></html>"#;
    let (world, gpu, target, _source, page) = render_page(&ctx, html);
    world
        .root_scope()
        .query_one::<&BlitzDocument>(page)
        .map(|mut row| row.get().insert_resource("solid.png", RED_PNG))
        .expect("the page entity exists");

    gpu.render_frames(&world, 2);

    let inside = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        inside[0].abs_diff(255) <= 2 && inside[1] <= 2 && inside[2] <= 2,
        "the served image should paint its own colours, got {inside:?}"
    );
}

/// A page draws an inline `data:` image without anything serving it.
async fn a_page_draws_an_image_from_a_data_url() {
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

/// A click reaches the document, toggles the control it lands on, and is
/// reported to the callbacks that watch the page.
async fn a_click_toggles_a_checkbox() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="checkbox" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 24px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input, events) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&input, &events, click([20.0, 20.0]));
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter().any(|event| event.name == "click"),
        "the click should reach the page's callbacks, got {seen:?}"
    );
    let toggled = seen
        .iter()
        .rev()
        .find(|event| event.name == "input")
        .expect("toggling the control dispatches an input event");
    assert_eq!(
        toggled.checked,
        Some(true),
        "the control should be checked when its input event is dispatched"
    );
}

/// Text a key produced reaches the focused field, which is what makes the page
/// an editable document rather than a picture of one.
async fn typing_reaches_a_text_field() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="text" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 200px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input, events) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&input, &events, click([40.0, 20.0]));
    gpu.render(&world);
    feed(
        &input,
        &events,
        [InputEvent::Text(TextEvent("hi".to_owned()))],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter()
            .any(|event| event.name == "input" && event.value == "hi"),
        "the typed text should reach the field, got {seen:?}"
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
    a_page_draws_an_image_from_a_registered_path,
    a_page_draws_an_image_from_a_data_url,
    a_click_toggles_a_checkbox,
    typing_reaches_a_text_field,
}
