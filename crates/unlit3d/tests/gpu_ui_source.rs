//! End-to-end GPU tests for [`UiSource`] as a frame source.
//!
//! Two groups. The first draws UI with no `MeshSource` and no camera at all,
//! which is the frame shape that used to produce nothing because the renderer
//! bailed out when it had no camera. The second draws a mesh and a UI in one
//! pass, which is where the two sources have to agree on record order, scissor
//! state and the frame's load ops.
//!
//! Every UI test renders one frame to warm egui up — the first frame does not
//! know the font metrics — and asserts on the second. Nothing here reads the
//! clock or the layout feedback, so a frame is reproducible.

#![cfg(feature = "ui")]

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// The colour a UI-only test clears to, distinguishable from every panel.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.02,
    b: 0.02,
    a: 1.0,
};

/// The clear colour as the byte an sRGB target stores for it.
///
/// A `wgpu::Color` is linear, and the target encodes it on write, so the byte
/// readback returns is the sRGB encoding rather than the linear value.
fn clear_rgb() -> [u8; 3] {
    let encode = |linear: f64| {
        let linear = linear.clamp(0.0, 1.0);
        let srgb = if linear <= 0.0031308 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round() as u8
    };
    [
        encode(CLEAR_COLOR.r),
        encode(CLEAR_COLOR.g),
        encode(CLEAR_COLOR.b),
    ]
}

/// A panel's own colour, opaque.
const RED: egui::Color32 = egui::Color32::from_rgb(255, 0, 0);

/// The second panel's colour, chosen so a pixel tells the two apart.
const GREEN: egui::Color32 = egui::Color32::from_rgb(0, 255, 0);

/// A translucent red: the UI's red at half alpha.
const TRANSLUCENT_RED: egui::Color32 = egui::Color32::from_rgba_premultiplied(128, 0, 0, 128);

/// Where a panel paints, in logical points.
#[derive(Clone, Copy)]
struct Rect {
    min: egui::Pos2,
    size: egui::Vec2,
}

impl Rect {
    const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            min: egui::Pos2::new(x, y),
            size: egui::Vec2::new(w, h),
        }
    }

    fn egui(self) -> egui::Rect {
        egui::Rect::from_min_size(self.min, self.size)
    }

    /// The centre of the rectangle, in points.
    fn centre(self) -> egui::Pos2 {
        self.egui().center()
    }

    /// A point `inset` points inside the rectangle's bottom-right corner; a
    /// negative `inset` reaches outside it.
    ///
    /// The corner is where a clip rectangle that forgot to scale by the pixel
    /// density stops matching the panel, so a test samples here rather than at
    /// the centre, which stays correct either way.
    fn far_corner(self, inset: f32) -> egui::Pos2 {
        let far = self.egui().max;
        egui::Pos2::new(far.x - inset, far.y - inset)
    }
}

/// A panel that paints `rect` in `color`.
fn paint(rect: Rect, color: egui::Color32) -> UiPanel {
    UiPanel::new(move |_world, _entity, ui| {
        ui.painter().rect_filled(rect.egui(), 0.0, color);
    })
}

/// The texels of the test image: four quadrants, each checkerboarded between a
/// light and a dark shade, so a stretched draw shows eight colours and a broken
/// sampler is obvious.
fn test_image() -> egui::ColorImage {
    const SIZE: usize = 8;
    const QUADRANT: usize = SIZE / 2;

    /// The light and dark texel of the quadrant `(x, y)` falls in.
    fn shades(x: usize, y: usize) -> ((u8, u8, u8), (u8, u8, u8)) {
        match (x / QUADRANT, y / QUADRANT) {
            (0, 0) => ((255, 96, 96), (160, 0, 0)),
            (1, 0) => ((96, 255, 96), (0, 160, 0)),
            (0, 1) => ((96, 96, 255), (0, 0, 160)),
            _ => ((255, 255, 96), (160, 160, 0)),
        }
    }

    let pixels = (0..SIZE)
        .flat_map(|y| {
            (0..SIZE).map(move |x| {
                let (light, dark) = shades(x, y);
                let (r, g, b) = if (x + y) % 2 == 0 { light } else { dark };
                egui::Color32::from_rgb(r, g, b)
            })
        })
        .collect();
    egui::ColorImage::new([SIZE, SIZE], pixels)
}

