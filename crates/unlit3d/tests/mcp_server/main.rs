//! End-to-end test: drive the MCP server over a pair of streams, the way an
//! MCP client does.
//!
//! The server runs in this process on its own thread, speaking
//! newline-delimited JSON-RPC to a world that lives on a second thread. The
//! test asks it to build a mesh, tint the clear colour and screenshot the
//! offscreen target, so the whole path — protocol, host, render, readback — is
//! exercised, not just the pieces in isolation.
//!
//! That body lives in [`native`], because the protocol and the modules it
//! reaches are native-only. Registering it `#[host_only]` keeps it out of the
//! wasm suite, and this file still carries the registry and `gpu_test_main!`
//! so the wasm build has the `run_test` export `wasm-bindgen` binds — a test
//! binary compiled with no bindings at all is one the CLI refuses.

use unlit_wgpu_test_util::{gpu_test_main, gpu_tests};

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod native;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn the_server_lists_its_tools_and_renders() {
    native::run();
}

// The registry both runners drive: `cargo nextest` natively, and a browser
// through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    #[host_only]
    the_server_lists_its_tools_and_renders,
}

gpu_test_main!(all_tests());
