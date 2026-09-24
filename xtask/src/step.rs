//! Running one step of a task as a child command.

use std::process::Command;

/// Run `command`, reporting which step failed.
///
/// A step inherits the terminal, so the tool's own output — clippy's
/// diagnostics, nextest's progress — reaches the contributor unchanged.
pub fn run(command: &mut Command, step: &str) -> Result<(), String> {
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
