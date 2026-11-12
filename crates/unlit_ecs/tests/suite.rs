//! The integration suite for `unlit_ecs`: every test file in one binary.
//!
//! The files below are modules of a single test target, which libtest's
//! generated harness drives; the root README's `Tests and benchmarks` section
//! explains why a crate's integration tests are one binary.

mod behavior;
mod bundle;
mod commands;
mod errors;
mod lifecycle;
mod names;
mod query;
mod scope;
mod world;
