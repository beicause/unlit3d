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
use unlit3dx_asset_bytes::soundfont as embedded_soundfont;
use unlit3dx_examples::headless::{AUDIO_SAMPLE_RATE, HeadlessConfig, run};
use unlit3dx_examples::scene::WINDOW_SIZE;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use std::path::Path;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use crate::support;

/// The frame captures' comparison tolerance for a run that draws the scene
/// itself. The scene is flat-shaded and the camera path is fixed, so the
/// floors are the driver-to-driver noise the reference suite measured for
/// this class of scene, not the library defaults.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(75.0),
    max_outliers: Some(0.001),
    channel_delta: 8,
};

/// The same tolerance for a browser, which draws through software WebGL2 in
/// CI. The scene's hard edges are where two rasterizers disagree, and that
/// backend disagrees with the Vulkan capture the baselines came from on
/// exactly those pixels: its worst frame here scores 67.79 with 0.16% of its
/// pixels past a channel difference of 8, so the floors sit below that
/// backend's noise and well above the mildest regression the reference suite
/// measured (a one-pixel shift, 1.75% of the frame).
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(60.0),
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

fn config(soundfont: Arc<[u8]>, frames: u32, capture: Vec<u32>) -> HeadlessConfig {
    HeadlessConfig {
        size: WINDOW_SIZE,
        frames,
        capture,
        audio_sample_rate: AUDIO_SAMPLE_RATE,
        soundfont,
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
    let output = run(
        &config(soundfont, FRAMES, CAPTURE.to_vec()),
        &ctx.device,
        &ctx.queue,
        ctx.capabilities,
    );
    assert_eq!(output.frames.len(), CAPTURE.len());
    let frames: [&[u8]; CAPTURE.len()] = [
        &output.frames[0],
        &output.frames[1],
        &output.frames[2],
        &output.frames[3],
    ];
    for (frame, snapshot) in frames.into_iter().zip(snapshots![
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
    let output = run(
        &config(soundfont, AUDIO_FRAMES, Vec::new()),
        &ctx.device,
        &ctx.queue,
        ctx.capabilities,
    );

    let samples = support::to_samples(&output.audio);
    let loudest = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(loudest > 1000, "the mix came out nearly silent");

    support::check_audio_baseline("unlit3dx/audio.flac", &samples);
}

// The frame check runs wherever the harness does. The audio check needs a
// filesystem and the real soundfont, so it is host-only: a browser and an
// activity have neither, and the wasm suite would otherwise list a trial
// the page has no test for.
gpu_tests! {
    frames_match_the_baselines,
    #[host_only]
    audio_matches_the_baseline,
}