/// Where the rich panel puts its parts, in logical points.
///
/// They leave the top middle of the frame clear — that is where the mesh shows
/// through in a combined frame — and the band is the lowest of them, so it is
/// the only thing covering the bottom of the frame.
const WIDGETS: Rect = Rect::new(8.0, 8.0, 112.0, 128.0);
const PICTURE: Rect = Rect::new(136.0, 8.0, 112.0, 72.0);
const BAND: Rect = Rect::new(8.0, 136.0, 240.0, 40.0);

/// Two of the shades [`test_image`] is built from. Neither appears anywhere
/// else in the panel, so a pixel of one proves the texture was sampled.
const LIGHT_GREEN: egui::Color32 = egui::Color32::from_rgb(96, 255, 96);
const LIGHT_BLUE: egui::Color32 = egui::Color32::from_rgb(96, 96, 255);

/// A rich, deterministic interface: real widgets, a texture and painted
/// geometry.
///
/// Everything sits at a fixed place, and nothing here reads the clock or reacts
/// to layout feedback, so two frames of the same world come out identical —
/// which is what lets it stand as a snapshot. The blend band is translucent so
/// that it shows the UI blending over whatever was drawn before it rather than
/// replacing it.
fn rich_panel() -> UiPanel {
    // The texture is registered inside the closure because it needs a context,
    // and cached so the passes egui runs in one frame share one handle. The
    // switch and the slider keep their state between frames for the same
    // reason: a widget that changed per frame would make the snapshot flaky.
    let mut image: Option<egui::TextureHandle> = None;
    let mut spin = true;
    let mut blend = 0.5f32;

    UiPanel::new(move |_world, _entity, ui| {
        let image = image.get_or_insert_with(|| {
            ui.ctx().load_texture(
                "unlit3d::test-image",
                test_image(),
                egui::TextureOptions::NEAREST,
            )
        });

        // The widgets go into a child ui pinned to a fixed rectangle, so their
        // layout does not depend on how much room the other panels took.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(WIDGETS.egui())
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.heading("unlit3d");
                ui.label("frame source UI");
                ui.separator();
                ui.checkbox(&mut spin, "spin");
                ui.add(egui::Slider::new(&mut blend, 0.0..=1.0));
                ui.add(
                    egui::ProgressBar::new(blend)
                        .desired_width(104.0)
                        .text("load"),
                );
            },
        );

        let painter = ui.painter();
        // The texture, stretched over its own rectangle. `NEAREST` keeps its
        // texels square, so a sampled pixel is one of the eight colours the
        // image was built from.
        painter.image(
            image.id(),
            PICTURE.egui(),
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        painter.line_segment(
            [
                egui::pos2(PICTURE.min.x, 86.0),
                egui::pos2(PICTURE.egui().max.x, 86.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_gray(120)),
        );
        // Shapes no widget produces, so the frame exercises the tessellated
        // path as well as the widget one. They sit to the right of the widget
        // column, clear of the slider's own drag-value box.
        painter.circle_filled(egui::pos2(190.0, 112.0), 15.0, GREEN);
        painter.circle_stroke(
            egui::pos2(190.0, 112.0),
            15.0,
            egui::Stroke::new(3.0, egui::Color32::WHITE),
        );
        painter.rect_stroke(
            egui::Rect::from_min_size(egui::pos2(212.0, 96.0), egui::Vec2::new(36.0, 32.0)),
            3.0,
            egui::Stroke::new(2.0, RED),
            egui::StrokeKind::Inside,
        );
        painter.rect_filled(BAND.egui(), 0.0, TRANSLUCENT_RED);
    })
}

/// A panel that paints the band in `color`, opaque or not.
///
/// The band is the part of the panel that covers a mesh behind the UI, so a
/// test that measures how an order or a blend resolves uses this rather than
/// the full panel.
fn band_panel(color: egui::Color32) -> UiPanel {
    paint(BAND, color)
}

/// The pixel a point lands on, at `pixels_per_point`.
fn pixel_of(point: egui::Pos2, pixels_per_point: f32) -> (u32, u32) {
    (
        (point.x * pixels_per_point).round() as u32,
        (point.y * pixels_per_point).round() as u32,
    )
}

