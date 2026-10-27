//! What the native and wasm drivers of the MCP-input sequence share.
//!
//! The two drivers run the same story over different transports: the native
//! one speaks MCP to a child process, the wasm one drives the interactive run
//! in-process. The frames they wait, the buttons they click and the baselines
//! they compare against are the story, not the transport, so they live here.

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use std::path::PathBuf;

#[cfg(target_arch = "wasm32")]
use unlit_ecs::World;
#[cfg(target_arch = "wasm32")]
use unlit3dx_examples::interactive::Interactive;
#[cfg(target_arch = "wasm32")]
use unlit3dx_examples::scene::SceneHandles;

/// How many frames the first recording runs: long enough to be sure an idle
/// scene stays silent, short enough to keep the test quick.
pub const WARMUP_FRAMES: u32 = 60;

/// How many frames a drop settles for its frame baseline.
pub const SETTLE_FRAMES: u32 = 60;

/// How many frames the one-shot sequence plays before its baseline.
pub const PLAY_FRAMES: u32 = 180;

/// How many frames the release waits after a clear and a pause.
pub const RELEASE_FRAMES: u32 = 300;

/// How many seconds of audio every recording in the sequence allows.
#[cfg(target_arch = "wasm32")]
pub const RECORDING_SECONDS: f32 = 60.0;

/// The peak a recording of the playing sequence clears, in unit samples.
#[cfg(target_arch = "wasm32")]
pub const MIN_PEAK: f32 = 100.0 / i16::MAX as f32;

/// The soundfont bytes the demo builds with, or an empty slice when the
/// checkout carries none.
#[cfg(target_arch = "wasm32")]
pub fn soundfont_bytes() -> &'static [u8] {
    unlit3dx_asset_bytes::soundfont()
}

/// The centre of every button the panel laid out, keyed by its label.
#[cfg(target_arch = "wasm32")]
pub fn button_centres(
    host: &World,
    worlds: &SceneHandles,
) -> hashbrown::HashMap<&'static str, [f32; 2]> {
    unlit3dx_examples::interactive::Interactive::panel_buttons(host, worlds)
        .into_iter()
        .map(|(label, rect)| (label, [rect.center().x, rect.center().y]))
        .collect()
}

/// Click a button's centre: the three input events the MCP `send_input`
/// tool sends, then the frame that delivers them.
#[cfg(target_arch = "wasm32")]
pub fn click(host: &World, interactive: &mut Interactive, at: [f32; 2]) {
    interactive.click(host, at);
    interactive.step(host, 1);
}

/// Compare a mixed recording against the stored FLAC baseline, sample for
/// sample. A browser has no filesystem, so the baseline rides in the binary
/// beside the one the frames use.
#[cfg(target_arch = "wasm32")]
pub fn check_audio_baseline(name: &str, samples: &[f32]) {
    const BASELINE: &[u8] = include_bytes!("snapshots/unlit3dx/mcp_input_audio.flac");
    let mut reader = claxon::FlacReader::new(std::io::Cursor::new(BASELINE))
        .expect("the baseline is a FLAC stream");
    let baseline: Vec<i16> = reader
        .samples()
        .map(|sample| sample.expect("a decodable sample") as i16)
        .collect();
    let quantized: Vec<i16> = samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect();
    assert_eq!(
        quantized.len(),
        baseline.len(),
        "the mix under `{name}` changed length; re-store it with SNAPSHOT_UPDATE=1",
    );
    let worst = quantized
        .iter()
        .zip(baseline.iter())
        .map(|(got, want)| got.abs_diff(*want))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= 8,
        "the mix drifted from `{name}`: worst sample delta {worst}",
    );
}

/// The path helper the native driver's soundfont check uses.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
