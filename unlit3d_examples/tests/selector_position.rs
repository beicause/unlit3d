//! The floating panels keep their dragged rectangles across a scene switch and
//! follow the corners they belong to when the screen changes shape.
//!
//! A scene switch rebuilds the whole scene — world, renderer, and the egui
//! context the window's position lives in — so the shell reads each window's
//! last rectangle before dropping the old scene and hands it to the new one as
//! its opening position. These tests pin both halves of that hand-off, for the
//! scene selector and for the GPU-info panel, and the re-seating that a rotated
//! frame asks of a corner panel.
//!
//! The file uses the harness `gpu_scenes.rs` uses, so the same binary runs
//! natively and as the module a wasm test page loads.

use unlit_wgpu_test_util::{Ctx, gpu_tests};
use unlit3d::prelude::*;
use unlit3d::ui::egui;
use unlit3d_examples::scenes::{self, SceneOptions};
use unlit3d_examples::{FIXED_STEP, PanelRects, Scene};

async fn selector_window_is_laid_out_after_a_frame() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);
    let (mut world, mut scene) = scene(&ctx, size, None);
    bind(&mut world, &mut scene, &ctx, size);

    scene.advance(&mut world, FIXED_STEP);
    scene.render(&world);
    scene.end_frame(&mut world).unwrap();

    let laid_out = rect(&world);
    assert!(
        laid_out.is_some(),
        "the selector must be laid out after a frame"
    );
}

async fn selector_window_opens_where_the_previous_scene_left_it() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);

    // Lay the selector out once, so the rectangle carried below is one it can
    // hold. egui grows a window past a `default_size` its content outgrows, and
    // a carried rectangle smaller than that content could not be restored
    // exactly — a fixed size here would silently assume how many scenes the
    // table holds. The position is a spot a user might have dragged it to.
    let (mut first_world, mut first) = scene(&ctx, size, None);
    bind(&mut first_world, &mut first, &ctx, size);
    first.advance(&mut first_world, FIXED_STEP);
    first.render(&first_world);
    first.end_frame(&mut first_world).unwrap();
    let laid_out = rect(&first_world).expect("the selector is laid out after a frame");

    // The rectangle the shell would read out of the dying scene's egui context.
    let carried = Some(egui::Rect::from_min_size(
        egui::pos2(100.0, 50.0),
        laid_out.size(),
    ));
    let (mut world, mut scene) = scene(&ctx, size, carried);
    bind(&mut world, &mut scene, &ctx, size);

    scene.advance(&mut world, FIXED_STEP);
    scene.render(&world);
    scene.end_frame(&mut world).unwrap();

    let opened = rect(&world);
    assert_eq!(
        opened, carried,
        "rebuild must inherit the carried rectangle"
    );
}

async fn the_gpu_info_panel_opens_where_the_previous_scene_left_it() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);

    // A rectangle a user might have dragged the panel to.
    let carried = egui::Rect::from_min_size(egui::pos2(120.0, 80.0), egui::vec2(300.0, 260.0));
    let (mut world, mut scene) = scene_with_gpu_info(&ctx, size, Some(carried));
    bind(&mut world, &mut scene, &ctx, size);

    scene.advance(&mut world, FIXED_STEP);
    scene.render(&world);
    scene.end_frame(&mut world).unwrap();

    let opened = gpu_info_rect(&world).expect("the GPU-info panel is laid out after a frame");
    assert_eq!(
        opened.min, carried.min,
        "rebuild must inherit the carried rectangle's position"
    );
}

async fn a_switched_scene_opens_where_the_selector_was_left() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);

    for (a, b) in [(0usize, 7usize), (7, 0)] {
        let (mut first_world, mut first) = scene_at(&ctx, size, None, scenes::SCENES[a]);
        bind(&mut first_world, &mut first, &ctx, size);
        for _ in 0..4 {
            first.advance(&mut first_world, FIXED_STEP);
            first.render(&first_world);
            first.end_frame(&mut first_world).unwrap();
        }
        let opened = rect(&first_world).expect("laid out");

        let (mut second_world, mut second) = scene_at(&ctx, size, Some(opened), scenes::SCENES[b]);
        bind(&mut second_world, &mut second, &ctx, size);
        for _ in 0..4 {
            second.advance(&mut second_world, FIXED_STEP);
            second.render(&second_world);
            second.end_frame(&mut second_world).unwrap();
        }
        let rebuilt = rect(&second_world).expect("laid out");
        assert_eq!(
            rebuilt.min, opened.min,
            "the rebuilt selector must keep the position the first scene left"
        );
    }
}

