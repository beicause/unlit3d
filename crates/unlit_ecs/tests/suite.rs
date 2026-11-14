//! The integration suite for `unlit_ecs`: every test file in one binary.
//!
//! The files below are modules of a single test target, which libtest's
//! generated harness drives; the repository's `TESTING.md`, in its `Test layers`
//! section, explains why a crate's integration tests are one binary.

mod behavior;
mod bundle;
mod commands;
mod errors;
mod lifecycle;
mod names;
mod query;
mod scope;
mod world;
