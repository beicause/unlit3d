//! End-to-end GPU tests for [`BlitzSource`], a frame source that lays out an
//! HTML document and paints it over the frame.
//!
//! What these pin down is the composite and the input path rather than the
//! layout engine itself: the page is rasterized into premultiplied texels that
//! hold sRGB-encoded channels, so a frame is only right if those bytes reach the
//! target through exactly one decode, and a page is only live if what the frame
//! feeds it reaches the document.

use std::sync::{Arc, Mutex};

use crate::common::{COLOR_FORMAT, Ctx, HEIGHT, TestGpu, WIDTH};
use keyboard_types::{Code, Location};
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::prelude::*;

mod input;
mod page;
mod pointer;
mod replay;

use self::{
    input::{
        a_callback_can_cancel_the_default_action, a_click_toggles_a_checkbox,
        a_key_reaches_the_page_as_a_code_and_a_side, a_key_release_reaches_the_page,
        a_pen_click_toggles_a_checkbox, a_wheel_scrolls_the_page,
        an_ime_commit_reaches_a_text_field, input_is_dispatched_once,
        leaving_the_page_is_reported_to_it, losing_focus_leaves_the_text_field,
        one_tick_reaches_a_shared_page_once, typing_reaches_a_text_field,
    },
    page::{
        a_new_base_still_serves_the_registered_bytes, a_page_draws_an_image_from_a_data_url,
        a_page_draws_an_image_from_a_registered_path, a_page_draws_its_content,
        a_page_loads_a_stylesheet_it_serves, a_page_loads_a_stylesheet_nobody_serves,
        a_page_paints_its_own_colours, a_replaced_page_is_not_drawn_from_the_old_pixels,
        a_transparent_page_leaves_the_frame_alone, new_markup_replaces_the_page,
    },
    pointer::{
        a_finger_that_stays_down_keeps_its_button, a_mouse_release_does_not_lift_the_pen,
        a_pen_press_states_the_press_it_makes, lifting_one_finger_leaves_the_other_down,
    },
    replay::{
        a_batch_replays_from_the_state_that_stood_before_it,
        a_batch_that_skips_one_still_replays_from_the_state_left,
        a_first_frame_replays_from_an_empty_baseline,
        a_frame_without_events_keeps_the_pointer_state,
        a_move_before_a_press_reports_the_button_that_still_stood,
        a_replaced_page_replays_from_the_world_state, a_wheel_is_reported_where_the_pointer_stood,
    },
};

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
    /// The physical key a key event named, when the event was a key event.
    code: Option<Code>,
    /// The side of the keyboard that key sits on, beside `code`.
    location: Option<Location>,
    /// The buttons a pointer or wheel event named, as the DOM bitmask.
    buttons: Option<u8>,
    /// The client position a pointer or wheel event named, in CSS pixels.
    client: Option<[f32; 2]>,
    /// Whether a pointer event named its pointer as the primary one.
    primary: Option<bool>,
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
    let source = gpu.spawn_source(&mut world, FrameSourceOrder::BLITZ, BlitzSource::new());
    // A page declares no depth state, so its target carries no depth
    // attachment: wgpu requires the pass and the pipeline to agree.
    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    let page = world
        .scope_mut(source)
        .spawn(bundle![BlitzDocument::new(html)]);
    (world, gpu, target, source, page)
}

/// What a pointer or wheel event states about the pointer, for the log.
///
/// `None` for a `Seen` entry that is not one of those events: a DOM name alone
/// says which event happened, not what it claimed about the devices.
fn pointer_payload(
    data: &blitz_traits::events::DomEventData,
) -> (Option<u8>, Option<[f32; 2]>, Option<bool>) {
    use blitz_traits::events::DomEventData;
    match data {
        DomEventData::PointerMove(event)
        | DomEventData::PointerDown(event)
        | DomEventData::PointerUp(event)
        | DomEventData::PointerCancel(event)
        | DomEventData::PointerLeave(event) => (
            Some(event.buttons.bits()),
            Some([event.coords.client_x, event.coords.client_y]),
            Some(event.is_primary),
        ),
        DomEventData::Wheel(event) => (
            Some(event.buttons.bits()),
            Some([event.coords.client_x, event.coords.client_y]),
            None,
        ),
        _ => (None, None, None),
    }
}

/// A page, the frame it draws into and everything needed to feed and watch it.
type WatchedPage = (
    World,
    TestGpu,
    wgpu::Texture,
    Entity,
    Arc<Mutex<Vec<Seen>>>,
    InputWriter,
);