/// Read a frame back out of `target`.
fn read(ctx: &Ctx, target: &wgpu::Texture) -> Frame {
    Frame {
        rgba: read_texture_bytes(ctx, target, WIDTH, HEIGHT, texel_bytes(target)),
        width: WIDTH,
        height: HEIGHT,
    }
}

/// Whether `pixel`'s colour is within `tolerance` per channel of `color`.
///
/// Readback returns the sRGB-encoded byte, which is the space egui's own
/// colours are stated in, so the two are compared directly.
fn near(pixel: [u8; 4], color: egui::Color32, tolerance: u8) -> bool {
    [color.r(), color.g(), color.b()]
        .iter()
        .zip(pixel[..3].iter())
        .all(|(&want, &got)| got.abs_diff(want) <= tolerance)
}

/// Whether `pixel` is the load-op clear colour.
fn is_clear(pixel: [u8; 4], tolerance: u8) -> bool {
    clear_rgb()
        .iter()
        .zip(pixel[..3].iter())
        .all(|(&want, &got)| got.abs_diff(want) <= tolerance)
}

/// Count the pixels in `x0..x1` that are within `tolerance` of `color`.
fn count_in(frame: &Frame, x0: u32, x1: u32, color: egui::Color32, tolerance: u8) -> usize {
    let mut count = 0;
    for y in 0..frame.height {
        for x in x0..x1 {
            if near(frame.pixel_u8(x, y), color, tolerance) {
                count += 1;
            }
        }
    }
    count
}

/// Count the pixels of `rect` that are within `tolerance` of `color`.
///
/// The rectangle is in logical points, so its pixel extent scales with the
/// density the same way the panel's own drawing does.
fn count_in_rect(frame: &Frame, rect: Rect, color: egui::Color32, tolerance: u8) -> usize {
    let (x0, y0) = pixel_of(rect.min, 1.0);
    let (x1, y1) = pixel_of(rect.egui().max, 1.0);
    let mut count = 0;
    for y in y0..y1.min(frame.height) {
        for x in x0..x1.min(frame.width) {
            if near(frame.pixel_u8(x, y), color, tolerance) {
                count += 1;
            }
        }
    }
    count
}

/// A UI-only world: a frame context, a renderer and a `UiSource`, with no
/// `MeshSource` and no camera.
fn ui_only_world(world: &mut LocalWorld, ctx: &Ctx) -> TestGpu {
    let gpu = TestGpu::frame_only(world, ctx);
    spawn_source(world, UiSource::new());
    gpu
}

/// Spawn an `InputState` stating the window's size and density.
///
/// The UI reads both from it, so this is how a test sets the pixel density.
fn spawn_input(world: &mut LocalWorld, scale_factor: f32) {
    let input = world.spawn((Resource, InputState::default()));
    let _ = world.with_mut::<InputState, _>(input, |state| {
        state.set_size_px(WIDTH, HEIGHT);
        state.set_scale_factor(scale_factor);
    });
}

/// Spawn the load ops every UI test opens its pass with.
fn spawn_load_ops(world: &mut LocalWorld) {
    world.spawn((
        Resource,
        RenderLoadOps {
            color: wgpu::LoadOp::Clear(CLEAR_COLOR),
            ..RenderLoadOps::default()
        },
    ));
}

// ---------------------------------------------------------------------------
// A. UI only — no mesh source, no camera
// ---------------------------------------------------------------------------

