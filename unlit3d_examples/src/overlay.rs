//! The windowed shell's overlays: the scene selector, the frame-rate readout,
//! the GPU-info panel and the two corner buttons.
//!
//! Every one of them is a UI panel or the free function a panel draws, kept out
//! of the shell proper so the widget trees can be driven against a bare
//! [`egui::Context`] in a test: what they have to get right — where a panel
//! opens, how wide it takes from its facts, whether the corners collide — is a
//! property of the widgets, not of the world.

use std::collections::VecDeque;

use unlit_ecs::bundle;
use unlit3d::prelude::{Entity, UiPanel, World};
use unlit3d::ui::egui;

use crate::scenes;
use crate::{FullscreenActive, FullscreenRequest, SceneSwitch};

/// The scene-selector window's title, which is also its egui area id: the
/// shell reads the window's remembered rectangle by this same id.
pub(crate) const SELECTOR_WINDOW: &str = "scenes";

/// The width the selector window opens at, in points.
///
/// Narrow enough that the list leaves the scene visible behind it: the
/// description wraps onto a few lines instead of the window stretching across
/// the frame. A window a user has dragged keeps the width it was dragged to.
const SELECTOR_DEFAULT_WIDTH: f32 = 220.0;

/// Shift a movable window by however far the screen corner it belongs to moved
/// when the screen changed shape.
///
/// egui applies a window's opening position on the pass that first shows it and
/// afterwards only clamps the remembered rectangle into the screen, so a window
/// seeded into a corner keeps its old spot when the display rotates and the
/// frame becomes a different shape. Shifting it by the corner's movement keeps
/// its distance from that corner — including a distance the user dragged it to —
/// instead of leaving it stranded off its edge.
///
/// The window is anchored to the corner `pivot` names, so its pivot position
/// (the point of the window sitting on that corner) is what moves. The screen
/// the last call saw is remembered under `id`, so the shift is computed once
/// per change and the returned position is applied with
/// [`egui::Window::current_pos`] for that one pass; every other pass returns
/// `None` and the window keeps the position egui remembers for it.
fn follow_screen_corner(
    ctx: &egui::Context,
    id: egui::Id,
    pivot: egui::Align2,
) -> Option<egui::Pos2> {
    let screen = ctx.content_rect();
    let screen_key = id.with("screen");
    let previous = ctx.data(|data| data.get_temp::<egui::Rect>(screen_key));
    ctx.data_mut(|data| {
        data.insert_temp(screen_key, screen);
    });
    let previous = previous?;
    if previous == screen {
        return None;
    }
    // The window has to have been laid out for its position to be known; on its
    // first pass it is seeded by the corner it belongs to, which is where this
    // pass would move it anyway.
    let rect = ctx.memory(|memory| memory.area_rect(id))?;
    let factor = pivot_factor(pivot);
    let moved = screen.lerp_inside(factor) - previous.lerp_inside(factor);
    Some(rect.lerp_inside(factor) + moved)
}

/// The fraction of a window's size at which its pivot sits, per axis.
///
/// The pivot of a window seeded into a corner is that corner, so this is the
/// point of the window — and the matching point of the screen — that stays
/// fixed as the window's size changes.
fn pivot_factor(pivot: egui::Align2) -> egui::Vec2 {
    egui::vec2(pivot.x().to_factor(), pivot.y().to_factor())
}

/// Where the selector window opens when no position was carried over — the
/// first build of a session.
///
/// The window's bottom-right corner sits one overlay margin from the screen's,
/// so the list opens out of the way of the frame-rate readout and the
/// fullscreen button in the top corners. The window is seeded by its
/// bottom-right pivot, so that corner stays put as the content decides the
/// window's size; a window a user has dragged is carried by its whole
/// rectangle, which a rebuild restores exactly.
fn selector_default_pos(ctx: &egui::Context) -> egui::Pos2 {
    ctx.content_rect().right_bottom() - egui::Vec2::splat(OVERLAY_MARGIN)
}

