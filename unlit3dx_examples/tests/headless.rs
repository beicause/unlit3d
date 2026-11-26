//! Deterministic snapshot tests: the offscreen frames on every target, and
//! on the host the mixed audio too, compared against baselines kept in the
//! asset submodule.
//!
//! The headless run steps the windowed mode's very same worlds inline at
//! fixed rates, so a run is reproducible: same spawn rolls, same balls,
//! same music, same frames. The file is also the module the wasm suite's
//! page loads, with the frame baselines embedded in the binary.
//!
//! The frames are compared whenever the run can render, soundfont or not:
//! the scene's rendering must not depend on the audio world, so a missing
//! soundfont is no reason to skip the frame baselines. The audio baseline
//! needs the real soundfont and a filesystem to decode it from, so it is a
//! host test; where `UNLIT3D_REQUIRE_SOUNDFONT` is set — as it is in CI — a
//! missing one fails the run instead of skipping it, so a checkout that
//! silently stopped exercising the mix cannot pass.

use std::sync::Arc;

use unlit_wgpu_test_util::{
    Ctx, Tolerance, assert_image_snapshot_with_tolerance, gpu_tests, snapshots,
};
use unlit3dx::app::{
    AUDIO_SAMPLE_RATE, Capture, HeadlessConfig, RunShape, WorldOwnership, headless as headless_run,
};
use unlit3dx_asset_bytes::soundfont as embedded_soundfont;
use unlit3dx_examples::demo::Demo;
use unlit3dx_examples::scene::WINDOW_SIZE;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use std::path::Path;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use crate::support;

/// The frame captures' comparison tolerance for a run that draws the scene
/// itself. The scene is flat-shaded and the camera path is fixed, so the
/// floors are the driver-to-driver noise the reference suite measured for
/// this class of scene, not the library defaults.
///
/// Against the baselines this test compares with, Metal draws the worst frame
/// at 66.54, with 0.24% of its pixels past a channel difference of 8; the other
/// native backends stay above that. A slower rasterizer than the one that
/// stored the baselines costs a few points on its own, so the score floor sits
/// below the worst measurement, and a regression that shifts the picture still
/// fails it: the reference suite scores a one-pixel shift at 57.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(62.0),
    max_outliers: Some(0.005),
    channel_delta: 8,
};

/// The same tolerance for a browser, which draws through software WebGL2 in
/// CI. The scene's hard edges are where two rasterizers disagree, and that
/// backend disagrees with the Vulkan capture the baselines came from on
/// exactly those pixels: its worst frame here scores 56.68 with 0.29% of its
/// pixels past a channel difference of 8.
///
/// That is close enough to the 57 the reference suite scores a one-pixel shift
/// at that the floor cannot be the gate which catches one, so it is set below
/// the worst correct frame by the few points a backend's own rasterizer is
/// worth, and the outlier share is the gate that catches it: a shift puts
/// 1.4-1.7% of a frame past the channel delta, well above the 1% allowed here.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(52.0),
    max_outliers: Some(0.01),
    channel_delta: 8,
};

/// How many frames the headless run steps: five seconds at its 60 Hz.
const FRAMES: u32 = 300;

/// Which frames the tests capture, spread over the three auto-dropped
/// balls' falls and bounces.
const CAPTURE: [u32; 4] = [120, 180, 240, 299];

/// The names of the frame baselines, in capture order.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const FRAME_BASELINES: [&str; CAPTURE.len()] = [
    "unlit3dx/frame_0120.webp",
    "unlit3dx/frame_0180.webp",
    "unlit3dx/frame_0240.webp",
    "unlit3dx/frame_0299.webp",
];

/// How many frames the audio run steps: ten seconds at its 60 Hz, long
/// enough for the spawn cadence and the fades to set a rhythm.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const AUDIO_FRAMES: u32 = 600;

/// The soundfont, kept out of version control under the workspace's
/// assets directory. The build embeds a silent stub when it is absent, so
/// a run without it still renders and still captures frames.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn soundfont() -> Option<Arc<[u8]>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/FluidR3Mono_GM.sf3");
    path.is_file()
        .then(|| std::fs::read(&path).expect("the soundfont reads"))
        .map(Into::into)
}

