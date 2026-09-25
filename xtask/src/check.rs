//! `cargo xtask check`: clippy over the workspace, then a formatting check.

use crate::step;

/// Run clippy over every workspace target, then check formatting.
///
/// Clippy first: its warnings and the diagnostics it raises may leave the
/// tree in a state `rustfmt` would rewrite, so formatting is the last word.
///
/// Clippy runs twice, over the default features and then over every feature,
/// because the two compile different code: a crate whose optional feature is
/// the only user of an item fails with that feature *off*. CI builds the
/// workspace with the default features, so leaving that combination unchecked
/// here would let a break reach CI that this command reports clean.
///
/// Every run denies warnings, as CI does. Without it a warning here is a
/// warning CI turns into an error, which is the same gap in a different guise.
pub fn run(release: bool) -> Result<(), String> {
    let profile: &[&str] = if release { &["--release"] } else { &[] };
    let deny: &[&str] = &["--", "-D", "warnings"];

    let mut default = std::process::Command::new("cargo");
    default
        .args(["clippy", "--workspace", "--all-targets"])
        .args(profile)
        .args(deny);
    step::run(&mut default, "clippy (default features)")?;

    let mut all = std::process::Command::new("cargo");
    all.args(["clippy", "--workspace", "--all-targets", "--all-features"])
        .args(profile)
        .args(deny);
    step::run(&mut all, "clippy (all features)")?;

    let mut fmt = std::process::Command::new("cargo");
    fmt.args(["fmt", "--all", "--", "--check"]);
    step::run(&mut fmt, "fmt")?;

    Ok(())
}