/// The UI draws in a frame with no camera and no mesh source at all.
///
/// This is the frame shape the renderer used to skip: it returned early when
/// the world held no camera, so a UI-only frame would have come out empty.
#[test]
fn ui_only_draws_without_a_camera() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, &ctx);
    spawn_load_ops(&mut world);

    world.spawn((rich_panel(),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    // The textured rectangle samples the image, so pixels of shades only the
    // image holds prove the texture made it to the GPU and was drawn.
    assert!(
        count_in_rect(&frame, PICTURE, LIGHT_GREEN, 12) > 0
            && count_in_rect(&frame, PICTURE, LIGHT_BLUE, 12) > 0,
        "the textured rectangle must sample the image's own colours"
    );
    // The widgets lay out text, so the column holds pixels that are neither the
    // clear colour nor the band: they can only be the text and controls.
    let (wx0, wy0) = pixel_of(WIDGETS.min, 1.0);
    let (wx1, wy1) = pixel_of(WIDGETS.egui().max, 1.0);
    let mut widget_pixels = 0;
    for y in wy0..wy1 {
        for x in wx0..wx1 {
            let pixel = frame.pixel_u8(x, y);
            if !is_clear(pixel, 12) && !near(pixel, TRANSLUCENT_RED, 8) {
                widget_pixels += 1;
            }
        }
    }
    assert!(
        widget_pixels > 200,
        "the widget column must draw its text and controls, only {widget_pixels} \
         pixels differ from the background"
    );
    // The band blends over the background rather than replacing it, so it comes
    // out a darker red than the pure colour.
    let (bx, by) = pixel_of(BAND.centre(), 1.0);
    let band = frame.pixel_u8(bx, by);
    assert!(
        band[0] > band[1] && band[0] > band[2],
        "the band tints its own region red, got {band:?}"
    );

    assert_image_snapshot("ui_only.webp", &frame, WIDTH, HEIGHT);
}

/// Two panels are both driven, and each paints its own region.
///
/// Panels are entities, so the source has to run every one of them rather than
/// a single interface.
#[test]
fn ui_only_with_two_panels() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, &ctx);
    spawn_load_ops(&mut world);

    let left = Rect::new(16.0, 40.0, 72.0, 72.0);
    let right = Rect::new(160.0, 40.0, 72.0, 72.0);
    world.spawn((paint(left, RED),));
    world.spawn((paint(right, GREEN),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    let (lx, ly) = pixel_of(left.centre(), 1.0);
    let (rx, ry) = pixel_of(right.centre(), 1.0);
    assert!(
        near(frame.pixel_u8(lx, ly), RED, 8),
        "the first panel paints red, got {:?}",
        frame.pixel_u8(lx, ly)
    );
    assert!(
        near(frame.pixel_u8(rx, ry), GREEN, 8),
        "the second panel paints green, got {:?}",
        frame.pixel_u8(rx, ry)
    );

    // Each colour is confined to its own half: the two panels were drawn at
    // their own positions, not both at one.
    let half = WIDTH / 2;
    assert_eq!(
        count_in(&frame, 0, half, RED, 8),
        (left.size.x * left.size.y) as usize,
        "the red panel fills its own half only"
    );
    assert_eq!(
        count_in(&frame, half, WIDTH, GREEN, 8),
        (right.size.x * right.size.y) as usize,
        "the green panel fills its own half only"
    );
}

/// The pass still opens with the frame's load ops; the UI draws on top.
///
/// A UI is an overlay, so a pixel no panel covers must still be the clear
/// colour the `RenderLoadOps` asked for.
#[test]
fn ui_only_clears_to_load_ops() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, &ctx);
    spawn_load_ops(&mut world);

    let rect = Rect::new(40.0, 40.0, 80.0, 60.0);
    world.spawn((paint(rect, RED),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    // A corner is outside the panel and keeps the clear colour.
    let corner = frame.pixel_u8(2, HEIGHT - 3);
    assert!(
        is_clear(corner, 4),
        "an uncovered pixel should be the load-op clear colour {:?}, got {corner:?}",
        clear_rgb()
    );
    let (x, y) = pixel_of(rect.centre(), 1.0);
    assert!(
        !is_clear(frame.pixel_u8(x, y), 12),
        "a covered pixel should not be the clear colour"
    );
}

/// The UI draws correctly into a multisampled sRGB target.
///
/// The pipeline is specialized for the target's sample count and color
/// format, so a mismatch would be a validation error, and an sRGB mistake
/// would show up as an inverted or darkened colour.
#[test]
fn ui_only_multisampled_and_srgb() {
    const SAMPLES: u32 = 4;
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, &ctx);
    spawn_load_ops(&mut world);

    let rect = Rect::new(40.0, 40.0, 80.0, 60.0);
    world.spawn((paint(rect, RED),));

    let target = gpu.bind_offscreen_target_with(&world, SAMPLES, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    let (x, y) = pixel_of(rect.centre(), 1.0);
    let centre = frame.pixel_u8(x, y);
    // The target is sRGB, so the stored byte is the colour egui wrote rather
    // than a linear value: a correct frame matches the panel's own channels.
    assert!(
        near(centre, RED, 8),
        "an sRGB multisampled target should hold the panel's colour, got {centre:?}"
    );
    assert_eq!(
        count_in(&frame, 0, WIDTH, RED, 8),
        (rect.size.x * rect.size.y) as usize,
        "the panel covers its own area under multisampling"
    );
}

/// A panel keeps its position and size at a density above one.
///
/// The panel is laid out in logical points while the clip rectangles are
/// physical pixels. This samples the rectangle's far corner because a clip
/// that forgot to scale by the density would cut the panel short there while
/// leaving its centre intact.
#[test]
fn ui_only_at_high_pixel_density() {
    const PPP: f32 = 2.0;
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, &ctx);
    spawn_load_ops(&mut world);
    spawn_input(&mut world, PPP);

    // Comfortably inside the target at 2x: 60 x 40 points is 120 x 80 pixels.
    let rect = Rect::new(24.0, 20.0, 60.0, 40.0);
    world.spawn((paint(rect, RED),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    let (cx, cy) = pixel_of(rect.centre(), PPP);
    assert!(
        near(frame.pixel_u8(cx, cy), RED, 10),
        "the panel's centre survives the density, got {:?}",
        frame.pixel_u8(cx, cy)
    );

    // One pixel inside the far corner must still be the panel's colour.
    let (fx, fy) = pixel_of(rect.far_corner(1.0), PPP);
    let far = frame.pixel_u8(fx, fy);
    assert!(
        near(far, RED, 12),
        "the panel's far corner must survive the density, got {far:?} at ({fx}, {fy})"
    );

    // Just outside it the clear colour shows.
    let (ox, oy) = pixel_of(rect.far_corner(-2.0), PPP);
    assert!(
        is_clear(frame.pixel_u8(ox, oy), 12),
        "outside the panel the clear colour shows, got {:?}",
        frame.pixel_u8(ox, oy)
    );

    // And the panel is twice as wide in pixels as it is in points.
    assert_eq!(
        count_in(&frame, 0, WIDTH, RED, 12),
        (rect.size.x * PPP * rect.size.y * PPP) as usize,
        "the panel covers points times density in pixels"
    );
}

// ---------------------------------------------------------------------------
// B. Mesh and UI in one frame
// ---------------------------------------------------------------------------

/// A world with a cube drawn through the mesh source.
fn mesh_world(ctx: &Ctx, world: &mut LocalWorld) -> TestGpu {
    let gpu = TestGpu::new(world, ctx);
    let key = gpu.key.clone();
    let mesh = gpu.allocate_cube_mesh(world);
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));
    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(0.6),
            scale: glam::Vec3::splat(0.7),
        },
        mesh,
        UnlitPipeline::new(key),
    ));
    gpu
}

