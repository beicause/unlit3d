//! The integration suite for `unlit3dx`: every test file in one binary.
//!
//! Cargo links one binary per test target, so these files used to be four
//! targets, each linking the multi-world stack on its own. Here they are
//! modules of a single target, which libtest's generated harness drives as
//! usual.
//!
//! The manifest states the features this binary needs (`audio`, `physics`,
//! `synth`) rather than each module gating its own tests: a target whose
//! required features are off is skipped whole.

#[path = "audio.rs"]
mod audio;
#[path = "multi_world.rs"]
mod multi_world;
#[path = "physics.rs"]
mod physics;
#[path = "subscriptions.rs"]
mod subscriptions;