/// Mount the windowed shell's scene-selector panel.
///
/// The panel lists every scene and writes its choice into the [`SceneSwitch`]
/// component, which the frame loop reads once after the frame's advance.
pub(crate) fn mount_selector(
    world: &mut World,
    ui: Entity,
    switch: Entity,
    current: &'static scenes::SceneDef,
    initial: Option<egui::Rect>,
) {
    world
        .scope_mut(ui)
        .expect("the UI source is alive")
        .spawn(bundle![UiPanel::new(move |world, _entity, ui| {
            // The id is set explicitly: `Window::new` derives it from the title's
            // `Atoms` text, whose hash differs from a plain string id, so the
            // shell's read of the remembered rect would never match.
            let id = egui::Id::unique(SELECTOR_WINDOW);
            // The window belongs to the bottom-right corner. A fresh one is seeded
            // by that corner, so the corner stays put while the first layout
            // measures the content; a carried rectangle is seeded by its own
            // bottom-right and given its size, so the first frame lays it out
            // exactly where the previous scene left it. `follow_screen_corner`
            // keeps the corner when the screen changes shape.
            let pivot = egui::Align2::RIGHT_BOTTOM;
            let window = egui::Window::new(SELECTOR_WINDOW).id(id).pivot(pivot);
            let window = match initial {
                Some(rect) => window
                    .default_pos(rect.lerp_inside(pivot_factor(pivot)))
                    .default_size(rect.size()),
                None => window
                    .default_width(SELECTOR_DEFAULT_WIDTH)
                    .default_pos(selector_default_pos(ui.ctx())),
            };
            // A titled window that is movable discards `current_pos`: egui
            // re-applies the pivot it had stored before the area began, so the
            // position given here would not stick. The reposition pass therefore
            // also turns the window immovable; every other pass is movable again
            // and a user's drag works as before.
            let window = match follow_screen_corner(ui.ctx(), id, pivot) {
                Some(pos) => window.current_pos(pos).movable(false),
                None => window,
            };
            window.show(ui.ctx(), |ui| {
                ui.label(format!("{} — {}", current.title, current.description));
                ui.separator();
                for scene in scenes::SCENES.iter().copied() {
                    let selected = scene.id == current.id;
                    if ui.selectable_label(selected, scene.title).clicked() {
                        let _ = world
                            .query_one::<&mut SceneSwitch>(switch)
                            .map(|mut s| s.get().0 = Some(scene));
                    }
                }
            });
        },),]);
}

/// The frame rate the windowed shell reports, as a component so the panel that
/// displays it and the loop that measures it share one value.
///
/// It lives in the world like any other behaviour state rather than being
/// captured by the panel's closure: a panel may run several times in one frame
/// when egui redoes a layout, and a panel must not reach for its own
/// [`UiPanel`], so the measurement belongs in a sibling component.
#[derive(Default)]
pub(crate) struct FrameRate {
    /// The frames measured so far, capped at [`FrameRate::WINDOW`].
    ///
    /// A sliding window rather than a running mean: a scene that stalls once
    /// should not drag the reading down for the rest of the session, and a rate
    /// that has just changed should show up within a few frames.
    samples: VecDeque<f32>,
}

impl FrameRate {
    /// How many frames the reading averages over.
    ///
    /// About a second at 60 Hz: long enough that the number does not flicker
    /// with every frame, short enough that a change is visible promptly.
    const WINDOW: usize = 60;

    /// Record one frame's duration, in seconds.
    pub(crate) fn push(&mut self, delta: f32) {
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(delta);
    }

    /// The mean frame time over the window, in seconds, or `None` before any
    /// frame was measured.
    fn mean_seconds(&self) -> Option<f32> {
        if self.samples.is_empty() {
            return None;
        }
        let total: f32 = self.samples.iter().sum();
        Some(total / self.samples.len() as f32)
    }

    /// The reading as `"60.0 fps · 16.7 ms"`.
    ///
    /// The frame time is shown next to the rate because the two answer
    /// different questions: the rate is what a display is judged against, while
    /// the milliseconds are what a budget is spent in.
    fn text(&self) -> String {
        match self.mean_seconds() {
            Some(seconds) if seconds > 0.0 => {
                format!("{:.1} fps · {:.1} ms", 1.0 / seconds, seconds * 1e3)
            }
            // The first frame has no previous one to measure against.
            _ => "— fps".to_owned(),
        }
    }
}

