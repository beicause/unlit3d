//! The integration suite for `unlit3d_examples`: both GPU test files in one
//! binary.
//!
//! Cargo links one binary per test target, and each of these files used to be a
//! target of its own, so each paid for a link of the whole GPU stack. Here they
//! are modules of a single target.

#[path = "gpu_scenes.rs"]
mod gpu_scenes;
#[path = "selector_position.rs"]
mod selector_position;

unlit_wgpu_test_util::gpu_test_main!(
    [gpu_scenes::all_tests(), selector_position::all_tests()].concat()
);