/// Render a mesh+UI frame and return the pixels.
///
/// The `panel` is the UI, and `ui_first` decides whether it is recorded before
/// or after the mesh.
fn mesh_and_ui_frame(ctx: &Ctx, world: &mut LocalWorld, ui_first: bool, panel: UiPanel) -> Frame {
    let gpu = mesh_world(ctx, world);
    spawn_load_ops(world);

    world.spawn((panel,));
    let ui = spawn_source(world, UiSource::new());
    if ui_first {
        let _ = world.with_mut::<Source, _>(ui, |source| {
            source.set_order(Some(FrameOrder(FrameOrder::MESH.0 - 1)))
        });
    }

    let target = gpu.bind_offscreen_target_with(world, 1, true);
    gpu.render_frames(world, 2);
    read(ctx, &target)
}

/// A pixel above the band, where only the cube can be.
const CUBE_PIXEL: (u32, u32) = (128, 60);
/// A pixel inside the band that the cube also covers.
const OVERLAP_PIXEL: (u32, u32) = (128, 144);
/// A pixel inside the band that the cube does not cover.
const PANEL_PIXEL: (u32, u32) = (16, 144);

/// A mesh and a UI in one pass: the mesh keeps its own look, the UI covers its
/// own region, and where they overlap the translucent band blends over the
/// cube.
#[test]
fn mesh_and_ui_in_one_frame() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let frame = mesh_and_ui_frame(&ctx, &mut world, false, rich_panel());

    let cube = frame.pixel_u8(CUBE_PIXEL.0, CUBE_PIXEL.1);
    let panel = frame.pixel_u8(PANEL_PIXEL.0, PANEL_PIXEL.1);
    let overlap = frame.pixel_u8(OVERLAP_PIXEL.0, OVERLAP_PIXEL.1);

    assert!(
        !is_clear(cube, 12) && !near(cube, TRANSLUCENT_RED, 8),
        "the cube keeps its own look where no panel covers it, got {cube:?}"
    );
    assert!(
        panel[0] > panel[1] && panel[0] > panel[2],
        "the translucent panel tints its own region red, got {panel:?}"
    );
    // The panel blends over the cube rather than replacing it, so the overlap
    // is neither the panel's colour alone nor the cube's.
    assert!(
        overlap != panel,
        "the panel should blend over the cube, not flatten it: overlap {overlap:?} equals panel {panel:?}"
    );
    assert!(
        overlap != cube,
        "the panel should tint the cube behind it, but overlap {overlap:?} equals cube {cube:?}"
    );
    assert!(
        overlap[0] > overlap[2],
        "the blend takes the panel's red, got {overlap:?}"
    );

    assert_image_snapshot("mesh_and_ui.webp", &frame, WIDTH, HEIGHT);
}

