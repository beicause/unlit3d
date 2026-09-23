//! `cargo xtask check`: clippy over the workspace, then a formatting check.

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
    run_step(&mut clippy, "clippy")?;

    let mut fmt = std::process::Command::new("cargo");
    fmt.args(["fmt", "--all", "--", "--check"]);
    run_step(&mut fmt, "fmt")?;

    Ok(())
}

/// Run `command`, reporting the step that failed.
fn run_step(command: &mut std::process::Command, step: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("running {step}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{step} failed; fix the diagnostics above and run again"
        ))
    }
}
