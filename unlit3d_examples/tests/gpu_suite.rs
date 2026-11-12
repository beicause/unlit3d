//! The integration suite for `unlit3d_examples`: both GPU test files in one
//! binary.
//!
//! The files below are modules of a single test target; the root README's
//! `Tests and benchmarks` section explains why a crate's integration tests are
//! one binary.

#[path = "gpu_scenes.rs"]
mod gpu_scenes;
#[path = "selector_position.rs"]
mod selector_position;

unlit_wgpu_test_util::gpu_test_main!(
    [gpu_scenes::all_tests(), selector_position::all_tests()].concat()
);
