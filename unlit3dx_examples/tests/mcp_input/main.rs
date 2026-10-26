//! Drive the real example process through MCP and snapshot its visible and
//! audible results.
//!
//! The child has no window and advances only when the client asks for frames.
//! Input still crosses the normal MCP/event/egui path; this client never clicks
//! by calling scene code. Recordings cross MCP in memory, so the silence check
//! writes no audio.
//!
//! That body lives in [`native`], because it starts a process and reads its
//! standard streams. Registering it `#[host_only]` keeps it off the list the
//! browser is offered.

use unlit_wgpu_test_util::gpu_tests;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod native;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn the_mcp_input_and_audio_match_the_snapshots() {
    native::run().await;
}

gpu_tests! {
    #[host_only]
    the_mcp_input_and_audio_match_the_snapshots,
}