/// The distance every overlay keeps from the edge of the screen.
///
/// Each corner overlay is anchored to its edge with this offset, so none of
/// them is drawn flush against the boundary — a readout or a button touching
/// the very edge of a phone's display reads as clipped, and the safe-area
/// insets the page already applies to the canvas are not visible to egui.
const OVERLAY_MARGIN: f32 = 8.0;

/// The gap between the frame-rate readout and the fullscreen button when a
/// window is too narrow to hold them on the same row.
const OVERLAY_GAP: f32 = 8.0;

/// The smallest a corner button is drawn, in points.
///
/// A corner button is an overlay on the scene, so it is kept small enough to
/// read as one — a control as tall as a panel's row competes with the frame it
/// annotates — while still being a comfortable target to click.
const OVERLAY_BUTTON_MIN_SIZE: egui::Vec2 = egui::Vec2::new(110.0, 32.0);

/// A corner button's inner padding, symmetric so the label is centred within
/// the button it is grown to.
///
/// A bare [`egui::Button::min_size`] would leave the label sitting in a corner:
/// the button's atoms are aligned by the surrounding layout, which is not
/// centred. Growing the padding instead — the same on every side — is what
/// pushes the label to the middle of the larger button.
const OVERLAY_BUTTON_PADDING: egui::Vec2 = egui::Vec2::new(10.0, 7.0);

/// The API docs the Pages workflow publishes beside the example page.
///
/// Absolute, because the browser and Android only open an `http(s)` URL — a
/// path relative to the page is not something either hands to a browser. The
/// site is the repository's, so a fork that publishes its own should point this
/// at its own Pages URL.
const API_DOCS_URL: &str = "https://beicause.github.io/unlit3d/docs/index.html";

/// The label the API-docs button carries.
const API_DOCS_LABEL: &str = "API docs";

/// The id of the frame-rate readout's area.
const FRAME_RATE_AREA: &str = "unlit3d::frame-rate";

/// The id of the fullscreen button's area.
const FULLSCREEN_AREA: &str = "unlit3d::fullscreen";

/// The id of the API-docs button's area.
const API_DOCS_AREA: &str = "unlit3d::api-docs";

/// The GPU-info panel's title, shown in its title bar.
const GPU_INFO_TITLE: &str = "GPU";

/// The id of the GPU-info panel's area, which is also the id egui remembers
/// the panel's rectangle under.
pub(crate) const GPU_INFO_AREA: &str = "unlit3d::gpu-info";

/// The narrowest the GPU-info panel may be dragged, in points.
///
/// A fresh panel opens as wide as its widest fact needs to stay on one line,
/// so this floor is only reached by dragging. Without it a drag could squeeze
/// the panel down to a column of one letter: the facts are wrapped text in a
/// scroll area, so they follow the panel all the way down.
const GPU_INFO_MIN_WIDTH: f32 = 160.0;

/// Slack added to the GPU-info panel's measured width.
///
/// A label wraps on a strict comparison against the width it is handed, and a
/// window rounds its size to a whole pixel, so a width that fits a fact exactly
/// can still come out a hair too narrow. A couple of points keep the longest
/// fact on its line when the rounding would otherwise break it.
const GPU_INFO_WIDTH_SLACK: f32 = 2.0;

/// The share of the window's width the GPU-info panel may take, at most.
///
/// The panel takes its opening width from its content, and a device can name a
/// limit that is longer than the window is wide. This keeps such a panel from
/// hanging off the edge and leaves the scene visible beside it; a name that
/// still does not fit wraps, and the panel may be dragged wider by hand.
const GPU_INFO_MAX_WIDTH_FRACTION: f32 = 0.85;

/// The share of the window's height the GPU-info panel may take.
///
/// The panel is an overlay on the scene, not a replacement for it: the limits
/// alone are several dozen rows, so the body scrolls within this share of the
/// window instead of covering it.
const GPU_INFO_HEIGHT_FRACTION: f32 = 0.6;

