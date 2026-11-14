//! The integration suite for `unlit3dx_examples`: every snapshot test the
//! example has, in one binary.
//!
//! The files below are modules of a single test target; the repository's
//! `TESTING.md`, in its `Test layers` section, explains why a crate's integration
//! tests are one binary. The registry each builds with `gpu_tests!` is
//! concatenated below and handed to the runner once.
//!
//! The runner is `gpu_test_main!`: it drives the registry through libtest-mimic
//! natively, and exports the `run_test` entry point the wasm suite's page calls.

mod headless;
mod mcp_input;
mod support;

unlit_wgpu_test_util::gpu_test_main!([headless::all_tests(), mcp_input::all_tests()].concat());