/// A browser or an activity cannot read the workspace's assets, so their
/// frame check runs on the silent stub the build embeds. The frames do not
/// depend on the audio world, which is why that is enough there.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
fn soundfont() -> Option<Arc<[u8]>> {
    None
}

/// Whether the run must not skip the audio check. CI sets this: there a
/// skipped test would let the whole end-to-end mix stop being exercised
/// without any failure.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn soundfont_required() -> bool {
    std::env::var_os("UNLIT3D_REQUIRE_SOUNDFONT").is_some()
}

/// The soundfont the audio check runs on, or the reason it may not run.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn required_soundfont() -> Option<Arc<[u8]>> {
    let soundfont = soundfont();
    if soundfont.is_none() && soundfont_required() {
        panic!("no soundfont under assets, and UNLIT3D_REQUIRE_SOUNDFONT is set");
    }
    soundfont
}

fn config(frames: u32, capture: Vec<u32>) -> HeadlessConfig {
    HeadlessConfig {
        size: WINDOW_SIZE,
        shape: RunShape {
            ownership: WorldOwnership::Hosted,
            frames: Some(frames),
            capture: Some(Capture::new(capture, None)),
            ..RunShape::default()
        },
        audio_sample_rate: AUDIO_SAMPLE_RATE,
        ..Default::default()
    }
}

async fn frames_match_the_baselines() {
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    support::assert_baselines_exist(&FRAME_BASELINES);
    // The frame check does not need the real soundfont: the scene renders
    // the same either way, and the build's silent stub keeps the run
    // compiling where the asset is absent. The audio check is the one that
    // insists on the real font.
    let soundfont = soundfont().unwrap_or_else(|| Arc::from(embedded_soundfont()));
    let ctx = Ctx::headless().await;
    let output = headless_run(
        Demo { soundfont },
        &config(FRAMES, CAPTURE.to_vec()),
        ctx.render_device(),
    )
    .await
    .expect("the headless scene renders");
    assert_eq!(output.frames.len(), CAPTURE.len());
    assert_eq!(
        output
            .frames
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        CAPTURE.to_vec(),
        "the run hands back the frames it was asked for, in capture order"
    );
    let frames: Vec<&[u8]> = output
        .frames
        .iter()
        .map(|(_, pixels)| pixels.as_slice())
        .collect();
    for (frame, snapshot) in frames.iter().copied().zip(snapshots![
        "unlit3dx/frame_0120.webp",
        "unlit3dx/frame_0180.webp",
        "unlit3dx/frame_0240.webp",
        "unlit3dx/frame_0299.webp",
    ]) {
        assert_image_snapshot_with_tolerance(
            snapshot,
            frame,
            WINDOW_SIZE.0,
            WINDOW_SIZE.1,
            FRAME_TOLERANCE,
        );
    }
}

/// A frame the run draws must not depend on whether the frames before it were
/// drawn too: a draw reads what the steps left behind, which is what lets the
/// run step between two captures without drawing. So the frame a run captures
/// alone and the same frame captured after three earlier ones have to hand
/// back the same pixels.
///
/// The run is its own, shorter than the baseline capture, so this check does
/// not lean on the baseline table naming a frame the second run draws.
async fn an_undrawn_frame_matches_a_drawn_one() {
    let soundfont = soundfont().unwrap_or_else(|| Arc::from(embedded_soundfont()));
    let ctx = Ctx::headless().await;
    let frames = 60;
    let captured = [15, 30, 45, frames - 1];
    let last = frames - 1;
    let alone = headless_run(
        Demo {
            soundfont: soundfont.clone(),
        },
        &config(frames, vec![last]),
        ctx.render_device(),
    )
    .await
    .expect("the headless scene renders");
    let surrounded = headless_run(
        Demo { soundfont },
        &config(frames, captured.to_vec()),
        ctx.render_device(),
    )
    .await
    .expect("the headless scene renders");
    assert_eq!(alone.frames.len(), 1);
    assert_eq!(surrounded.frames.len(), captured.len());
    let drawn_late = &alone.frames[0].1;
    let drawn_on_time = &surrounded.frames[captured.len() - 1].1;
    if drawn_late != drawn_on_time {
        let differing = drawn_late
            .iter()
            .zip(drawn_on_time)
            .filter(|(late, on_time)| late != on_time)
            .count();
        panic!(
            "frame {last} differs in {differing} of {} bytes whether the frames before it were drawn",
            drawn_late.len()
        );
    }
}

