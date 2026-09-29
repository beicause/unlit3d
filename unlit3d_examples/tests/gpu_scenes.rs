//! End-to-end snapshot tests for the example's scenes.
//!
//! Every scene the example ships freezes the frames its snapshots describe, and
//! these tests draw them: each builds through [`Scene`] at the size and frame
//! count the scene declares, renders offscreen, and compares each frozen frame
//! against the stored reference through the harness — the same comparison the
//! crates' GPU tests make.
//!
//! Living beside the crate they exercise rather than behind a command line is
//! what lets one body serve every runner: `cargo nextest run` drives them
//! natively, `UNLIT3D_DEVICE_TIER=webgl2` drives the same tests against the
//! device a browser has, and on `wasm32-unknown-unknown` the file *is* the
//! module the test page loads, with the snapshots embedded in the binary.
//!
//! That last part is why each scene's frame table is written out rather than
//! built at run time: a web build can only carry the files it was told about at
//! compile time. The scene still names each frame itself, and the test checks
//! the two agree, so the table cannot drift from the scene it describes.

use unlit_wgpu_test_util::{
    Ctx, Snapshot, Tolerance, assert_image_snapshot_with_tolerance, gpu_test_main, gpu_tests,
    read_texture_bytes, snapshots,
};
use unlit3d::prelude::*;
use unlit3d_examples::scenes::{self, SceneDef, SceneOptions};
use unlit3d_examples::{FIXED_STEP, Scene};

/// The tolerance the scenes that draw no panel are judged with.
///
/// A scene's frames cover whole surfaces, so a regression changes large regions
/// of one and its score falls far below the bar, while two implementations of the
/// same drawing land close together — except on the edges. Which of two triangles
/// an edge pixel belongs to is the rasterizer's tie-breaking rule to decide, and
/// the API leaves that to each implementation, so a correct frame differs between
/// backends by a scatter of single pixels along its outlines.
///
/// Those pixels cost far more than their number: a real WebGL2 pass scored 76.51
/// on the worst frame here while differing from the baseline by a mean of 0.02 per
/// channel over 14 pixels. `mesh_topologies` is the extreme case of the same
/// thing — every one of its pixels is an outline — and its measurements sit in the
/// same table, which is why one bound covers both:
///
/// | frame | SSIMULACRA2 |
/// |-------|-------------|
/// | `mesh_topologies` over a real WebGL2 pass | 76.51 |
/// | the worst of the other scenes' frames, over that pass | 76.55 |
/// | `mesh_topologies` on Metal and on DX12 | 77.14 |
/// | the baseline against itself, on the GPU that captured it | 100.00 |
/// | `mesh_topologies` with its differing outline pixels corrected | 98.20 |
/// | a real regression: its triangle strip's cell shifted one pixel | 63.82 |
/// | a real regression: that cell drawn as a triangle list | -56.92 |
///
/// 75 leaves the least room above that noise floor while staying clear of real
/// regressions: it sits under the 76.5 every implementation reached, and far above
/// the mildest regression measured. The default 85 would fail every implementation
/// that is not the one the frames were captured on.
///
/// The outlier bound is the same difference seen as a count: no scene here
/// exceeded 0.037% of its pixels beyond a channel difference of 8, so 0.1% leaves
/// that scatter room while catching a frame differing over many more.
const SCENE_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(75.0),
    max_outliers: Some(0.001),
    channel_delta: 8,
};

/// The tolerance the scenes that draw an egui panel are judged with.
///
/// A panel is text, and text is anti-aliased: every glyph edge is a partial
/// coverage value, which two rasterizers round differently. That differs over far
/// more pixels than a geometric edge does, and it reaches lower scores, so the
/// bound is looser on both gates:
///
/// | frame | SSIMULACRA2 | pixels past a channel difference of 8 |
/// |-------|-------------|---------------------------------------|
/// | `mesh_and_ui`, over a real WebGL2 pass | 82.46 | 1.41% |
/// | `ui_only`, over the same pass | 86.68 | 1.41% |
/// | `spin_cube`, over the same pass | 87.19 | 0.63% |
///
/// 2% leaves that scatter room while still failing a frame a change moved: a
/// regression that shifts the geometry behind a panel or the panel itself moves
/// whole regions, not one glyph edge.
const UI_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(75.0),
    max_outliers: Some(0.02),
    channel_delta: 8,
};

