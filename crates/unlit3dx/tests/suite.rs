//! The integration suite for `unlit3dx`: every test file in one binary.
//!
//! The files below are modules of a single test target, which libtest's
//! generated harness drives; the repository's `TESTING.md`, in its `Test layers`
//! section, explains why a crate's integration tests are one binary.
//!
//! The manifest states the features this binary needs (`audio`, `physics`,
//! `synth`) rather than each module gating its own tests: a target whose
//! required features are off is skipped whole.

mod audio;
mod multi_world;
mod physics;
mod subscriptions;
