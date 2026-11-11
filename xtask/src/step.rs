//! Running one step of a task as a child command.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

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
/// The directory a task's tools work on, which is the workspace root.
///
/// Asked of cargo rather than read from the working directory, so that a task
/// started inside one crate still covers the whole workspace: `typos` reads the
/// configuration beside the files it is pointed at, and `typos.toml` lives at
/// the root.
pub fn workspace_root() -> Result<PathBuf, String> {
    let mut metadata = Command::new("cargo");
    metadata.args(["locate-project", "--workspace", "--message-format", "plain"]);
    let output = metadata
        .output()
        .map_err(|error| format!("running `cargo locate-project`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo locate-project` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let manifest = String::from_utf8(output.stdout)
        .map_err(|error| format!("reading `cargo locate-project`'s output: {error}"))?;
    Path::new(manifest.trim())
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "`cargo locate-project` named a manifest with no directory".to_owned())
}