/// Draw the scene at its own size and compare every frame it freezes.
///
/// The capture settings are the ones the snapshots were taken with: the scene's
/// own size and frame count, no letterbox, and a sequence that advances once per
/// drawn frame. `frames` names the snapshots in frame order, and the scene's own
/// table is walked alongside it — a scene freezes only some of the frames it
/// draws, and the two have to agree on which.
async fn compare_scene(def: &'static SceneDef, tolerance: Tolerance, frames: &[Snapshot]) {
    let ctx = Ctx::headless().await;
    let size = def.size;

    let mut scene = Scene::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        ctx.capabilities,
        size,
        SceneOptions {
            ui: def.ui,
            selector: false,
            reproducible: true,
            letterbox: false,
            sequence_step: None,
        },
        def,
    );

    let (world, renderer) = (&scene.world, scene.renderer);
    let target = world
        .with_mut::<Renderer, _>(renderer, |renderer| {
            unlit3d_examples::bind_offscreen_target(
                world,
                renderer,
                &ctx.device,
                size,
                def.samples,
                def.depth,
            )
        })
        .expect("the renderer is a resource entity");
    set_frame_viewport(world, scene.viewport.map(FrameViewport));

    let bytes_per_pixel = target
        .format()
        .block_copy_size(None)
        .expect("a render-target format has a block copy size");

    let mut compared = 0;
    for frame in 0..def.frames {
        scene.advance(FIXED_STEP);
        scene.render();
        scene.end_frame();

        let Some(name) = (scene.control.snapshot)(frame) else {
            continue;
        };
        let snapshot = frames.get(compared).unwrap_or_else(|| {
            panic!(
                "scene `{}` freezes more frames than its table names",
                def.id
            )
        });
        assert_eq!(
            name,
            snapshot.name(),
            "frame {frame} of scene `{}` is not the snapshot the table names there",
            def.id
        );

        let bytes = read_texture_bytes(&ctx, &target, size.0, size.1, bytes_per_pixel);
        assert_image_snapshot_with_tolerance(*snapshot, &bytes, size.0, size.1, tolerance);
        compared += 1;
    }

    assert_eq!(
        compared,
        frames.len(),
        "scene `{}` freezes {compared} frames but its table names {}",
        def.id,
        frames.len()
    );
}

/// Draw the scene at `size` with its content letterboxed, and compare the last
/// frame against `snapshot`.
///
/// A scene's own snapshots are all at its declared size, so a capture at
/// another shape is a comparison of its own. This is the letterbox check: the
/// picture has to land inside the fitted region with the rest left clear, so a
/// change that stretched the content to the target, dropped the region, or let
/// the content leak outside it fails here.
async fn compare_letterboxed(def: &'static SceneDef, size: (u32, u32), snapshot: Snapshot) {
    let ctx = Ctx::headless().await;

    let mut scene = Scene::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        ctx.capabilities,
        size,
        SceneOptions {
            ui: def.ui,
            selector: false,
            reproducible: true,
            letterbox: true,
            sequence_step: None,
        },
        def,
    );

    let (world, renderer) = (&scene.world, scene.renderer);
    let target = world
        .with_mut::<Renderer, _>(renderer, |renderer| {
            unlit3d_examples::bind_offscreen_target(
                world,
                renderer,
                &ctx.device,
                size,
                def.samples,
                def.depth,
            )
        })
        .expect("the renderer is a resource entity");
    set_frame_viewport(world, scene.viewport.map(FrameViewport));

    let bytes_per_pixel = target
        .format()
        .block_copy_size(None)
        .expect("a render-target format has a block copy size");

    for _ in 0..def.frames {
        scene.advance(FIXED_STEP);
        scene.render();
        scene.end_frame();
    }

    let bytes = read_texture_bytes(&ctx, &target, size.0, size.1, bytes_per_pixel);
    assert_image_snapshot_with_tolerance(snapshot, &bytes, size.0, size.1, UI_TOLERANCE);
}

