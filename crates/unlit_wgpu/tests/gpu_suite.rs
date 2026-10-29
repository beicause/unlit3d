//! The integration suite for `unlit_wgpu`: both GPU test files in one binary.
//!
//! Cargo links one binary per test target, and each of these files used to be a
//! target of its own, so each paid for a link of the whole GPU stack. Here they
//! are modules of a single target, sharing one compilation of [common].
//!
//! The manifest states the features this binary needs (`lit`, `egui`) rather
//! than the modules repeating `#[cfg(feature = ...)]`: a target whose required
//! features are off is skipped whole.

mod common;

#[path = "gpu_lit.rs"]
mod gpu_lit;
#[path = "gpu_ui.rs"]
mod gpu_ui;

unlit_wgpu_test_util::gpu_test_main!([gpu_ui::all_tests(), gpu_lit::all_tests()].concat());
