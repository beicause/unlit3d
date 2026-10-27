//! The wasm half of the MCP-input test: the same clicks and steps the native
//! driver sends, applied in-process.
//!
//! The native driver reaches the scene through the MCP protocol; a browser has
//! no transport thread to answer it with, so this module drives the interactive
//! run directly: the same input events, the same fixed steps, the same readback
//! and recording, compared against the same baselines. What it proves is that
//! the scene a browser serves behaves like the scene the protocol serves.

use unlit_wgpu_test_util::{Ctx, Tolerance, assert_image_snapshot_with_tolerance, snapshot};
use unlit3dx_examples::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};
use unlit3dx_examples::interactive::{InteractiveConfig, run as interactive_run, scene_status};
use unlit3dx_examples::scene::WINDOW_SIZE;

use crate::mcp_input::shared;

/// The frame captures' comparison tolerance for a browser, which draws
/// through software WebGL2. The headless suite measured that backend against
/// the Vulkan capture these baselines came from: it loses a few points on the
/// scene's hard edges, so the floor sits below its worst correct frame
/// and the outlier share is the gate that catches a shifted picture.
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(52.0),
    max_outliers: Some(0.01),
    channel_delta: 8,
};

/// Drive the interactive run the way the native MCP driver does: wait for the
/// panel, drop balls, play the sequence, record the mix, and compare the
/// frames and the sound against the stored baselines.
pub async fn run() {
    let ctx = Ctx::headless().await;
    let soundfont = shared::soundfont_bytes().to_vec().into();
    let (host, _input, mut interactive) = interactive_run(InteractiveConfig {
        device: ctx.device,
        queue: ctx.queue,
        capabilities: ctx.capabilities,
        soundfont,
    });

    // The panel is laid out on the first frame the gameplay world ticks.
    interactive.step(&host, 1);
    let buttons = shared::button_centres(&host, interactive.worlds_handle());
    let status = scene_status(&host, interactive.worlds_handle());
    assert_eq!(status.balls, 0, "a fresh scene holds no balls");
    assert_eq!(
        status.sequence.map(|state| state.0),
        Some(true),
        "the sequence starts paused"
    );

    // A recording before anything plays captures silence.
    interactive.start_recording(&host, shared::RECORDING_SECONDS);
    interactive.step(&host, shared::WARMUP_FRAMES);
    let silent = interactive
        .stop_recording(&host)
        .expect("the idle recording stops");
    assert!(
        silent.samples.iter().all(|sample| sample.abs() == 0.0),
        "an idle scene records silence",
    );

    // One ball, one frame set.
    shared::click(&host, &mut interactive, buttons["Drop the next ball"]);
    interactive.step(&host, shared::SETTLE_FRAMES);
    let status = scene_status(&host, interactive.worlds_handle());
    assert_eq!(status.balls, 1, "one drop lands one ball");
    assert_eq!(
        status.sequence.map(|state| state.0),
        Some(true),
        "dropping does not play the sequence"
    );
    let first = interactive.render_frame(&host);
    assert_image_snapshot_with_tolerance(
        snapshot!("unlit3dx/mcp_input_1.webp"),
        &first,
        WINDOW_SIZE.0,
        WINDOW_SIZE.1,
        FRAME_TOLERANCE,
    );

    // Two more balls and the one-shot sequence, recorded while playing.
    interactive.start_recording(&host, shared::RECORDING_SECONDS);
    shared::click(&host, &mut interactive, buttons["Drop the next ball"]);
    shared::click(&host, &mut interactive, buttons["Drop the next ball"]);
    shared::click(&host, &mut interactive, buttons["Play"]);
    interactive.step(&host, shared::PLAY_FRAMES);
    let status = scene_status(&host, interactive.worlds_handle());
    assert_eq!(status.balls, 3, "three drops land three balls");
    assert_eq!(
        status.sequence.map(|state| state.0),
        Some(false),
        "the sequence plays"
    );
    let second = interactive.render_frame(&host);
    assert_image_snapshot_with_tolerance(
        snapshot!("unlit3dx/mcp_input_2.webp"),
        &second,
        WINDOW_SIZE.0,
        WINDOW_SIZE.1,
        FRAME_TOLERANCE,
    );
    let playing = interactive
        .stop_recording(&host)
        .expect("the playing recording stops");
    assert_eq!(
        playing.format.sample_rate, AUDIO_SAMPLE_RATE,
        "the mix is pulled at the run's rate"
    );
    assert_eq!(
        usize::from(playing.format.channels),
        AUDIO_CHANNELS,
        "the mix is stereo"
    );
    let peak = playing
        .samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > shared::MIN_PEAK, "the sequence sounds: peak {peak}");
    shared::check_audio_baseline("unlit3dx/mcp_input_audio.flac", &playing.samples);

    // Cleared and paused, the panel is back where it started.
    shared::click(&host, &mut interactive, buttons["Clear the balls"]);
    shared::click(&host, &mut interactive, buttons["Pause"]);
    interactive.step(&host, shared::RELEASE_FRAMES);
    let status = scene_status(&host, interactive.worlds_handle());
    assert_eq!(status.balls, 0, "the clear takes every ball back");
    assert_eq!(
        status.sequence.map(|state| state.0),
        Some(true),
        "the pause holds"
    );
    interactive.start_recording(&host, shared::RECORDING_SECONDS);
    interactive.step(&host, shared::WARMUP_FRAMES);
    let paused = interactive
        .stop_recording(&host)
        .expect("the paused recording stops");
    assert!(
        paused.samples.iter().all(|sample| sample.abs() == 0.0),
        "a paused sequence records silence",
    );
    let third = interactive.render_frame(&host);
    assert_image_snapshot_with_tolerance(
        snapshot!("unlit3dx/mcp_input_3.webp"),
        &third,
        WINDOW_SIZE.0,
        WINDOW_SIZE.1,
        FRAME_TOLERANCE,
    );
}
