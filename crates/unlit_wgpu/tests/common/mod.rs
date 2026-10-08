//! Shared harness for the crate's GPU integration tests.
//!
//! This module is not a test binary itself — it is included by per-topic
//! test files (`tests/*.rs`).

// Re-exported wholesale: each test file uses a different subset of the
// harness, and a glob re-export is the one form unused_imports does not
// flag per binary.
pub use unlit_wgpu_test_util::*;
