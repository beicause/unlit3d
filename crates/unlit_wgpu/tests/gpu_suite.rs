//! The integration suite for `unlit_wgpu`: every GPU test file in one binary.
//!
//! The files below are modules of a single test target, sharing one
//! compilation of [common]; the repository's `TESTING.md`, in its `Test layers`
//! section, explains why a crate's integration tests are one binary.
//!
//! The manifest states the feature this binary needs (`egui`) rather than the
//! modules repeating `#[cfg(feature = ...)]`: a target whose required feature is
//! off is skipped whole.

mod common;

mod gpu_egui;
mod gpu_lit;
mod gpu_resources;

unlit_wgpu_test_util::gpu_test_main!(
    [
        gpu_egui::all_tests(),
        gpu_lit::all_tests(),
        gpu_resources::all_tests()
    ]
    .concat()
);