/// The audio baseline is a host-only check: it needs the real soundfont and
/// a filesystem, neither of which a browser has. A skipped check has to say
/// so out loud: a green run that quietly tested nothing is worse than a red
/// one, and the expectation sits here rather than on the file so a target
/// that drops the check does not leave it unfulfilled.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
#[expect(
    clippy::print_stderr,
    reason = "naming the reason a test skipped is part of the test's output"
)]
async fn audio_matches_the_baseline() {
    let Some(soundfont) = required_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    support::assert_baselines_exist(&["unlit3dx/audio.flac"]);
    let ctx = Ctx::headless().await;
    let output = headless_run(
        Demo { soundfont },
        &config(AUDIO_FRAMES, Vec::new()),
        ctx.render_device(),
    )
    .await
    .expect("the headless scene renders");

    let samples = support::to_samples(&output.audio);
    let loudest = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(loudest > 1000, "the mix came out nearly silent");

    support::check_audio_baseline("unlit3dx/audio.flac", &samples);
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn an_ended_or_failed_turn_does_not_capture_a_frame() {
    use unlit_ecs::World;
    use unlit3dx::app::{App, NoArgs, RenderPorts, RenderSetup, RunError, ScenePorts, SceneWorlds};
    use unlit3dx::turn::{TickOutcome, WorldError, WorldStage};
    use unlit3dx::world::WorldRunner;
    use unlit3dx_examples::scene::setup_render_world;

    struct EndOnSecondTurn {
        fail: bool,
    }

    impl App for EndOnSecondTurn {
        type Args = NoArgs;

        fn spawn_scene<R: WorldRunner>(
            &mut self,
            host: &mut World,
            _: ScenePorts<'_>,
        ) -> SceneWorlds {
            let fail = self.fail;
            let gameplay = R::spawn(
                host,
                "gameplay",
                1,
                |_| false,
                move |_, _, ran, _| {
                    if !core::mem::replace(ran, true) {
                        return TickOutcome::Running;
                    }
                    assert!(!fail, "the gameplay tick failed");
                    TickOutcome::Finished
                },
            )
            .expect("the gameplay world starts");
            let mut idle = |name| {
                R::spawn(host, name, 1, |_| (), |_, _, _, _| TickOutcome::Running)
                    .expect("the idle world starts")
            };
            SceneWorlds {
                gameplay,
                physics: idle("physics"),
                audio: idle("audio"),
            }
        }

        fn setup_render(&mut self, host: &mut World, ports: RenderPorts<'_>) -> RenderSetup {
            let (renderer, pass) =
                setup_render_world(host, ports.world, ports.device.clone(), None);
            RenderSetup { renderer, pass }
        }
    }

    let ctx = Ctx::headless().await;
    for fail in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "unlit3dx-ended-capture-{}-{fail}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("a fresh capture directory");
        let mut config = config(2, vec![]);
        config.size = (16, 16);
        config.shape.capture = Some(Capture::new(vec![0, 1], Some(dir.clone())));
        let result = headless_run(EndOnSecondTurn { fail }, &config, ctx.render_device()).await;
        let captured = std::fs::read_dir(&dir)
            .expect("the capture directory is readable")
            .map(|entry| {
                let entry = entry.expect("a capture directory entry is readable");
                std::fs::remove_file(entry.path()).expect("the test capture is removable");
                entry.file_name()
            })
            .collect::<Vec<_>>();
        std::fs::remove_dir(&dir).expect("the now empty test directory is removable");
        assert_eq!(captured, ["frame_0.png"]);
        if fail {
            assert!(matches!(
                result,
                Err(RunError::Scene(WorldError::Panic { stage: WorldStage::Body, message }))
                    if &*message == "the gameplay tick failed"
            ));
        } else {
            let output = result.expect("the scene finishes normally");
            assert_eq!(output.advanced, 1);
            assert_eq!(output.frames.len(), 1);
            assert_eq!(output.frames[0].0, 0);
        }
    }
}

// Filesystem capture and the real soundfont are available only on the host.
gpu_tests! {
    frames_match_the_baselines,
    an_undrawn_frame_matches_a_drawn_one,
    #[host_only]
    an_ended_or_failed_turn_does_not_capture_a_frame,
    #[host_only]
    audio_matches_the_baseline,
}
