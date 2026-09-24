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

    let rect = Rect::new(40.0, 40.0, 80.0, 60.0);
    world.spawn((paint(rect, RED),));

    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    gpu.render_frames(&world, 2);
    let frame = read(&ctx, &target);

    let (x, y) = pixel_of(rect.centre(), 1.0);
    let centre = frame.pixel_u8(x, y);
    assert!(
        near(centre, RED, 8),
        "the panel should paint its rectangle red at its centre, got {centre:?}"
    );
    assert_eq!(
        count_in(&frame, 0, WIDTH, RED, 8),
        (rect.size.x * rect.size.y) as usize,
        "the panel's rectangle should cover exactly its own area"
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
/// The UI covers the lower band of the frame and the cube sits in the middle,
/// so the two overlap; `ui_first` decides which of them is recorded first.
fn mesh_and_ui_frame(
    ctx: &Ctx,
    world: &mut LocalWorld,
    ui_first: bool,
    panel_color: egui::Color32,
) -> Frame {
    let gpu = mesh_world(ctx, world);
    spawn_load_ops(world);

    let panel = Rect::new(8.0, 96.0, 240.0, 72.0);
    world.spawn((paint(panel, panel_color),));
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

/// A pixel above the panel, where only the cube can be.
const CUBE_PIXEL: (u32, u32) = (128, 60);
/// A pixel inside the panel that the cube also covers.
const OVERLAP_PIXEL: (u32, u32) = (128, 140);
/// A pixel inside the panel that the cube does not cover.
const PANEL_PIXEL: (u32, u32) = (16, 140);

/// A mesh and a UI in one pass: the mesh keeps its own look, the UI covers its
/// own region, and where they overlap the translucent panel blends over the
/// cube.
#[test]
fn mesh_and_ui_in_one_frame() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let frame = mesh_and_ui_frame(&ctx, &mut world, false, TRANSLUCENT_RED);

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
    let ui_on_top = mesh_and_ui_frame(&ctx, &mut ui_last, false, OPAQUE_BLUE);

    let mut ui_first = LocalWorld::new();
    let mesh_on_top = mesh_and_ui_frame(&ctx, &mut ui_first, true, OPAQUE_BLUE);

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
/// A difference means some per-frame state — a cache, a uniform or a scene —
/// leaked from one frame into the next.
#[test]
fn mesh_and_ui_survive_a_second_frame() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = mesh_world(&ctx, &mut world);
    spawn_load_ops(&mut world);

    let panel = Rect::new(8.0, 96.0, 240.0, 72.0);
    world.spawn((paint(panel, TRANSLUCENT_RED),));
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
