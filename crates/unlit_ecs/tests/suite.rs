//! The integration suite for `unlit_ecs`: every test file in one binary.
//!
//! The files below are modules of a single test target, which libtest's
//! generated harness drives; the root README's `Tests and benchmarks` section
//! explains why a crate's integration tests are one binary.

#[path = "behavior.rs"]
mod behavior;
#[path = "bundle.rs"]
mod bundle;
#[path = "commands.rs"]
mod commands;
#[path = "errors.rs"]
mod errors;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "names.rs"]
mod names;
#[path = "query.rs"]
mod query;
#[path = "scope.rs"]
mod scope;
#[path = "world.rs"]
mod world;
