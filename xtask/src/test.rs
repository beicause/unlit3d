//! `cargo xtask test`: nextest over the workspace, then the doctests.

use crate::step;

/// Run the workspace's tests.
///
/// `cargo nextest run` is the test runner: it gives every test its own
/// process, so one test's device, logger or panic cannot reach another's.
/// Nextest does not run doctests, so those get a `cargo test --doc` pass of
/// their own — the two commands together are what CI runs.
pub fn run(release: bool) -> Result<(), String> {
    let profile: &[&str] = if release { &["--release"] } else { &[] };

    let mut nextest = std::process::Command::new("cargo");
    nextest
        .args([
            "nextest",
            "run",
            "--workspace",
            "--all-targets",
            "--all-features",
        ])
        .args(profile);
    step::run(&mut nextest, "nextest")?;

    let mut doctests = std::process::Command::new("cargo");
    doctests
        .args(["test", "--workspace", "--all-features", "--doc"])
        .args(profile);
    step::run(&mut doctests, "doctests")?;

    Ok(())
}
