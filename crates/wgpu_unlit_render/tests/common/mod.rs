//! Shared harness for the crate's GPU integration tests.
//!
//! This module is not a test binary itself — it is included by per-topic
//! test files (`tests/*.rs`).

#![expect(unused_imports, reason = "different test files use different subsets")]

pub use wgpu_unlit_test_util::{
    Ctx, Frame, assert_image_snapshot, assert_image_snapshot_with_threshold, bg_entry,
    count_pixels_off_background, read_texture_bytes, readback_buffer, rgb, srgb_to_linear_u8,
    texel_bytes,
};