/// Record order follows the declared `FrameOrder`, not the mount order.
///
/// The panel is opaque so each order leaves one colour on top: with the UI
/// last the panel's colour survives the overlap, and with the UI first the
/// cube's does. Nothing but the record order differs between the two frames.
#[test]
fn mesh_and_ui_respects_source_order() {
    /// An opaque panel, so the topmost draw decides the overlap pixel.
    const OPAQUE_BLUE: egui::Color32 = egui::Color32::from_rgb(0, 0, 255);

    let ctx = Ctx::headless();
    let mut ui_last = LocalWorld::new();
    let ui_on_top = mesh_and_ui_frame(&ctx, &mut ui_last, false, band_panel(OPAQUE_BLUE));

    let mut ui_first = LocalWorld::new();
    let mesh_on_top = mesh_and_ui_frame(&ctx, &mut ui_first, true, band_panel(OPAQUE_BLUE));

    // The overlap lies over the cube, so whichever source records last wins.
    let overlap_ui_on_top = ui_on_top.pixel_u8(OVERLAP_PIXEL.0, OVERLAP_PIXEL.1);
    let overlap_mesh_on_top = mesh_on_top.pixel_u8(OVERLAP_PIXEL.0, OVERLAP_PIXEL.1);

    assert!(
        near(overlap_ui_on_top, OPAQUE_BLUE, 8),
        "recorded last, the panel's colour must win the overlap, got {overlap_ui_on_top:?}"
    );
    assert!(
        !near(overlap_mesh_on_top, OPAQUE_BLUE, 8),
        "recorded first, the panel must lose the overlap to the cube, got {overlap_mesh_on_top:?}"
    );
    assert_ne!(
        overlap_ui_on_top, overlap_mesh_on_top,
        "the record order must change what the overlap looks like"
    );

    // The cube is unchanged where no panel covers it: only the overlap depends
    // on the order. Comparing the same pixel across the two frames is what
    // makes this independent of the mesh's per-vertex colours.
    assert_eq!(
        ui_on_top.pixel_u8(CUBE_PIXEL.0, CUBE_PIXEL.1),
        mesh_on_top.pixel_u8(CUBE_PIXEL.0, CUBE_PIXEL.1),
        "the order does not change the cube where no panel covers it"
    );
    // And the panel away from the cube is drawn either way.
    assert_eq!(
        ui_on_top.pixel_u8(PANEL_PIXEL.0, PANEL_PIXEL.1),
        mesh_on_top.pixel_u8(PANEL_PIXEL.0, PANEL_PIXEL.1),
        "the panel is drawn the same where nothing overlaps it"
    );
}

