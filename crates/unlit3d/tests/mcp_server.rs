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
//! reaches are native-only. Registering it `#[host_only]` keeps it off the
//! list the browser is offered. The suite's root is what compiles to a wasm
//! module carrying the `run_test` export `wasm-bindgen` binds — a test binary
//! built with no bindings at all is one the CLI refuses.

use unlit_wgpu_test_util::gpu_tests;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod native;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn the_server_lists_its_tools_and_renders() {
    native::run();
}

// This file's share of the suite's registry: the suite root concatenates
// it with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
gpu_tests! {
    #[host_only]
    the_server_lists_its_tools_and_renders,
}