/// Mount the frame-rate readout at the top of the window, centred.
///
/// An [`egui::Area`] rather than a window: it is pinned, not something the user
/// can drag away or accidentally resize, and `interactable(false)` keeps it
/// from swallowing a click meant for the scene underneath it.
pub(crate) fn mount_frame_rate(world: &mut World, ui: Entity, rate: Entity) {
    world
        .scope_mut(ui)
        .expect("the UI source is alive")
        .spawn(bundle![UiPanel::new(move |world, _entity, ui| {
            let text = world
                .query_one::<&FrameRate>(rate)
                .map(|mut rate| rate.get().text())
                .expect("the frame-rate component exists");
            frame_rate_readout(ui, &text);
        },),]);
}

/// Draw the frame-rate readout, showing `text`, centred along the top edge of
/// `ui`'s screen.
///
/// The label is set to [`egui::Label::extend`], so the readout grows to fit its
/// text instead of wrapping it. An anchored area lays its contents out against
/// the size it remembered from the previous pass, so a reading that is wider
/// than the last one — the first real reading after the empty placeholder, or
/// any reading after the rate changes — would otherwise be broken across lines
/// in a narrow window.
///
/// A free function rather than the body of the panel's closure so a test can
/// drive it against a bare [`egui::Context`]: what it has to get right — one
/// line, whatever the previous reading was — is a property of the widget tree,
/// not of the world.
fn frame_rate_readout(ui: &mut egui::Ui, text: &str) {
    egui::Area::new(egui::Id::unique(FRAME_RATE_AREA))
        .anchor(egui::Align2::CENTER_TOP, [0.0, OVERLAY_MARGIN])
        .interactable(false)
        // The readout is an overlay, not a window: a scene panel dragged over
        // the same corner must not cover it, so it sits in the layer above the
        // one windows are drawn in.
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text).monospace()).extend());
            });
        });
}

/// The device facts the GPU-info panel shows, read once when it is mounted.
///
/// Reading them once rather than every frame keeps the panel's per-frame pass
/// free of work the device cannot change: an adapter's name, the features it
/// was created with and the limits it reports are fixed for the lifetime of
/// the device, so the strings are built where the device is in hand and the
/// pass only lays them out.
struct GpuInfo {
    /// The adapter's name, shown as the panel's title.
    name: String,
    /// One line naming the backend, the kind of device and the driver.
    summary: String,
    /// The rest of the adapter's description, as label/value pairs.
    rows: Vec<(String, String)>,
    /// The names of the features the device was created with.
    features: Vec<&'static str>,
    /// The limits the device reports, as field-name/value pairs.
    limits: Vec<(String, String)>,
}

impl GpuInfo {
    /// Read `device`'s adapter description, enabled features and limits.
    fn read(device: &wgpu::Device) -> Self {
        let info = device.adapter_info();
        let features = device.features();
        let limits = device.limits();

        // The field names are the panel's labels, so they are spelled once and
        // `stringify!`d rather than repeated as string literals that could drift
        // from the fields they name.
        Self {
            name: info.name.clone(),
            summary: format!(
                "{} · {} · {}",
                info.backend.to_str(),
                device_type_name(info.device_type),
                info.driver,
            ),
            rows: vec![
                ("driver info".to_owned(), info.driver_info.clone()),
                ("vendor".to_owned(), format!("{:#06x}", info.vendor)),
                ("device".to_owned(), format!("{:#06x}", info.device)),
                ("pci bus".to_owned(), info.device_pci_bus_id.clone()),
                (
                    "subgroup".to_owned(),
                    format!("{}..{}", info.subgroup_min_size, info.subgroup_max_size),
                ),
                (
                    "transient saves memory".to_owned(),
                    match info.transient_saves_memory {
                        Some(saves) => saves.to_string(),
                        None => "unknown".to_owned(),
                    },
                ),
            ],
            features: features.iter_names().map(|(name, _)| name).collect(),
            limits: limit_rows(&limits),
        }
    }
}

