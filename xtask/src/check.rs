//! `cargo xtask check`: clippy over the workspace, then a formatting check.

use crate::step;

/// Run clippy over every workspace target, then check formatting.
///
/// Clippy first: its warnings and the diagnostics it raises may leave the
/// tree in a state `rustfmt` would rewrite, so formatting is the last word.
pub fn run(release: bool) -> Result<(), String> {
    let profile: &[&str] = if release { &["--release"] } else { &[] };
    let mut clippy = std::process::Command::new("cargo");
    clippy
        .args(["clippy", "--workspace", "--all-targets", "--all-features"])
        .args(profile);
    step::run(&mut clippy, "clippy")?;

    let mut fmt = std::process::Command::new("cargo");
    fmt.args(["fmt", "--all", "--", "--check"]);
    step::run(&mut fmt, "fmt")?;

    Ok(())
}
