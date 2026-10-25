//! Shared harness for the crate's GPU integration tests.
//!
//! This module is not a test file itself — it is included by the per-topic
//! modules the suite binary pulls in (`tests/*.rs`).

// Re-exported wholesale: each test file uses a different subset of the
// harness, and a glob re-export is the one form unused_imports does not
// flag per module.
pub use unlit_wgpu_test_util::*;