/// The device's limits as label/value pairs, in the order the device reports
/// them.
///
/// A [`wgpu::Limits`] is serialisable, so the labels and values come from its
/// own serde description: the field names are the JSON object's keys and the
/// values are its entries. Nothing here names a limit, so a field wgpu adds is
/// shown without a change, and a field it renames is not silently dropped.
///
/// The keys come out in the JSON object's order, which serde_json keeps sorted
/// rather than in declaration order; the list is stable and complete either
/// way, and a reader looking a limit up benefits from the order.
///
/// Falls back to an empty list rather than failing: a panel that loses its
/// limits is a cosmetic loss, and serialising a plain struct of integers has
/// no way to fail.
fn limit_rows(limits: &wgpu::Limits) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(limits) else {
        return Vec::new();
    };
    fields
        .into_iter()
        .map(|(name, value)| (name, json_scalar(&value)))
        .collect()
}

/// One limit's value as text.
///
/// Every field of a [`wgpu::Limits`] is an integer, so a scalar is all there
/// is to render; the fallback covers a future field of another shape without
/// hiding it.
fn json_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// The name of a [`wgpu::DeviceType`], for the panel's summary line.
///
/// [`wgpu::DeviceType`] has no `Display` impl, so its names are spelled here.
fn device_type_name(device_type: wgpu::DeviceType) -> &'static str {
    match device_type {
        wgpu::DeviceType::Other => "other",
        wgpu::DeviceType::IntegratedGpu => "integrated GPU",
        wgpu::DeviceType::DiscreteGpu => "discrete GPU",
        wgpu::DeviceType::VirtualGpu => "virtual GPU",
        wgpu::DeviceType::Cpu => "CPU",
    }
}

/// Where the GPU-info panel opens when no position was carried over.
///
/// Like the selector, it is seeded by the corner it belongs in — here the
/// bottom-left, one overlay margin in — so that corner stays put as the
/// content decides the panel's size; a panel the user has dragged is carried
/// by its whole rectangle, which a rebuild restores exactly.
fn gpu_info_default_pos(ctx: &egui::Context) -> egui::Pos2 {
    ctx.content_rect().left_bottom() + egui::Vec2::new(OVERLAY_MARGIN, -OVERLAY_MARGIN)
}

/// Mount the GPU-info panel, a draggable window in the bottom-left corner.
///
/// A window rather than an anchored area so the user can move it out of the
/// way. It is an ordinary window, in the layer the scene's own windows are
/// drawn in: the title bar then highlights exactly when it is the window on
/// top, the same as the selector's, and a scene panel dragged over it covers
/// it the same way.
///
/// The device is read here, once, where it is in hand; the panel then draws
/// from the strings that read produced.
pub(crate) fn mount_gpu_info(
    world: &mut World,
    ui: Entity,
    device: wgpu::Device,
    initial: Option<egui::Rect>,
) {
    let info = GpuInfo::read(&device);
    world
        .scope_mut(ui)
        .expect("the UI source is alive")
        .spawn(bundle![UiPanel::new(move |_world, _entity, ui| {
            gpu_info_panel(ui, &info, initial);
        },),]);
}

/// One line of the GPU-info panel.
///
/// The panel sizes itself from these lines and then draws the same values, so a
/// line's wording and the font it is drawn in are chosen once: a width measured
/// from a differently-styled string would not be the width the panel draws with.
enum GpuInfoLine {
    /// The adapter's name or its one-line description, above the separator.
    Header(egui::WidgetText),
    /// A heading that opens a group of facts, set off by a gap above it.
    Heading(egui::WidgetText),
    /// One fact.
    Fact(egui::WidgetText),
}

impl GpuInfoLine {
    /// The line's text and styling, as it is both measured and drawn.
    fn text(&self) -> &egui::WidgetText {
        match self {
            Self::Header(text) | Self::Heading(text) | Self::Fact(text) => text,
        }
    }

    /// Whether the line is part of the header, above the separating rule.
    fn is_header(&self) -> bool {
        matches!(self, Self::Header(_))
    }

    /// Whether the line opens a section, which is set off by a gap above it.
    fn is_heading(&self) -> bool {
        matches!(self, Self::Heading(_))
    }
}

