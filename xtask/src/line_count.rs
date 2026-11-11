//! `cargo xtask line-count`: how long each Rust file in the workspace is.
//!
//! A file that grows past a thousand lines holds several unrelated concerns in
//! one place, and reading it in parts invites a missed half and a misjudged
//! whole. The repository's conventions put a ceiling there, and this task names
//! the files that reach it.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{LineCountArgs, step};

/// The length at which a file is over the ceiling.
const CEILING: usize = 1000;

/// Print the line count of every Rust file in the workspace, longest first,
/// marking the ones that reach the ceiling.
///
/// The files are git's, not the directory walk's: `git ls-files` lists what the
/// repository actually holds, so build output, `target/` and anything else
/// ignored, the contents of the asset submodule, and a stray copy under some
/// other worktree's checkout are all absent without a list of names to skip —
/// which would fall out of step with `.gitignore` the first time it changed. An
/// untracked file is still listed, because it is one `git add` from joining the
/// tree.
///
/// Reporting rather than failing: the files over the ceiling today are the ones
/// still to be split, and a task that is red until then would be ignored.
pub fn run(_task: &LineCountArgs) -> Result<(), String> {
    let root = step::workspace_root()?;

    let mut counted = Vec::new();
    for path in sources(&root)? {
        let full = root.join(&path);
        // The index keeps a deleted file until the deletion is staged, so a
        // path that is no longer on disk is a file the workspace no longer has.
        if !full.exists() {
            continue;
        }
        let lines = measure(&full)?;
        counted.push((lines, path));
    }
    // Longest first: the files that need splitting are what a reader is looking
    // for, and the rest are there to be scanned.
    counted.sort_by(|(lines, path), (other, other_path)| {
        other.cmp(lines).then_with(|| path.cmp(other_path))
    });

    let mut total = 0;
    let mut over = 0;
    for (lines, path) in &counted {
        total += lines;
        let mark = if *lines >= CEILING {
            over += 1;
            "  over"
        } else {
            ""
        };
        println!("{lines:>6}  {}{mark}", path.display());
    }
    println!();
    println!(
        "{files} files, {total} lines, {over} at or over {CEILING}",
        files = counted.len()
    );
    Ok(())
}

/// Every Rust file git lists in the workspace, relative to the root.
///
/// `--cached` is the index, `--others` adds the untracked files,
/// `--exclude-standard` drops the ignored ones, and `--deduplicate` keeps a file
/// that is both from being listed twice. `-z` makes the names unambiguous: a
/// path may contain a newline, so they are separated by NUL rather than by lines.
fn sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut git = Command::new("git");
    git.args([
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "--deduplicate",
        "-z",
        "--",
        "*.rs",
    ]);
    git.current_dir(root);
    let output = git
        .output()
        .map_err(|error| format!("running `git ls-files`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git ls-files` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let names = String::from_utf8(output.stdout)
        .map_err(|error| format!("reading `git ls-files`'s output: {error}"))?;
    Ok(names.split_terminator('\0').map(PathBuf::from).collect())
}

/// The line count of a file.
///
/// Counted in bytes rather than decoded as a string: every Rust file is UTF-8,
/// and a newline byte cannot occur inside a multi-byte sequence. A last line
/// with no newline still counts.
fn measure(path: &Path) -> Result<usize, String> {
    let bytes = fs::read(path).map_err(|error| format!("reading {path:?}: {error}"))?;
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    match bytes.last() {
        Some(b'\n') | None => Ok(newlines),
        Some(_) => Ok(newlines + 1),
    }
}