async fn ecs_animated_matches_its_snapshots() {
    compare_scene(
        &scenes::animated::SCENE,
        SCENE_TOLERANCE,
        &snapshots![
            "ecs_animated/frame_00_alloc3_free0_live3.webp",
            "ecs_animated/frame_01_alloc2_free0_live5.webp",
            "ecs_animated/frame_02_alloc0_free0_live5.webp",
            "ecs_animated/frame_03_alloc3_free0_live8.webp",
            "ecs_animated/frame_04_alloc0_free1_live7.webp",
            "ecs_animated/frame_05_alloc1_free0_live8.webp",
            "ecs_animated/frame_06_alloc0_free2_live6.webp",
            "ecs_animated/frame_07_alloc2_free0_live8.webp",
        ],
    )
    .await;
}

async fn ecs_skinned_matches_its_snapshots() {
    compare_scene(
        &scenes::skinned::SCENE,
        SCENE_TOLERANCE,
        &snapshots![
            "ecs_skinned/frame_00.webp",
            "ecs_skinned/frame_01.webp",
            "ecs_skinned/frame_02.webp",
            "ecs_skinned/frame_03.webp",
            "ecs_skinned/frame_04.webp",
            "ecs_skinned/frame_05.webp",
        ],
    )
    .await;
}

async fn ecs_morphed_matches_its_snapshots() {
    compare_scene(
        &scenes::morphed::SCENE,
        SCENE_TOLERANCE,
        &snapshots![
            "ecs_morphed/frame_00.webp",
            "ecs_morphed/frame_01.webp",
            "ecs_morphed/frame_02.webp",
            "ecs_morphed/frame_03.webp",
            "ecs_morphed/frame_04.webp",
            "ecs_morphed/frame_05.webp",
        ],
    )
    .await;
}

async fn instanced_skinned_morph_matches_its_snapshots() {
    compare_scene(
        &scenes::instanced_skinned_morph::SCENE,
        SCENE_TOLERANCE,
        &snapshots![
            "instanced_skinned_morph/frame_00.webp",
            "instanced_skinned_morph/frame_01.webp",
            "instanced_skinned_morph/frame_02.webp",
            "instanced_skinned_morph/frame_03.webp",
            "instanced_skinned_morph/frame_04.webp",
            "instanced_skinned_morph/frame_05.webp",
        ],
    )
    .await;
}

async fn spin_cube_matches_its_snapshot() {
    compare_scene(
        &scenes::spin_cube::SCENE,
        UI_TOLERANCE,
        &snapshots!["spin_cube.webp"],
    )
    .await;
}

async fn mesh_and_ui_matches_its_snapshot() {
    compare_scene(
        &scenes::mesh_and_ui::SCENE,
        UI_TOLERANCE,
        &snapshots!["mesh_and_ui.webp"],
    )
    .await;
}

async fn ui_only_matches_its_snapshot() {
    compare_scene(
        &scenes::ui_only::SCENE,
        UI_TOLERANCE,
        &snapshots!["ui_only.webp"],
    )
    .await;
}

async fn transparent_zsorted_matches_its_snapshot() {
    compare_scene(
        &scenes::transparent::SCENE,
        SCENE_TOLERANCE,
        &snapshots!["transparent_zsorted.webp"],
    )
    .await;
}

async fn mesh_topologies_matches_its_snapshot() {
    compare_scene(
        &scenes::mesh_topologies::SCENE,
        SCENE_TOLERANCE,
        &snapshots!["mesh_topologies.webp"],
    )
    .await;
}

/// The default scene at a target narrower than its baseline.
///
/// The scene's own snapshots are all at its baseline shape, which has no bars
/// to check, so the fitted picture is compared against a capture of its own.
async fn the_default_scene_letterboxes_into_a_narrow_target() {
    compare_letterboxed(
        &scenes::spin_cube::SCENE,
        (480, 720),
        snapshots!["spin_cube_narrow.webp"][0],
    )
    .await;
}

gpu_tests! {
    ecs_animated_matches_its_snapshots,
    ecs_skinned_matches_its_snapshots,
    ecs_morphed_matches_its_snapshots,
    instanced_skinned_morph_matches_its_snapshots,
    spin_cube_matches_its_snapshot,
    mesh_and_ui_matches_its_snapshot,
    ui_only_matches_its_snapshot,
    transparent_zsorted_matches_its_snapshot,
    mesh_topologies_matches_its_snapshot,
    the_default_scene_letterboxes_into_a_narrow_target,
}

gpu_test_main!(all_tests());