/// The UI's clip rectangle must not clip the mesh drawn before it.
///
/// Scissor state is a local of each `Scene::record`, not of the pass, so a
/// panel's clip cannot reach the earlier 3D draws.
#[test]
fn ui_does_not_clip_the_mesh() {
    /// A clip region in one corner, far from the cube.
    const CLIP: f32 = 32.0;
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = mesh_world(&ctx, &mut world);
    spawn_load_ops(&mut world);

    world.spawn((UiPanel::new(|_world, _entity, ui| {
        let clip = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(CLIP));
        ui.painter().with_clip_rect(clip).rect_filled(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(CLIP)),
            0.0,
            RED,
        );
    }),));
    spawn_source(&mut world, UiSource::new());

    let target = gpu.bind_offscreen_target_with(&world, 1, true);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    // The cube sits around the middle of the frame, well outside the clip
    // region, so any cube pixel there proves the clip did not reach it.
    let mut cube_pixels = 0;
    let mut counted = 0;
    for y in HEIGHT / 4..(HEIGHT * 3) / 4 {
        for x in WIDTH / 4..(WIDTH * 3) / 4 {
            counted += 1;
            let pixel = frame.pixel_u8(x, y);
            if !is_clear(pixel, 12) {
                cube_pixels += 1;
            }
        }
    }
    assert!(
        cube_pixels > counted / 4,
        "the cube must survive the UI's clip rectangle: {cube_pixels}/{counted} \
         pixels drawn outside the clip region"
    );
}

/// Two frames of a static scene are pixel-identical.
///
/// A difference means some per-frame state — a cache, a uniform, a texture or a
/// scene — leaked from one frame into the next. The rich panel is what makes
/// this worth checking: it registers a texture on its first frame, and every
/// later frame has to reuse it rather than re-upload or lose it.
#[test]
fn mesh_and_ui_survive_a_second_frame() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = mesh_world(&ctx, &mut world);
    spawn_load_ops(&mut world);

    world.spawn((rich_panel(),));
    spawn_source(&mut world, UiSource::new());

    let target = gpu.bind_offscreen_target_with(&world, 1, true);
    // One frame warms egui up, then two are compared.
    gpu.render(&world);
    gpu.render(&world);
    let first = read(&ctx, &target);
    gpu.render(&world);
    let second = read(&ctx, &target);

    assert_eq!(
        first.rgba, second.rgba,
        "a static frame must be identical on the next frame"
    );
}

/// Where [`burst_panel`] always paints its red rectangle.
const FIXED: Rect = Rect::new(40.0, 40.0, 80.0, 60.0);

/// How many extra rectangles a [`burst_panel`] paints.
#[derive(Clone, Copy)]
struct Burst(usize);

/// A panel painting a red rectangle at [`FIXED`], plus `extra` green ones.
///
/// The extras exist only to change how much geometry a frame holds: the
/// geometry buffer grows geometrically and never shrinks, so a frame that once
/// held many of them leaves the buffer with room to spare for every later one.
fn burst_panel(extra: Entity) -> UiPanel {
    UiPanel::new(move |world, _entity, ui| {
        let burst = world.get::<Burst>(extra).map_or(0, |burst| burst.0);
        let painter = ui.painter();
        for index in 0..burst {
            let column = (index % 8) as f32;
            let row = (index / 8) as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(column * 16.0, row * 16.0),
                    egui::Vec2::splat(8.0),
                ),
                0.0,
                GREEN,
            );
        }
        painter.rect_filled(FIXED.egui(), 0.0, RED);
    })
}