/// The lines the GPU-info panel shows, top to bottom.
///
/// The wording lives here and nowhere else: the width measurement and the
/// drawing both walk this list, so a fact cannot be measured as one string and
/// drawn as another, nor measured in one font and drawn in another.
fn gpu_info_lines(info: &GpuInfo) -> Vec<GpuInfoLine> {
    let mut lines = Vec::new();
    lines.push(GpuInfoLine::Header(
        egui::RichText::new(&info.name).strong().into(),
    ));
    lines.push(GpuInfoLine::Header(
        egui::RichText::new(&info.summary).monospace().into(),
    ));
    lines.push(GpuInfoLine::Heading(
        egui::RichText::new("Adapter").strong().into(),
    ));
    for (label, value) in &info.rows {
        lines.push(GpuInfoLine::Fact(format!("{label}: {value}").into()));
    }
    lines.push(GpuInfoLine::Heading(
        egui::RichText::new(format!("Features ({})", info.features.len()))
            .strong()
            .into(),
    ));
    if info.features.is_empty() {
        lines.push(GpuInfoLine::Fact("none".into()));
    } else {
        for feature in &info.features {
            lines.push(GpuInfoLine::Fact((*feature).into()));
        }
    }
    lines.push(GpuInfoLine::Heading(
        egui::RichText::new(format!("Limits ({})", info.limits.len()))
            .strong()
            .into(),
    ));
    for (name, value) in &info.limits {
        lines.push(GpuInfoLine::Fact(format!("{name} = {value}").into()));
    }
    lines
}

/// How wide the GPU-info panel has to be to keep every line on one row.
///
/// The facts are strings the device chose, and its own names for its limits run
/// past fifty characters, so a fixed opening width would wrap them. Each line is
/// laid out unwrapped as it will be drawn, the widest one sets the content
/// width, and the window's frame and the body's scroll bar are added back on:
/// the panel's size, unlike the text's, includes both.
///
/// A free function rather than part of the panel so a test can ask what width a
/// given set of lines needs without drawing a pass.
fn gpu_info_width(ui: &egui::Ui, lines: &[GpuInfoLine]) -> f32 {
    let mut widest: f32 = 0.0;
    for line in lines {
        let width = line
            .text()
            .clone()
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                0.0,
                egui::FontSelection::Default,
            )
            .size()
            .x;
        widest = widest.max(width);
    }
    let style = ui.style();
    let frame = egui::Frame::window(style).total_margin().sum().x;
    let scroll_bar = style.spacing.scroll.allocated_width();
    widest + frame + scroll_bar + GPU_INFO_WIDTH_SLACK
}

/// Draw the GPU-info panel for `info`, seeded at `initial` or the bottom-left
/// corner of `ui`'s screen.
///
/// A free function rather than the body of the panel's closure so a test can
/// drive it against a bare [`egui::Context`]: what it has to get right — where
/// it opens, the width it takes from its facts and the facts it prints — is a
/// property of the widget tree, not of the world.
fn gpu_info_panel(ui: &mut egui::Ui, info: &GpuInfo, initial: Option<egui::Rect>) {
    // The id is set explicitly, as in the selector: `Window::new` would derive
    // it from the title text, and the shell reads the remembered rectangle back
    // under this id.
    //
    // The panel is resizable in both directions, like the selector and the
    // scene's own windows: the limits alone are several dozen rows, and how
    // much of the scene the panel covers while they are read is the reader's
    // to decide. It opens as wide as its facts need, and dragging it narrower
    // is what wraps them.
    let id = egui::Id::unique(GPU_INFO_AREA);
    // The panel belongs to the bottom-left corner, so it is seeded and kept by
    // that corner: a fresh one opens there, a carried rectangle is seeded by its
    // own bottom-left and given its size, and `follow_screen_corner` moves it
    // when the screen changes shape.
    let pivot = egui::Align2::LEFT_BOTTOM;
    let window = egui::Window::new(GPU_INFO_TITLE)
        .id(id)
        .pivot(pivot)
        .min_width(GPU_INFO_MIN_WIDTH)
        .max_width(ui.ctx().content_rect().width() * GPU_INFO_MAX_WIDTH_FRACTION);
    let lines = gpu_info_lines(info);
    // The lines above the rule are the header; the rest are the facts, which
    // scroll, so the two runs are drawn into different places from the one
    // list.
    let header = lines.iter().take_while(|line| line.is_header()).count();
    let window = match initial {
        Some(rect) => window
            .default_pos(rect.lerp_inside(pivot_factor(pivot)))
            .default_size(rect.size()),
        None => window
            // The size is given as well as the position: an area is laid out
            // against the size it had before the first pass measures it, and
            // egui's default area size is wider than a narrow window, which
            // would push the seeded corner inward when the area is constrained
            // to the screen.
            .default_size(egui::Vec2::new(
                gpu_info_width(ui, &lines),
                ui.ctx().content_rect().height() * GPU_INFO_HEIGHT_FRACTION,
            ))
            .default_pos(gpu_info_default_pos(ui.ctx())),
    };
    // A titled window that is movable discards `current_pos`: egui re-applies
    // the pivot it had stored before the area began, so the position given here
    // would not stick. The reposition pass therefore also turns the window
    // immovable; every other pass is movable again and a user's drag works as
    // before.
    let window = match follow_screen_corner(ui.ctx(), id, pivot) {
        Some(pos) => window.current_pos(pos).movable(false),
        None => window,
    };
    window.show(ui.ctx(), |ui| {
        let (header_lines, body_lines) = lines.split_at(header);
        ui.vertical(|ui| {
            for line in header_lines {
                ui.label(line.text().clone());
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(ui.ctx().content_rect().height() * GPU_INFO_HEIGHT_FRACTION)
                // Fill the panel's width so the facts wrap within it instead of
                // the panel sizing itself to the widest one; shrink to the
                // content's height so a device with few limits gets a short
                // panel, and dragging it taller then does not pad it out with
                // empty space.
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for line in body_lines {
                        if line.is_heading() {
                            ui.add_space(OVERLAY_GAP);
                        }
                        ui.label(line.text().clone());
                    }
                });
        });
    });
}