async fn the_selector_re_seats_in_its_corner_when_the_screen_changes_shape() {
    let ctx = Ctx::headless().await;
    let (mut world, mut scene) = scene(&ctx, (960, 720), None);
    bind(&mut world, &mut scene, &ctx, (960, 720));
    for _ in 0..2 {
        scene.advance(&mut world, FIXED_STEP);
        scene.render(&world);
        scene.end_frame(&mut world).unwrap();
    }

    // How far the selector's bottom-right corner sits from the frame's, which
    // the panel keeps as its margin from the screen edge.
    let opened = rect(&world).expect("the selector is laid out");
    let margin = egui::vec2(960.0, 720.0) - opened.max.to_vec2();

    // Rotating a phone swaps the frame's width and height; rebinding the
    // offscreen target at the new size is what a window Resized does to the
    // size egui lays its panels out against.
    bind(&mut world, &mut scene, &ctx, (720, 960));
    for _ in 0..2 {
        scene.advance(&mut world, FIXED_STEP);
        scene.render(&world);
        scene.end_frame(&mut world).unwrap();
    }

    let rotated = rect(&world).expect("the selector is laid out");
    assert!(
        (rotated.max.to_vec2() - (egui::vec2(720.0, 960.0) - margin)).length() < 1.0,
        "the selector left its corner when the screen changed shape: {rotated:?}"
    );
}

fn rect(world: &World) -> Option<egui::Rect> {
    panel_rect(world, "scenes")
}

/// The carried rectangles for a scene whose selector opens at `initial`.
fn selector_only(initial: Option<egui::Rect>) -> PanelRects {
    PanelRects {
        selector: initial,
        gpu_info: None,
    }
}

/// The carried rectangles for a scene whose GPU-info panel opens at `gpu_info`.
fn gpu_info_only(gpu_info: Option<egui::Rect>) -> PanelRects {
    PanelRects {
        selector: None,
        gpu_info,
    }
}

/// The GPU-info panel's rectangle, read by the id the panel's window is given.
fn gpu_info_rect(world: &World) -> Option<egui::Rect> {
    panel_rect(world, "unlit3d::gpu-info")
}

/// The rectangle egui remembers for the window with `id`.
fn panel_rect(world: &World, id: &str) -> Option<egui::Rect> {
    world
        .root_scope()
        .query::<(Entity, &Source)>()
        .iter()
        .find_map(|(_, source)| {
            let ui = source.as_ref::<UiSource>()?;
            ui.context()
                .memory(|memory| memory.area_rect(egui::Id::new(id)))
        })
}

fn scene_at(
    ctx: &Ctx,
    size: (u32, u32),
    initial: Option<egui::Rect>,
    def: &'static scenes::SceneDef,
) -> (World, Scene) {
    Scene::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        ctx.capabilities,
        size,
        SceneOptions {
            ui: true,
            selector: true,
            reproducible: true,
            letterbox: false,
            sequence_step: None,
        },
        def,
        selector_only(initial),
    )
}

/// A scene whose GPU-info panel opens at `gpu_info`.
fn scene_with_gpu_info(
    ctx: &Ctx,
    size: (u32, u32),
    gpu_info: Option<egui::Rect>,
) -> (World, Scene) {
    Scene::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        ctx.capabilities,
        size,
        SceneOptions {
            ui: true,
            selector: true,
            reproducible: true,
            letterbox: false,
            sequence_step: None,
        },
        scenes::SCENES[0],
        gpu_info_only(gpu_info),
    )
}

/// A scene with the selector mounted, opened at `initial`.
fn scene(ctx: &Ctx, size: (u32, u32), initial: Option<egui::Rect>) -> (World, Scene) {
    Scene::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        ctx.capabilities,
        size,
        SceneOptions {
            ui: true,
            selector: true,
            reproducible: true,
            letterbox: false,
            sequence_step: None,
        },
        scenes::SCENES[0],
        selector_only(initial),
    )
}

/// Bind an offscreen target the way the snapshot tests do, so `render` draws.
fn bind(world: &mut World, scene: &mut Scene, ctx: &Ctx, size: (u32, u32)) {
    let context = world
        .root_scope()
        .query_one::<&Renderer>(scene.renderer)
        .expect("the renderer is a resource entity")
        .context();
    let _ = unlit3d_examples::bind_offscreen_target(
        world,
        context,
        scene.pass,
        &ctx.device,
        size,
        1,
        false,
    );
}

gpu_tests! {
    selector_window_is_laid_out_after_a_frame,
    selector_window_opens_where_the_previous_scene_left_it,
    the_gpu_info_panel_opens_where_the_previous_scene_left_it,
    a_switched_scene_opens_where_the_selector_was_left,
    the_selector_re_seats_in_its_corner_when_the_screen_changes_shape,
}
