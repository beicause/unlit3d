//! The baseline helpers both snapshot tests share.
//!
//! A baseline that is missing must fail rather than be re-stored by the
//! comparison, and an audio baseline is compared sample by sample the same way
//! whichever test recorded it. Keeping that in one place means neither test can
//! pass by quietly forgetting to check its mix.
//!
//! The checks are host-only: a browser has neither a filesystem nor the real
//! soundfont, and its baselines are embedded at build time instead.

#![cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]

use std::path::Path;

use unlit_wgpu_test_util::snapshot_path;
use unlit3dx::prelude::{AudioFormat, RecordingFormat};
use unlit3dx_examples::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};

/// How far a sample may sit from its baseline.
///
/// The mix is deterministic — the same frames, the same synth, the same
/// samples — but the last bit is the compiler's to spend: a reordering in the
/// synthesizer moves a sample by one or two. Eight is well below the smallest
/// difference a listener can hear and well above that noise.
pub const AUDIO_TOLERANCE: u16 = 8;

/// Fail on a missing baseline instead of letting a comparison store it: a
/// checkout that lost its baselines must not look green.
///
/// A wasm build needs no check of its own: its baselines are embedded with
/// `include_bytes!`, so a missing one fails the build rather than the test.
pub fn assert_baselines_exist(names: &[&str]) {
    if updating() {
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

/// Store a mix under `name` when `SNAPSHOT_UPDATE` asks for it, and compare it
/// against the stored baseline otherwise.
pub fn check_audio_baseline(name: &str, samples: &[i16]) {
    if updating() {
        let path = snapshot_path(name);
        // The frame checks may be creating the directory concurrently.
        std::fs::create_dir_all(path.parent().expect("a parent directory"))
            .expect("the snapshot directory creates");
        let format = AudioFormat {
            sample_rate: AUDIO_SAMPLE_RATE,
            channels: AUDIO_CHANNELS as u16,
        };
        let audio: Vec<f32> = samples
            .iter()
            .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
            .collect();
        let flac = RecordingFormat::Flac
            .encode(format, &audio)
            .expect("the audio baseline encodes");
        std::fs::write(&path, flac).expect("the audio baseline stores");
        return;
    }
    let expected = read_flac(&snapshot_path(name));
    // The length is checked first: a mix that grew or shrank is a change in
    // what the scene played, not drift, and the sample comparison would report
    // it as a wall of mismatches at the end instead.
    assert_eq!(
        expected.len(),
        samples.len(),
        "the mix under `{name}` changed length; re-store it with SNAPSHOT_UPDATE=1"
    );
    let worst = expected
        .iter()
        .zip(samples)
        .map(|(one, two)| one.abs_diff(*two))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= AUDIO_TOLERANCE,
        "the mix drifted from `{name}`: worst sample delta {worst}"
    );
}

/// Quantize a recorded mix into the 16-bit samples a baseline holds.
pub fn to_samples(audio: &[f32]) -> Vec<i16> {
    audio
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect()
}

/// Whether this run stores the baselines instead of comparing them.
fn updating() -> bool {
    std::env::var("SNAPSHOT_UPDATE").as_deref() == Ok("1")
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
