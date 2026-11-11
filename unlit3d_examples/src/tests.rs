use crate::scene::{letterbox, tick};
// The scene-switch test below drives the app's own slots, so it is the only
// part of this module that needs these names.
#[cfg(not(target_arch = "wasm32"))]
use crate::present::switch_scene;
// These three are the scene-switch test's, which a browser build leaves out.
#[cfg(not(target_arch = "wasm32"))]
use crate::scenes;
#[cfg(not(target_arch = "wasm32"))]
use crate::state::{InitialScene, PresenterEntity, SceneEntity};
#[cfg(not(target_arch = "wasm32"))]
use unlit_ecs::bundle;
#[cfg(not(target_arch = "wasm32"))]
use unlit3d::prelude::World;
/// A scene switch drops the presentation path, which was built around the
/// scene it replaced.
///
/// The path's surface and the pass that presents through it belong to the
/// dying scene's world, so the rebuilt scene builds a fresh one instead of
/// presenting through a pass that went with the world it drew.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_scene_switch_drops_the_presentation_path() {
    let mut world = World::new();

    let old_scene = world.root_scope_mut().spawn(bundle![]);
    let old_presenter = world.root_scope_mut().spawn(bundle![]);
    let app = world.root_scope_mut().spawn(bundle![
        InitialScene(scenes::default()),
        PresenterEntity(Some(old_presenter)),
        SceneEntity(Some(old_scene)),
    ]);

    let next = scenes::by_id("ui_only").expect("the example ships a UI-only scene");
    switch_scene(&world, app, next);
    world.apply().unwrap();

    assert!(
        world
            .root_scope()
            .query_one::<&PresenterEntity>(app)
            .expect("the app keeps its presentation slot")
            .get()
            .0
            .is_none(),
        "the presentation path went with the scene it presented"
    );
    assert!(
        !world.root_scope().contains(old_presenter),
        "the presentation path's entity was despawned"
    );
    assert!(
        !world.root_scope().contains(old_scene),
        "the old scene was despawned"
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&InitialScene>(app)
            .expect("the app keeps its initial scene")
            .get()
            .0
            .id,
        "ui_only"
    );
}

#[test]
fn a_clock_reaches_its_step_only_after_enough_time() {
    let mut clock = 0.0;
    let step = 0.5;

    // Half the interval is not enough.
    assert!(!tick(&mut clock, step, 0.25));
    // The interval is reached exactly.
    assert!(tick(&mut clock, step, 0.25));
    // The clock keeps the leftover rather than resetting, so the next
    // interval is reached in the time that is left.
    assert!(!tick(&mut clock, step, 0.4));
    assert!(tick(&mut clock, step, 0.1));
}

#[test]
fn a_long_stall_advances_one_step_and_keeps_the_rest() {
    let mut clock = 0.0;
    // A two-second stall over a half-second step consumes one step and
    // leaves the surplus for later, rather than skipping frames.
    assert!(tick(&mut clock, 0.5, 2.0));
    assert_eq!(clock, 1.5, "the surplus stays in the clock");

    // The surplus is spent over the next three calls, one step each.
    assert!(tick(&mut clock, 0.5, 0.0));
    assert!(tick(&mut clock, 0.5, 0.0));
    assert!(tick(&mut clock, 0.5, 0.0));
    assert_eq!(clock, 0.0, "the surplus is used up");
    assert!(!tick(&mut clock, 0.5, 0.0));
}

#[test]
fn only_a_scene_with_a_baseline_and_a_window_letterboxes() {
    // The windowed path keeps a declared baseline's aspect.
    let (viewport, content) = letterbox(Some((960, 720)), (1600, 600), true);
    assert_eq!(content, (800, 600), "the camera is built for the region");
    let viewport = viewport.expect("a letterbox is stated");
    assert_eq!((viewport.width, viewport.height), (800.0, 600.0));
    assert_eq!(viewport.x, 400.0, "centred horizontally");

    // A capture never does: it draws at the scene's own size.
    assert_eq!(
        letterbox(Some((960, 720)), (1600, 600), false),
        (None, (1600, 600))
    );

    // A scene with no aspect of its own draws into the whole target, which
    // is what a UI-only scene wants at any size.
    assert_eq!(letterbox(None, (1600, 600), true), (None, (1600, 600)));

    // A target of the baseline's own shape needs no bars.
    let (viewport, content) = letterbox(Some((960, 720)), (960, 720), true);
    assert_eq!(content, (960, 720));
    let viewport = viewport.expect("a letterbox is stated");
    assert_eq!((viewport.x, viewport.y), (0.0, 0.0));
}
