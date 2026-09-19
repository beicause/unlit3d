//! End-to-end GPU tests for the egui backend.
//!
//! These draw a real egui frame through [`UiRenderer`] and inspect the pixels
//! that come back, so a broken projection, a wrong blend or a missing texture
//! shows up as a wrong frame rather than as a passing unit test.

mod common;

use common::*;
use wgpu_unlit_render::globals::Globals;
use wgpu_unlit_render::pipeline::{CAMERA_BINDING, FRAME_BINDING, UnlitPipeline};
use wgpu_unlit_render::render_attachments::{AttachmentsInfo, RenderAttachments};
use wgpu_unlit_render::resources::{Resource, ResourceGraph};
use wgpu_unlit_render::ui::{EguiIntegration, screen_view, ui_options};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Clear color (linear), matching what a 3D scene behind the UI would leave.
const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
/// MSAA sample count shared by the pipeline and the renderer. The renderer
/// resolves into its target through the MSAA attachment, so a count of 1 would
/// discard everything.
const SAMPLES: u32 = 4;

/// The UI: text, a button, an opaque red rectangle and a translucent blue
/// rectangle, all at deterministic positions.
fn ui_contents(ui: &mut egui::Ui) {
    ui.label("hello world");
    let _ = ui.button("press me");
    ui.painter().rect_filled(
        egui::Rect::from_min_size(egui::Pos2::new(16.0, 120.0), egui::Vec2::new(96.0, 48.0)),
        0.0,
        egui::Color32::from_rgb(255, 0, 0),
    );
    ui.painter().rect_filled(
        egui::Rect::from_min_size(egui::Pos2::new(128.0, 120.0), egui::Vec2::new(96.0, 48.0)),
        0.0,
        egui::Color32::from_rgba_premultiplied(0, 0, 128, 128),
    );
}

fn input(pixels_per_point: f32) -> egui::RawInput {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(WIDTH as f32, HEIGHT as f32),
        )),
        ..Default::default()
    };
    let viewport = input
        .viewports
        .get_mut(&input.viewport_id)
        .expect("root viewport");
    viewport.native_pixels_per_point = Some(pixels_per_point);
    input
}

/// Run one egui frame through the renderer and return the pixels.
///
/// The first frame only knows the default font sizes, so it is run and
/// discarded; the measured frame is the second one.
fn render_ui(ctx: &Ctx, pixels_per_point: f32) -> Frame {
    render_ui_with(ctx, pixels_per_point, ui_contents)
}

