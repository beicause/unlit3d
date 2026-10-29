//! `cargo xtask print-gpu`: describe the device the tests draw with.
//!
//! A snapshot mismatch is usually a disagreement between drivers, so a CI log
//! that names the backend, adapter, features and limits is what turns a score
//! into something a reader can act on. The task runner links no graphics code,
//! so the description comes from `unlit_wgpu_test_util`'s `gpu-info` binary,
//! which is built and run here.
//!
//! A test run draws through two tiers -- the default one, then `webgl2` -- so
//! both are described, each labelled with the tier it belongs to.

use std::process::Command;

use crate::{PrintGpuArgs, step};

/// The environment variable that picks the device tier.
///
/// Repeated from `unlit_wgpu_test_util`, which this crate does not depend on.
const TIER_ENV: &str = "UNLIT3D_DEVICE_TIER";

/// The tiers `cargo xtask test` runs its passes at, in the order it runs them.
///
/// The first is the default tier, named by its `UNLIT3D_DEVICE_TIER` spelling so
/// a reader can look the name up; the second asks only for what a browser
/// grants, so the two describe different capability sets even on one device.
const TEST_TIERS: &[&str] = &["webgpu", "webgl2"];

/// Print the device a test run at this tier would draw with.
pub fn run(args: &PrintGpuArgs) -> Result<(), String> {
    match &args.tier {
        Some(tier) => print_tier(tier),
        None => TEST_TIERS.iter().try_for_each(|tier| print_tier(tier)),
    }
}

/// Print the device one tier would draw with.
fn print_tier(tier: &str) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command.args([
        "run",
        "--quiet",
        "-p",
        "unlit_wgpu_test_util",
        "--bin",
        "gpu-info",
    ]);
    command.env(TIER_ENV, tier);
    step::run(&mut command, &format!("printing the device ({tier})"))
}
