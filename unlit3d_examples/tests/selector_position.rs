//! The scene-selector window keeps its dragged position across a scene switch.
//!
//! A scene switch rebuilds the whole scene — world, renderer, and the egui
//! context the window's position lives in — so the shell reads the window's
//! last rectangle before dropping the old scene and hands it to the new one as
//! its opening position. These tests pin both halves of that hand-off.
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

    let position = selector_position(&scene);
    assert!(
        position.is_some(),
        "the selector must be laid out after a frame"
    );
}

async fn selector_window_opens_where_the_previous_scene_left_it() {
    let ctx = Ctx::headless().await;
    let size = (960, 720);

    // A position a user might have dragged the window to; the shell would
    // read it out of the dying scene's egui context.
    let carried = Some(egui::pos2(100.0, 50.0));
    let mut scene = scene(&ctx, size, carried);
    bind(&mut scene, &ctx, size);

    scene.advance(FIXED_STEP);
    scene.render();
    scene.end_frame();

    let position = selector_position(&scene);
    assert_eq!(
        position, carried,
        "rebuild must inherit the carried position"
    );
}

/// A scene with the selector mounted, opened at `initial`.
fn scene(ctx: &Ctx, size: (u32, u32), initial: Option<egui::Pos2>) -> Scene {
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

/// `Scene::selector_position` is private; mirror its body here.
fn selector_position(scene: &Scene) -> Option<egui::Pos2> {
    scene.world.query::<&Source>().find_map(|(_, source)| {
        let ui = source.as_ref::<UiSource>()?;
        ui.context().memory(|memory| {
            memory
                .area_rect(egui::Id::new("scenes"))
                .map(|rect| rect.min)
        })
    })
}

gpu_tests! {
    selector_window_is_laid_out_after_a_frame,
    selector_window_opens_where_the_previous_scene_left_it,
}

gpu_test_main!(all_tests());