/// Like [`render_ui`], drawing the caller's own `contents`.
///
/// The closure runs before each frame, so it can build and register egui
/// textures once and reuse them across the warm-up and the measured frame.
fn render_ui_with(
    ctx: &Ctx,
    pixels_per_point: f32,
    mut contents: impl FnMut(&mut egui::Ui),
) -> Frame {
    // The caller owns the globals: camera and frame buffers, registered in
    // the shared ledger, and the bind group binding them.
    let camera = uniform_buffer(&ctx.device, "ui::camera", view_size());
    let globals = uniform_buffer(&ctx.device, "ui::globals", globals_size());
    let mut graph = ResourceGraph::new();
    let camera_id = graph
        .insert(Resource::Buffer(camera.clone()), &[])
        .expect("an empty dependency list always resolves");
    let globals_id = graph
        .insert(Resource::Buffer(globals.clone()), &[])
        .expect("an empty dependency list always resolves");
    // The test target is sRGB: the UI converts its output to linear light.
    // The pipeline is built once and shared: the global bind group is created
    // from its layout, and the UI draws with it.
    let mut ui_opts = ui_options(true);
    ui_opts.color_target.format = COLOR_FORMAT;
    ui_opts.sample_count = SAMPLES;
    let pipeline = UnlitPipeline::new(&ctx.device, &ui_opts);
    let global_group_id = graph
        .insert(
            Resource::BindGroup(global_group(&ctx.device, &pipeline, &camera, &globals)),
            &[camera_id, globals_id],
        )
        .expect("both dependencies were registered");
    let mut ui = EguiIntegration::new(&ctx.device, global_group_id, pipeline);
    let egui_ctx = egui::Context::default();
    // egui positions its vertices in points, and the projection maps points
    // onto clip space, so the viewport the projection needs is the point size
    // regardless of the pixel density.
    let viewport = [WIDTH as f32, HEIGHT as f32];
    let warm = egui_ctx.run_ui(input(pixels_per_point), &mut contents);
    ui.update(&mut graph, &ctx.queue, &egui_ctx, warm, pixels_per_point);
    // egui's tessellated points map onto clip space through the caller's
    // camera uniform, written once: the viewport does not change.
    use zerocopy::IntoBytes;
    ctx.queue
        .write_buffer(&camera, 0, screen_view(viewport).as_bytes());
    ctx.queue
        .write_buffer(&globals, 0, Globals::default().as_bytes());
    let output = egui_ctx.run_ui(input(pixels_per_point), &mut contents);
    ui.update(&mut graph, &ctx.queue, &egui_ctx, output, pixels_per_point);

    let (width, height) = (WIDTH, HEIGHT);
    let context = RenderAttachments::new(
        &ctx.device,
        AttachmentsInfo {
            color: Some(COLOR_FORMAT),
            depth: Some(wgpu::TextureFormat::Depth32Float),
            width,
            height,
            sample_count: SAMPLES,
            transient_depth: true,
        },
    );
    // Readback comes from the context's own color texture.
    let target = context
        .color_texture()
        .expect("a color pass has a color texture")
        .clone();
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
    let scene = ui.scene(&mut graph);
    {
        let mut pass = context.begin_pass(
            &mut encoder,
            Some(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            Some(context.depth_clear()),
        );
        scene.record(&mut pass);
    }
    ctx.queue.submit([encoder.finish()]);
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");

    Frame {
        rgba: read_texture_bytes(ctx, &target, width, height, texel_bytes(&target)),
        width,
        height,
    }
}

/// A uniform buffer of `size` bytes, written through the queue.
fn uniform_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Shader size of the camera uniform, straight from its layout.
fn view_size() -> u64 {
    <wgpu_unlit_render::globals::View as const_shader_layout::ShaderLayout>::SIZE.get()
}

/// Shader size of the frame-globals uniform.
fn globals_size() -> u64 {
    <Globals as const_shader_layout::ShaderLayout>::SIZE.get()
}

/// The UI's global bind group: camera and frame-globals uniforms.
///
/// The layout is the built-in UI pipeline's global layout, so the bind group
/// is created from the shared pipeline ahead of the UI that draws with it.
fn global_group(
    device: &wgpu::Device,
    pipeline: &UnlitPipeline,
    camera: &wgpu::Buffer,
    globals: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ui::globals"),
        layout: &pipeline.global_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: CAMERA_BINDING,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: FRAME_BINDING,
                resource: globals.as_entire_binding(),
            },
        ],
    })
}

#[test]
fn text_and_button_render_over_the_clear() {
    let ctx = Ctx::headless();
    let frame = render_ui(&ctx, 1.0);

    // Text in the top-left region: the glyphs use egui's light default color,
    // so at least some pixels there are clearly brighter than the clear.
    let mut any_bright = false;
    for y in 0..40 {
        for x in 0..120 {
            let p = frame.pixel_u8(x, y);
            if p[0] > 100 && p[1] > 100 && p[2] > 100 {
                any_bright = true;
            }
        }
    }
    assert!(
        any_bright,
        "text should render bright pixels in the top-left"
    );
}

#[test]
fn opaque_rect_reaches_full_red() {
    let ctx = Ctx::headless();
    let frame = render_ui(&ctx, 1.0);

    // Centre of the opaque red rect (16,120)-(112,168).
    let centre = frame.pixel_u8(64, 144);
    assert!(
        centre[0] > 200 && centre[1] < 60 && centre[2] < 60,
        "the opaque rect should be bright red, got {centre:?}"
    );
}