/// Spawn a page and a callback that records every event it dispatches.
fn render_watched_page(ctx: &Ctx, html: &str) -> WatchedPage {
    let (mut world, gpu, target, source, _page) = render_page(ctx, html);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    world
        .scope_mut(source)
        .spawn(bundle![OnBlitzEvent::new(move |_scope, _entity, event| {
            let value = match event.data() {
                blitz_traits::events::DomEventData::Input(input) => input.value.clone(),
                _ => String::new(),
            };
            let (code, location) = match event.data() {
                blitz_traits::events::DomEventData::KeyDown(key)
                | blitz_traits::events::DomEventData::KeyUp(key) => {
                    (Some(key.code), Some(key.location))
                }
                _ => (None, None),
            };
            let checked = event
                .document_mut()
                .get_element_by_id("box")
                .and_then(|id| event.document_mut().get_node(id))
                .and_then(|node| node.element_data())
                .and_then(|element| element.checkbox_input_checked());
            let (buttons, client, primary) = pointer_payload(event.data());
            log.lock().expect("the log is not poisoned").push(Seen {
                name: event.data().name().to_owned(),
                value,
                checked,
                code,
                location,
                buttons,
                client,
                primary,
            });
        })]);
    let input = spawn_input(&mut world);
    (world, gpu, target, source, seen, input)
}

/// Spawn an input buffer at the target's size and a density of one.
fn spawn_input(world: &mut World) -> InputWriter {
    let input = InputWriter::new();
    let events = input.subscribe();
    // Input is shared by every view, so it belongs to the shared subtree: a
    // source reads it from its own scope, which cannot see a sibling's.
    world.scope_mut(world.shared()).spawn(events);
    input.push(InputEvent::Resized {
        size_px: (WIDTH, HEIGHT),
    });
    input.push(InputEvent::ScaleFactorChanged { scale_factor: 1.0 });
    refresh_input(world);
    input
}

/// Feed `batch` as the next frame's input, and tick the world's buffers the
/// way a frame's start does.
fn feed(world: &World, input: &InputWriter, batch: impl IntoIterator<Item = InputEvent>) {
    input.push_all(batch);
    refresh_input(world);
}

/// A press and a release at `position`, which is the pair a document reads as a
/// click.
fn click(position: [f32; 2]) -> [InputEvent; 2] {
    [
        InputEvent::Mouse(MouseEvent::Button {
            position: Some(position),
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        }),
        InputEvent::Mouse(MouseEvent::Button {
            position: Some(position),
            button: MouseButton::Primary,
            pressed: false,
            modifiers: Modifiers::default(),
        }),
    ]
}

/// Bind a target to the given pass, so the frame opens it and builds what it reaches.
fn bind_target(gpu: &TestGpu, world: &World, pass: Entity) -> wgpu::Texture {
    let device = gpu
        .context
        .device(&world.root_scope())
        .expect("the context device");
    let color = create_color_target(&device, COLOR_FORMAT, WIDTH, HEIGHT);
    let (_, color_view) = gpu.register_texture(world, color.clone());
    let attachments = FrameAttachments::new(
        world,
        gpu.context,
        COLOR_FORMAT,
        (WIDTH, HEIGHT),
        1,
        false,
        false,
    );
    let mut pass = world.root_scope().query_one::<&mut FramePass>(pass);
    pass.get().set_attachments(&attachments, color_view);
    color
}

gpu_tests! {
    a_page_paints_its_own_colours,
    a_page_draws_its_content,
    a_transparent_page_leaves_the_frame_alone,
    new_markup_replaces_the_page,
    a_page_draws_an_image_from_a_registered_path,
    a_page_draws_an_image_from_a_data_url,
    a_click_toggles_a_checkbox,
    typing_reaches_a_text_field,
    input_is_dispatched_once,
    a_page_loads_a_stylesheet_nobody_serves,
    a_page_loads_a_stylesheet_it_serves,
    a_new_base_still_serves_the_registered_bytes,
    a_replaced_page_is_not_drawn_from_the_old_pixels,
    one_tick_reaches_a_shared_page_once,
    losing_focus_leaves_the_text_field,
    a_key_release_reaches_the_page,
    a_wheel_scrolls_the_page,
    leaving_the_page_is_reported_to_it,
    a_callback_can_cancel_the_default_action,
    a_pen_click_toggles_a_checkbox,
    a_pen_press_states_the_press_it_makes,
    a_finger_that_stays_down_keeps_its_button,
    a_mouse_release_does_not_lift_the_pen,
    lifting_one_finger_leaves_the_other_down,
    an_ime_commit_reaches_a_text_field,
    a_key_reaches_the_page_as_a_code_and_a_side,
    a_batch_replays_from_the_state_that_stood_before_it,
    a_move_before_a_press_reports_the_button_that_still_stood,
    a_wheel_is_reported_where_the_pointer_stood,
    a_frame_without_events_keeps_the_pointer_state,
    a_first_frame_replays_from_an_empty_baseline,
    a_batch_that_skips_one_still_replays_from_the_state_left,
    a_replaced_page_replays_from_the_world_state,
}
