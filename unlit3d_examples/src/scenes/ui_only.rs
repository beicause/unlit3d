//! The UI-only scene: egui drawn into a frame with no mesh source and no
//! camera.
//!
//! Ported from the `ui_only_draws_without_a_camera` snapshot test: the frame
//! shape that used to produce nothing, because the renderer bailed out when
//! the world held no camera. The snapshot `ui_only.webp` freezes the second
//! frame — the first only warms egui's font atlas up.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE};
use unlit3d::prelude::*;
use unlit3d::ui::egui;

/// The colour the scene clears to, distinguishable from every panel.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.02,
    b: 0.02,
    a: 1.0,
};

/// The scene: a rich egui interface and nothing else.
pub static SCENE: SceneDef = SceneDef {
    id: "ui_only",
    title: "UI only",
    description: "a rich egui panel, no mesh source and no camera",
    size: TEST_SIZE,
    frames: 2,
    samples: 1,
    depth: false,
    ui: true,
    reproducible_ui: false,
    build,
};

/// Build the UI-only scene's world content.
fn build(
    world: &mut LocalWorld,
    _context: RenderContext,
    _renderer: Entity,
    _size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    // The pass opens with this world's load ops: a dark clear the panels paint
    // over.
    world.spawn((
        Resource,
        RenderLoadOps {
            color: wgpu::LoadOp::Clear(CLEAR_COLOR),
            ..RenderLoadOps::default()
        },
    ));

    if options.ui {
        world.spawn((rich_panel(),));
    }

    SceneControl {
        // The interface is static: egui needs no per-frame advance.
        advance: Box::new(|_world, _frame, _delta| {}),
        // The second frame is the first with the font metrics laid out.
        snapshot: Box::new(|frame| (frame == 1).then(|| "ui_only.webp".to_owned())),
    }
}

/// Where the rich panel puts its parts, in logical points.
pub(crate) const WIDGETS: Rect = Rect::new(8.0, 8.0, 112.0, 128.0);
pub(crate) const PICTURE: Rect = Rect::new(136.0, 8.0, 112.0, 72.0);
pub(crate) const BAND: Rect = Rect::new(8.0, 136.0, 240.0, 40.0);

/// A shade of [`test_image`] the scene's painted circle reuses.
pub(crate) const LIGHT_GREEN: egui::Color32 = egui::Color32::from_rgb(96, 255, 96);

/// A rich, deterministic interface: real widgets, a texture and painted
/// geometry.
///
/// Everything sits at a fixed place, and nothing here reads the clock or reacts
/// to layout feedback, so two frames of the same world come out identical —
/// which is what lets it stand as a snapshot. The blend band is translucent so
/// that it shows the UI blending over whatever was drawn before it rather than
/// replacing it.
///
/// Shared with the mesh-and-UI scene, which draws the same panel over a cube.
pub(crate) fn rich_panel() -> UiPanel {
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
        // path as well as the widget one.
        painter.circle_filled(egui::pos2(190.0, 112.0), 15.0, LIGHT_GREEN);
        painter.circle_stroke(
            egui::pos2(190.0, 112.0),
            15.0,
            egui::Stroke::new(3.0, egui::Color32::WHITE),
        );
        painter.rect_stroke(
            egui::Rect::from_min_size(egui::pos2(212.0, 96.0), egui::Vec2::new(36.0, 32.0)),
            3.0,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 0, 0)),
            egui::StrokeKind::Inside,
        );
        painter.rect_filled(
            BAND.egui(),
            0.0,
            egui::Color32::from_rgba_premultiplied(128, 0, 0, 128),
        );
    })
}

/// The texels of the test image: four quadrants, each checkerboarded between a
/// light and a dark shade.
pub(crate) fn test_image() -> egui::ColorImage {
    const SIZE: usize = 8;
    const QUADRANT: usize = SIZE / 2;

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

/// A rectangle in logical points.
#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) min: egui::Pos2,
    pub(crate) size: egui::Vec2,
}

impl Rect {
    pub(crate) const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            min: egui::Pos2::new(x, y),
            size: egui::Vec2::new(w, h),
        }
    }

    pub(crate) fn egui(self) -> egui::Rect {
        egui::Rect::from_min_size(self.min, self.size)
    }
}
