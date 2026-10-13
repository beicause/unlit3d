//! Deterministic headless snapshot tests: the offscreen frames and the
//! mixed audio are compared against baselines kept in the asset
//! submodule.
//!
//! The headless run steps the windowed mode's very same worlds inline at
//! fixed rates, so a run is reproducible: same spawn rolls, same shapes,
//! same music, same frames.
//!
//! The frames are compared whenever the run can render, soundfont or not:
//! the scene's rendering must not depend on the audio world, so a missing
//! soundfont is no reason to skip the frame baselines. The audio baseline
//! needs the real soundfont; where `UNLIT3D_REQUIRE_SOUNDFONT` is set — as
//! it is in CI — a missing one fails the run instead of skipping it, so a
//! checkout that silently stopped exercising the mix cannot pass.

#![cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]

use std::path::Path;
use std::sync::Arc;

use unlit_wgpu_test_util::{
    Ctx, Tolerance, assert_image_snapshot_with_tolerance, block_on, snapshot_path, snapshots,
};
use unlit3dx_examples::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, HeadlessConfig, run};
use unlit3dx_examples::scene::{SOUNDFONT, WINDOW_SIZE};

/// The frame captures' comparison tolerance. The scene is flat-shaded and
/// the camera path is fixed, so the floors are the driver-to-driver noise
/// the reference suite measured for this class of scene, not the library
/// defaults.
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(75.0),
    max_outliers: Some(0.001),
    channel_delta: 8,
};

/// How many frames the headless run steps: five seconds at its 60 Hz.
const FRAMES: u32 = 300;

/// How many frames the audio run steps: ten seconds at its 60 Hz, long
/// enough for the spawn cadence and the fades to set a rhythm.
const AUDIO_FRAMES: u32 = 600;

/// Which frames the tests capture, spread over the three auto-dropped
/// balls' falls and bounces.
const CAPTURE: [u32; 4] = [120, 180, 240, 299];

/// The names of the frame baselines, in capture order.
const FRAME_BASELINES: [&str; CAPTURE.len()] = [
    "unlit3dx/frame_0120.webp",
    "unlit3dx/frame_0180.webp",
    "unlit3dx/frame_0240.webp",
    "unlit3dx/frame_0299.webp",
];

/// The soundfont, kept out of version control under the workspace's
/// assets directory. The build embeds a silent stub when it is absent, so
/// a run without it still renders and still captures frames.
fn soundfont() -> Option<Arc<[u8]>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/FluidR3Mono_GM.sf3");
    path.is_file()
        .then(|| std::fs::read(&path).expect("the soundfont reads"))
        .map(Into::into)
}

/// Whether the run must not skip the audio check. CI sets this: there a
/// skipped test would let the whole end-to-end mix stop being exercised
/// without any failure.
fn soundfont_required() -> bool {
    std::env::var_os("UNLIT3D_REQUIRE_SOUNDFONT").is_some()
}

/// The soundfont the audio check runs on, or the reason it may not run.
fn required_soundfont() -> Option<Arc<[u8]>> {
    let soundfont = soundfont();
    if soundfont.is_none() && soundfont_required() {
        panic!("no soundfont under assets, and UNLIT3D_REQUIRE_SOUNDFONT is set");
    }
    soundfont
}

/// Fail on a missing baseline instead of letting the comparison store the
/// frame and pass: a checkout that lost its baselines must not look green.
fn assert_baselines_exist(names: &[&str]) {
    if std::env::var("SNAPSHOT_UPDATE").as_deref() == Ok("1") {
        return;
    }
    for name in names {
        let path = snapshot_path(name);
        assert!(
            path.exists(),
            "the baseline `{name}` is missing at {}; store it with SNAPSHOT_UPDATE=1",
            path.display(),
        );
    }
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

#[test]
fn frames_match_the_baselines() {
    assert_baselines_exist(&FRAME_BASELINES);
    // The frame check does not need the real soundfont: the scene renders
    // the same either way, and the build's silent stub keeps the run
    // compiling where the asset is absent. The audio check is the one that
    // insists on the real font.
    let soundfont = soundfont().unwrap_or_else(|| Arc::from(SOUNDFONT));
    let ctx = block_on(Ctx::headless());
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

#[test]
fn audio_matches_the_baseline() {
    let Some(soundfont) = required_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    assert_baselines_exist(&["unlit3dx/audio.flac"]);
    let ctx = block_on(Ctx::headless());
    let output = run(
        &config(soundfont, AUDIO_FRAMES, Vec::new()),
        &ctx.device,
        &ctx.queue,
        ctx.capabilities,
    );

    // The mix quantizes straight into the baseline's 16-bit samples.
    let samples: Vec<i16> = output
        .audio
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect();
    let loudest = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(loudest > 1000, "the mix came out nearly silent");

    let baseline = snapshot_path("unlit3dx/audio.flac");
    if std::env::var("SNAPSHOT_UPDATE").as_deref() == Ok("1") {
        // The frame test may be creating the directory concurrently.
        std::fs::create_dir_all(baseline.parent().expect("a parent directory"))
            .expect("the snapshot directory creates");
        unlit3dx_examples::headless::write_flac(&baseline, &output.audio, AUDIO_SAMPLE_RATE)
            .expect("the audio baseline stores");
        return;
    }
    let expected = read_flac(&baseline);
    assert_eq!(
        expected.len(),
        samples.len(),
        "the audio length changed; re-store with SNAPSHOT_UPDATE=1"
    );
    let worst = expected
        .iter()
        .zip(&samples)
        .map(|(one, two)| one.abs_diff(*two))
        .max()
        .unwrap();
    assert!(
        worst <= 8,
        "the mix drifted from the baseline: worst sample delta {worst}"
    );
}

/// Decode the baseline FLAC stream into its interleaved 16-bit samples.
///
/// The stream is lossless, so the samples it holds are exactly the ones the
/// encoder was given; the format the header declares is checked so a baseline
/// recorded at some other rate or channel count fails here rather than
/// showing up as a drift in the mix.
fn read_flac(path: &Path) -> Vec<i16> {
    let file = std::fs::File::open(path).expect("the audio baseline opens");
    let mut reader = claxon::FlacReader::new(file).expect("the audio baseline decodes");
    let info = reader.streaminfo();
    assert_eq!(
        usize::try_from(info.channels).expect("a channel count"),
        AUDIO_CHANNELS,
        "the audio baseline is not stereo"
    );
    assert_eq!(
        info.sample_rate, AUDIO_SAMPLE_RATE,
        "the audio baseline was recorded at another sample rate"
    );
    reader
        .samples()
        .map(|sample| sample.expect("a decodable sample") as i16)
        .collect()
}
