//! The integration suite for `unlit3dx_examples`: every snapshot test the
//! example has, in one binary.
//!
//! Cargo links one binary per test target, and each of these files used to be a
//! target of its own. They share the same harness, so every one of them paid
//! for a link of the whole GPU stack. Here they are modules of a single target,
//! and the registry each builds with `gpu_tests!` is concatenated below and
//! handed to the runner once.
//!
//! The runner is `gpu_test_main!`: it drives the registry through libtest-mimic
//! natively, and exports the `run_test` entry point the wasm suite's page calls.

#[path = "headless.rs"]
mod headless;
#[path = "mcp_input/main.rs"]
mod mcp_input;
#[path = "support.rs"]
mod support;

unlit_wgpu_test_util::gpu_test_main!([headless::all_tests(), mcp_input::all_tests()].concat());
