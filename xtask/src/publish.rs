//! `cargo xtask publish`: publish the workspace's crates in dependency order.
//!
//! A crate cannot be packaged until the crates it depends on are in the
//! registry, so the order is not a preference — publishing `unlit3d` before
//! `unlit_wgpu` fails with `no matching package named unlit_wgpu found`. The
//! order is written down here rather than left to the caller, and each
//! `cargo publish` waits for the crate it uploaded to appear in the index, so
//! the next one's dependencies resolve by the time it is packaged.
//!
//! `cargo publish --workspace` orders the crates itself, but its `--dry-run`
//! cannot verify them (rust-lang/cargo#16525), and a release is worth checking
//! before it is uploaded; going crate by crate keeps that check.
//!
//! The test harness, the example and the task runner are `publish = false`, so
//! they are not in the list and are never uploaded.

use std::process::Command;

use super::{PublishArgs, step};

/// The publishable crates, in the order publishing requires.
///
/// `unlit3d` depends on the other two and so goes last. A crate added to the
/// workspace belongs here if and only if it is meant to reach crates.io.
const CRATES: [&str; 3] = ["unlit_ecs", "unlit_wgpu", "unlit3d"];

/// Publish every crate, in order.
pub fn run(args: &PublishArgs) -> Result<(), String> {
    for (index, krate) in CRATES.iter().enumerate() {
        let mut cargo = Command::new("cargo");
        cargo.args(["publish", "-p", krate]);
        if args.dry_run {
            cargo.arg("--dry-run");
        }

        let step_name = format!("publishing `{krate}`");
        if let Err(error) = step::run(&mut cargo, &step_name) {
            // A real publish is not repeatable: cargo refuses a version that is
            // already on the registry, so a run that stopped partway has to be
            // resumed at the crate that failed.
            return Err(if index == 0 || args.dry_run {
                error
            } else {
                format!(
                    "{error}\nthe crates before `{krate}` were uploaded; resume with \
                     `cargo publish -p {krate}`"
                )
            });
        }
    }

    Ok(())
}
