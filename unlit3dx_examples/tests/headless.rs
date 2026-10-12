//! Deterministic headless snapshot tests: the offscreen frames and the
//! mixed audio are compared against baselines kept in the asset
//! submodule.
//!
//! The headless run steps the windowed mode's very same worlds inline at
//! fixed rates, so a run is reproducible: same spawn rolls, same
//! collisions, same notes, same frames.

#![cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]

use std::path::Path;
use std::sync::Arc;

use unlit_wgpu_test_util::{
    Ctx, Tolerance, assert_image_snapshot_with_tolerance, block_on, snapshot_path, snapshots,
};
use unlit3dx_examples::headless::{AUDIO_SAMPLE_RATE, HeadlessConfig, run};
use unlit3dx_examples::scene::WINDOW_SIZE;

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

/// The soundfont, kept out of version control under the workspace's
/// assets directory; the tests skip when the repository does not carry
/// it.
fn soundfont() -> Option<Arc<[u8]>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/FluidR3Mono_GM.sf3");
    path.is_file()
        .then(|| std::fs::read(&path).expect("the soundfont reads"))
        .map(Into::into)
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
    let Some(soundfont) = soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
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
    let Some(soundfont) = soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let ctx = block_on(Ctx::headless());
    let output = run(
        &config(soundfont, AUDIO_FRAMES, Vec::new()),
        &ctx.device,
        &ctx.queue,
        ctx.capabilities,
    );

    // The mix quantizes straight into the baseline's PCM16 samples.
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

    let baseline = snapshot_path("unlit3dx/audio.wav");
    if !baseline.exists() || std::env::var("SNAPSHOT_UPDATE").as_deref() == Ok("1") {
        // The frame test may be creating the directory concurrently.
        std::fs::create_dir_all(baseline.parent().expect("a parent directory"))
            .expect("the snapshot directory creates");
        unlit3dx_examples::headless::write_wav(&baseline, &output.audio, AUDIO_SAMPLE_RATE)
            .expect("the audio baseline stores");
        return;
    }
    let expected = read_pcm16_wav(&baseline);
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

/// Read the 16-bit stereo PCM data chunk of the baseline WAV.
fn read_pcm16_wav(path: &Path) -> Vec<i16> {
    let bytes = std::fs::read(path).expect("the audio baseline reads");
    let mut at = 12;
    let mut data = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len =
            u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("a chunk length")) as usize;
        if id == b"data" {
            data = Some(bytes[at + 8..at + 8 + len].to_vec());
        }
        at += 8 + len + (len % 2);
    }
    let data = data.expect("the baseline carries a data chunk");
    data.as_chunks::<{ size_of::<i16>() }>()
        .0
        .iter()
        .map(|chunk| i16::from_le_bytes(*chunk))
        .collect()
}