/// Mount the fullscreen button in the window's top-right corner.
///
/// A button rather than the old "the first press asks for fullscreen": a press
/// on the canvas is not discoverable — a phone shows no hint that it would
/// work, and nothing tells a user who left fullscreen that they may ask again —
/// whereas a button is visible on every platform and keeps working after the
/// browser has left fullscreen. Nothing about the request is latched, so the
/// display can be entered and left as often as the button is pressed.
///
/// The click is recorded rather than served here: a panel runs while the world
/// is borrowed, and it has no window to ask; the frame loop reads the request
/// and calls [`crate::fullscreen::toggle`], which is where a browser accepts it
/// from.
pub(crate) fn mount_fullscreen(world: &mut World, ui: Entity, request: Entity, state: Entity) {
    world
        .scope_mut(ui)
        .expect("the UI source is alive")
        .spawn(bundle![UiPanel::new(move |world, _entity, ui| {
            let active = world
                .query_one::<&FullscreenActive>(state)
                .is_ok_and(|mut state| state.get().0);
            if fullscreen_button(ui, active) {
                let _ = world
                    .query_one::<&mut FullscreenRequest>(request)
                    .map(|mut request| request.get().0 = true);
            }
        },),]);
}

/// Mount the link to the API docs in the window's top-right corner.
///
/// The link lives in the UI rather than in the page so the two corner overlays
/// are laid out by the same code and cannot collide: the page's own link knew
/// nothing about the button, which is what put the two on top of each other.
/// Mounted after [`mount_fullscreen`], because the link is placed under the
/// button it measures.
///
/// The click is recorded rather than served here: a panel runs while the world
/// is borrowed and has no page to reach; the frame loop reads the request and
/// opens it with [`webbrowser`], which covers every platform the example runs
/// on.
pub(crate) fn mount_api_docs(world: &mut World, ui: Entity) {
    world
        .scope_mut(ui)
        .expect("the UI source is alive")
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            api_docs_button(ui);
        }),]);
}