#[test]
fn translucent_rect_blends_over_the_clear() {
    let ctx = Ctx::headless();
    let frame = render_ui(&ctx, 1.0);

    // Centre of the translucent blue rect (128,120)-(224,168): premultiplied
    // half-alpha blue over the dark clear — clearly blue, but not full blue.
    let centre = frame.pixel_u8(176, 144);
    assert!(
        centre[2] > centre[0] && centre[2] > centre[1] && centre[2] < 250,
        "the translucent rect should be blue-tinted but not opaque, got {centre:?}"
    );
}

/// The UI must land in the same place whatever the pixel density: egui
/// tessellates in physical pixels, so a denser target scales the layout rather
/// than moving it.
#[test]
fn ui_lands_in_the_same_place_at_any_pixel_density() {
    let ctx = Ctx::headless();
    let low = render_ui(&ctx, 1.0);
    let high = render_ui(&ctx, 2.0);

    // The layout is in points, so the red rect's centre in points is the same
    // physical pixel at both densities. A projection that wrongly scaled by
    // pixels_per_point would place it elsewhere.
    for (frame, ppp) in [(&low, 1.0), (&high, 2.0)] {
        let centre = frame.pixel_u8(64, 144);
        assert!(
            centre[0] > 150 && centre[1] < 100,
            "the red rect centre should be red at {ppp}x, got {centre:?}"
        );
    }
}

#[test]
fn ui_matches_snapshot() {
    let ctx = Ctx::headless();
    // The image texture is registered once and reused, so the measured frame
    // redraws the same texture rather than reallocating it.
    let mut image: Option<egui::TextureHandle> = None;
    let frame = render_ui_with(&ctx, 1.0, |ui| {
        ui_contents(ui);
        let handle = image.get_or_insert_with(|| load_test_image(ui));
        draw_test_image(ui, handle);
    });
    assert_image_snapshot("egui_ui.webp", &frame, frame.width, frame.height);
}

/// A magenta-and-white 2x1 image registered with egui: the left half is
/// magenta, the right half white.
fn load_test_image(ui: &mut egui::Ui) -> egui::TextureHandle {
    let size = [2, 1];
    let pixels = vec![
        egui::Color32::from_rgb(255, 0, 255),
        egui::Color32::from_rgb(255, 255, 255),
    ];
    let image_data =
        egui::ImageData::Color(std::sync::Arc::new(egui::ColorImage::new(size, pixels)));
    ui.ctx()
        .load_texture("test::image", image_data, egui::TextureOptions::NEAREST)
}

/// Draw `texture` stretched over a 128x64 rectangle at (16, 16): the left
/// half magenta, the right half white.
fn draw_test_image(ui: &mut egui::Ui, texture: &egui::TextureHandle) {
    ui.painter().image(
        texture.id(),
        egui::Rect::from_min_size(egui::pos2(16.0, 16.0), egui::vec2(128.0, 64.0)),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// A user-allocated image (not the font atlas) must upload and render: this
/// exercises the [`egui::ImageData`] -> texture path that the text tests
/// never touch. Drawing text beside it makes the frame carry two distinct
/// textures, so the material groups cannot be confused with one another.
#[test]
fn user_image_uploads_and_renders() {
    let ctx = Ctx::headless();
    let mut image: Option<egui::TextureHandle> = None;

    let frame = render_ui_with(&ctx, 1.0, |ui| {
        ui.label("hello");
        let handle = image.get_or_insert_with(|| load_test_image(ui));
        draw_test_image(ui, handle);
    });

    // The left half of the drawn image is magenta (255,0,255): at its centre
    // the red and blue channels are strong and green is absent.
    let left = frame.pixel_u8(48, 48);
    assert!(
        left[0] > 150 && left[2] > 150 && left[1] < 80,
        "the image's left half should be magenta, got {left:?}"
    );

    // The right half is white: all channels bright at the right-hand centre.
    let right = frame.pixel_u8(128, 48);
    assert!(
        right[0] > 150 && right[1] > 150 && right[2] > 150,
        "the image's right half should be white, got {right:?}"
    );
}
