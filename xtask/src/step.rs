//! Running one step of a task as a child command.

use std::ffi::OsStr;
use std::process::Command;

/// Run `command`, reporting which step failed.
///
/// The step's name and the command line are printed first. A task's steps are
/// the commands it wraps, so seeing which one is running — and being able to
/// paste it back into a shell — is most of what makes a slow task legible.
///
/// A step inherits the terminal, so the tool's own output — clippy's
/// diagnostics, nextest's progress — reaches the contributor unchanged.
pub fn run(command: &mut Command, step: &str) -> Result<(), String> {
    println!("==> {step}");
    println!("    {}", describe(command));
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

/// The command line `command` would run, quoted the way a shell would need it,
/// so it can be pasted back into a terminal and run as-is.
pub fn describe(command: &Command) -> String {
    let mut line = quote(command.get_program());
    for argument in command.get_args() {
        line.push(' ');
        line.push_str(&quote(argument));
    }
    line
}

/// Quote one word for a shell, leaving it bare where it needs no quoting.
fn quote(word: &OsStr) -> String {
    let word = word.to_string_lossy();
    let bare = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./=:,+@".contains(c));
    if bare {
        word.into_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}
