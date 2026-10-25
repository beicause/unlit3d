//! The integration suite for `unlit_ecs`: every test file in one binary.
//!
//! Cargo links one binary per test target, so these files used to be nine
//! targets, each linking the crate on its own. Here they are modules of a
//! single target, which libtest's generated harness drives as usual.

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
