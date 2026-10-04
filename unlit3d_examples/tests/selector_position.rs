//! The scene-selector window keeps its dragged rectangle across a scene
//! switch.
//!
//! A scene switch rebuilds the whole scene — world, renderer, and the egui
//! context the window's position lives in — so the shell reads the window's
//! last rectangle before dropping the old scene and hands it to the new one as
//! its opening position and size. These tests pin both halves of that hand-off.
//!
//! The file uses the harness `gpu_scenes.rs` uses, so the same binary runs
//! natively and as the module a wasm test page loads.

use unlit_wgpu_test_util::{Ctx, gpu_test_main, gpu_tests};
use unlit3d::prelude::*;
use unlit3d::ui::egui;
use unlit3d_examples::scenes::{self, SceneOptions};
use unlit3d_examples::{FIXED_STEP, Scene};

async fn selector_window_is_laid_out_after_a_frame() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);
    let mut scene = scene(&ctx, size, None);
    bind(&mut scene, &ctx, size);

    scene.advance(FIXED_STEP);
    scene.render();
    scene.end_frame();

    let laid_out = rect(&scene);
    assert!(
        laid_out.is_some(),
        "the selector must be laid out after a frame"
    );
}

async fn selector_window_opens_where_the_previous_scene_left_it() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);

    // A rectangle a user might have dragged the window to; the shell would
    // read it out of the dying scene's egui context.
    let carried = Some(egui::Rect::from_min_size(
        egui::pos2(100.0, 50.0),
        egui::vec2(340.0, 297.0),
    ));
    let mut scene = scene(&ctx, size, carried);
    bind(&mut scene, &ctx, size);

    scene.advance(FIXED_STEP);
    scene.render();
    scene.end_frame();

    let opened = rect(&scene);
    assert_eq!(
        opened, carried,
        "rebuild must inherit the carried rectangle"
    );
}

async fn a_switched_scene_opens_where_the_selector_was_left() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);

    for (a, b) in [(0usize, 7usize), (7, 0)] {
        let mut first = scene_at(&ctx, size, None, scenes::SCENES[a]);
        bind(&mut first, &ctx, size);
        for _ in 0..4 {
            first.advance(FIXED_STEP);
            first.render();
            first.end_frame();
        }
        let opened = rect(&first).expect("laid out");

        let mut second = scene_at(&ctx, size, Some(opened), scenes::SCENES[b]);
        bind(&mut second, &ctx, size);
        for _ in 0..4 {
            second.advance(FIXED_STEP);
            second.render();
            second.end_frame();
        }
        let rebuilt = rect(&second).expect("laid out");
        assert_eq!(
            rebuilt.min, opened.min,
            "the rebuilt selector must keep the position the first scene left"
        );
    }
}

fn rect(scene: &Scene) -> Option<egui::Rect> {
    scene.world.query::<&Source>().find_map(|(_, source)| {
        let ui = source.as_ref::<UiSource>()?;
        ui.context()
            .memory(|memory| memory.area_rect(egui::Id::new("scenes")))
    })
}

fn scene_at(
    ctx: &Ctx,
    size: (u32, u32),
    initial: Option<egui::Rect>,
    def: &'static scenes::SceneDef,
) -> Scene {
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
        initial,
    )
}

/// A scene with the selector mounted, opened at `initial`.
fn scene(ctx: &Ctx, size: (u32, u32), initial: Option<egui::Rect>) -> Scene {
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
        initial,
    )
}

/// Bind an offscreen target the way the snapshot tests do, so `render` draws.
fn bind(scene: &mut Scene, ctx: &Ctx, size: (u32, u32)) {
    let (world, renderer) = (&scene.world, scene.renderer);
    world
        .with_mut::<Renderer, _>(renderer, |renderer| {
            unlit3d_examples::bind_offscreen_target(world, renderer, &ctx.device, size, 1, false)
        })
        .expect("the renderer is a resource entity");
}

gpu_tests! {
    selector_window_is_laid_out_after_a_frame,
    selector_window_opens_where_the_previous_scene_left_it,
    a_switched_scene_opens_where_the_selector_was_left,
}

gpu_test_main!(all_tests());