/// Render the same small panel twice, after a frame of `burst` extra geometry.
///
/// The first frame leaves the geometry buffer sized for `burst` rectangles and
/// it never shrinks, so the second frame's own geometry sits in a buffer with
/// room to spare. How much geometry a frame holds must not change where its
/// vertices are read from.
fn frame_after_burst(ctx: &Ctx, burst: usize) -> Frame {
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, ctx);
    spawn_load_ops(&mut world);

    let extra = world.spawn((Burst(burst),));
    world.spawn((burst_panel(extra),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    // The first frame is drawn with the extra geometry, the second without.
    gpu.render(&world);
    let _ = world.with_mut::<Burst, _>(extra, |burst| burst.0 = 0);
    gpu.render(&world);
    read(ctx, &target)
}

/// A frame whose geometry buffer has room to spare draws the same picture as
/// one that fills it exactly.
///
/// The buffer is split in two: every vertex's position first, then its UV and
/// color. That split is the frame's own vertex count, not the buffer's
/// capacity — and the two differ as soon as a frame shrinks, because the buffer
/// keeps the larger size it grew to. Splitting on the capacity instead leaves
/// the positions right and the UVs and colors wrong, which reads back as a
/// garbled but correctly-placed interface.
#[test]
fn a_frame_with_spare_geometry_buffer_room_draws_the_same_picture() {
    let ctx = Ctx::headless();
    let grown = frame_after_burst(&ctx, 64);
    let filled = frame_after_burst(&ctx, 0);

    // Both frames paint the same red rectangle once the burst is gone.
    let (x, y) = pixel_of(FIXED.centre(), 1.0);
    assert!(
        near(grown.pixel_u8(x, y), RED, 8),
        "the fixed rectangle must still be red after a larger frame, got {:?}",
        grown.pixel_u8(x, y)
    );
    assert_eq!(
        grown.rgba, filled.rgba,
        "a frame must draw the same picture whether or not the geometry buffer \
         has room to spare"
    );
}

// ---------------------------------------------------------------------------
// C. Texture updates after the first upload
// ---------------------------------------------------------------------------

/// Where a patch of the second texture is written, in texels.
const PATCH_AT: [usize; 2] = [2, 2];
/// The second texture's side, in texels.
const PATCHED_SIDE: usize = 8;
/// Where the patched texture is painted, in logical points.
const PATCHED: Rect = Rect::new(136.0, 96.0, 64.0, 64.0);

/// A texture that starts black and has a red square patched into its middle.
///
/// The whole image is uploaded on the first pass, so the patch in the second
/// pass is the only thing that can put red in the middle — which is what makes
/// this a test of the partial-update path rather than of textures in general.
fn patched_texture() -> UiPanel {
    /// The texel the patch is filled with.
    const RED_TEXEL: egui::Color32 = egui::Color32::from_rgb(255, 0, 0);
    /// The surrounding texels, chosen so only the patch can be red.
    const BLACK_TEXEL: egui::Color32 = egui::Color32::from_rgb(0, 0, 0);

    let mut handle: Option<egui::TextureHandle> = None;
    let mut patched = false;

    UiPanel::new(move |_world, _entity, ui| {
        let handle = handle.get_or_insert_with(|| {
            let blank = egui::ColorImage::new(
                [PATCHED_SIDE, PATCHED_SIDE],
                vec![BLACK_TEXEL; PATCHED_SIDE * PATCHED_SIDE],
            );
            let handle =
                ui.ctx()
                    .load_texture("unlit3d::patched", blank, egui::TextureOptions::NEAREST);
            // Painting it here is what makes the frame reference the texture;
            // the painter only records the id.
            handle
        });

        // The patch is written once, after the texture exists, so it arrives as
        // a partial delta rather than as part of the first full upload.
        if !patched {
            patched = true;
            // A solid square in the middle of the otherwise black image.
            let texels = vec![RED_TEXEL; (PATCHED_SIDE - 4) * (PATCHED_SIDE - 4)];
            handle.set_partial(
                PATCH_AT,
                egui::ColorImage::new([PATCHED_SIDE - 4, PATCHED_SIDE - 4], texels),
                egui::TextureOptions::NEAREST,
            );
        }

        ui.painter().image(
            handle.id(),
            PATCHED.egui(),
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    })
}

/// A texture update that arrives after the texture's first upload is applied.
///
/// egui patches its textures — the font atlas grows a glyph at a time — after
/// the whole image has been uploaded once. The patch is written into the
/// texture, so the upload path has to reach the texture itself rather than the
/// view a material samples; reaching for the view finds no texture and the
/// patch is dropped, which leaves whole glyphs missing from the interface.
#[test]
fn a_patched_texture_shows_the_patch() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = ui_only_world(&mut world, &ctx);
    spawn_load_ops(&mut world);

    world.spawn((patched_texture(),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    // The patch covers the middle of the image, which the draw maps to the
    // middle of the rectangle.
    let (x, y) = pixel_of(PATCHED.centre(), 1.0);
    let pixel = frame.pixel_u8(x, y);
    assert!(
        near(pixel, RED, 8),
        "the patched square must be red, got {pixel:?}; a dropped partial \
         update leaves the texture black"
    );
}
