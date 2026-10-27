//! Drive the example scene through the input path and snapshot its visible and
//! audible results.
//!
//! The native driver starts the real example process and speaks MCP to it: the
//! child has no window and advances only when the client asks for frames, so
//! the protocol is the only thing that can move it. Recordings cross MCP in
//! memory, so the silence check writes no audio.
//!
//! The wasm driver has no process to talk to and no transport thread, so it
//! drives the interactive run in-process: the same clicks and steps, the same
//! baselines. What the pair proves is that a browser serves the scene the
//! protocol serves.

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod native;

#[cfg(not(target_os = "android"))]
mod wasm;

mod shared;

use unlit_wgpu_test_util::gpu_tests;

/// The body the native runner drives: the story over the real transport.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn the_mcp_input_and_audio_match_the_snapshots() {
    native::run().await;
}

/// The body a browser drives: the same story, in-process. Compiled on the
/// host as well — with nothing wasm-specific inside, the proxy needs it
/// registered there to list the browser trial.
#[cfg(not(target_os = "android"))]
async fn the_interactive_scene_matches_the_snapshots() {
    wasm::run().await;
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
gpu_tests! {
    #[host_only]
    the_mcp_input_and_audio_match_the_snapshots,
    the_interactive_scene_matches_the_snapshots,
}

#[cfg(target_arch = "wasm32")]
gpu_tests! {
    the_interactive_scene_matches_the_snapshots,
}