/// The vertical offset a corner button's anchor is drawn at, given the area
/// above it in the same corner.
///
/// `above` and the button share the top row when the window is wide enough for
/// both to sit side by side. When it is not — the frame-rate readout is
/// centred, so on a portrait phone it reaches into the corner the fullscreen
/// button anchors to — the button drops to the row under `above` instead:
/// hiding the end of the reading under the button that is meant to sit beside
/// it on a roomier window does not help. The fallback keeps both legible and
/// separated at any width.
///
/// Both areas are anchored, and an anchored area is laid out against the
/// rectangle the previous pass remembered for it — which is what is compared
/// here, so the decision is made about the very positions egui will use.
fn overlay_button_top(ctx: &egui::Context, above: &str, id: &str) -> f32 {
    let above = ctx.memory(|memory| memory.area_rect(egui::Id::unique(above)));
    let button = ctx.memory(|memory| memory.area_rect(egui::Id::unique(id)));
    match (above, button) {
        // The area above stops clear of the button's column.
        (Some(above), Some(button)) if above.right() + OVERLAY_GAP < button.left() => {
            OVERLAY_MARGIN
        }
        // The area above reaches into the button's corner: stack beneath it.
        (Some(above), _) => above.bottom() + OVERLAY_GAP,
        // The button is the first thing drawn; the top row is where it belongs.
        (None, _) => OVERLAY_MARGIN,
    }
}

/// Draw a button that owns a corner, and report whether it was pressed.
///
/// `id` names the area, so the button can be read back — the docs link is
/// placed under the fullscreen button by measuring it — and so two corners can
/// be tested apart. A free function rather than the body of a panel's closure
/// so a test can drive it against a bare [`egui::Context`].
fn corner_button(ui: &mut egui::Ui, id: &str, top: f32, label: egui::RichText) -> bool {
    egui::Area::new(egui::Id::unique(id))
        .anchor(egui::Align2::RIGHT_TOP, [-OVERLAY_MARGIN, top])
        // A corner button is an overlay, not a window: a scene panel dragged
        // over the corner must not cover it, so it sits in the layer above the
        // one windows are drawn in.
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .show(ui, |ui| {
                    ui.style_mut().spacing.button_padding = OVERLAY_BUTTON_PADDING;
                    ui.add(
                        egui::Button::new(label)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .min_size(OVERLAY_BUTTON_MIN_SIZE),
                    )
                    .clicked()
                })
                .inner
        })
        .inner
}

/// Draw the fullscreen button, labelled for what pressing it does, and report
/// whether it was pressed.
///
/// The label follows the document rather than a local flag: the user can leave
/// fullscreen with the browser's own control — Escape, or the back gesture on a
/// phone — and the button has to offer to go back in afterwards. That is also
/// why it owns its corner rather than sharing a row with the readout: each
/// anchored area keeps to itself, and the button is only drawn on top of the
/// readout's line when the two would otherwise collide.
///
/// The glyphs are drawn from the default font — ⛶ to enter and ⏏ to leave —
/// with a word beside each so the meaning does not rest on a symbol alone. Both
/// labels are laid out to extend rather than wrap: the exit label is the wider
/// of the two, and an anchored area lays its contents out against the size it
/// remembered from the previous pass, so switching to it would otherwise break
/// it across two lines in the narrower area the enter label had established.
fn fullscreen_button(ui: &mut egui::Ui, active: bool) -> bool {
    let label = if active {
        "⏏ Exit fullscreen"
    } else {
        "⛶ Fullscreen"
    };
    let top = overlay_button_top(ui.ctx(), FRAME_RATE_AREA, FULLSCREEN_AREA);
    corner_button(
        ui,
        FULLSCREEN_AREA,
        top,
        egui::RichText::new(label).monospace(),
    )
}

/// Draw the API-docs link under the fullscreen button, and report whether it
/// was pressed.
///
/// The click becomes egui's own URL command, which the UI source publishes in
/// the world for the frame loop to serve, rather than opening the page here: a
/// panel runs while the world is borrowed and cannot wait on one.
fn api_docs_button(ui: &mut egui::Ui) -> bool {
    let top = overlay_button_top(ui.ctx(), FULLSCREEN_AREA, API_DOCS_AREA);
    let label = egui::RichText::new(API_DOCS_LABEL).monospace();
    let clicked = corner_button(ui, API_DOCS_AREA, top, label);
    if clicked {
        ui.ctx().open_url(egui::OpenUrl::same_tab(API_DOCS_URL));
    }
    clicked
}

#[cfg(test)]
mod tests;
