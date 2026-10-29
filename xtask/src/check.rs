//! `cargo xtask check`: the typo and TOML checks, clippy, the doc build, then a
//! formatting check.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::step;

/// Run the typo and TOML checks, clippy over every workspace target, the doc
/// build, then the formatting check.
///
/// The two checks that need no compiler go first, so a misspelling or an
/// unsorted feature table costs a second rather than the minutes clippy takes.
///
/// Clippy next: its warnings and the diagnostics it raises may leave the tree
/// in a state `rustfmt` would rewrite, so formatting is the last word. The doc
/// build sits between the two because a rustdoc warning is a lint like clippy's
/// — it should fail here rather than only in CI — while a doc comment it would
/// rewrite is still rustfmt's to settle.
///
/// Clippy runs three times, because each builds a different set of code:
///
/// 1. The workspace with its default features. This is the combination the
///    platform artifacts are built with, where every crate is compiled on its
///    own: a package's default features hold unless that package is named
///    directly, so an optional feature that is the only user of an item is
///    *off* in the one crate that owns the item and *on* everywhere else, and
///    the item reads as dead code. `--all-features` cannot see this: it turns
///    the feature on in that crate too, and the workspace run as a whole
///    unifies features across its members, so the item is used and the
///    diagnostic never fires.
/// 2. Every package on its own, default features, library targets only. Naming
///    a package stops the unification that hid the combination above, so this
///    is the pass that catches it. Library targets are what the artifacts
///    ship; a unit test inside a library counts as a use, so leaving the test
///    targets out matters rather than being an omission.
/// 3. The whole workspace with every feature, which is what the doc build and
///    the full test run compile.
///
/// Every run denies warnings, as CI does. Without it a warning here is a
/// warning CI turns into an error, which is the same gap in a different guise.
pub fn run(release: bool) -> Result<(), String> {
    let root = workspace_root()?;

    // Both tools take the directory to work on as their only argument, and both
    // are run from here rather than from the working directory: a task may be
    // started from anywhere inside the workspace, and these two read their
    // configuration — `typos.toml`, `tombi.toml` — from where they are pointed.
    require("typos", "cargo install typos-cli")?;
    require("tombi", "cargo install tombi")?;

    let mut typos = Command::new("typos");
    typos.arg(&root);
    step::run(&mut typos, "typos")?;

    // `lint` and `format --check` are separate invocations because they report
    // different things: a schema violation and a file the formatter would
    // rewrite. The lint denies warnings, as CI does, so the two runs agree.
    let mut tombi_lint = Command::new("tombi");
    tombi_lint.args(["lint", "--error-on-warnings"]).arg(&root);
    step::run(&mut tombi_lint, "tombi lint")?;

    let mut tombi_format = Command::new("tombi");
    tombi_format
        .args(["format", "--check", "--diff"])
        .arg(&root);
    step::run(&mut tombi_format, "tombi format")?;

    let profile: &[&str] = if release { &["--release"] } else { &[] };
    let deny: &[&str] = &["--", "-D", "warnings"];

    let mut default = Command::new("cargo");
    default
        .args(["clippy", "--workspace", "--all-targets"])
        .args(profile)
        .args(deny);
    step::run(&mut default, "clippy (default features)")?;

    for package in libraried_packages()? {
        let mut one = Command::new("cargo");
        one.args(["clippy", "-p", &package, "--lib"])
            .args(profile)
            .args(deny);
        step::run(&mut one, &format!("clippy -p {package} (default features)"))?;
    }

    let mut all = Command::new("cargo");
    all.args(["clippy", "--workspace", "--all-targets", "--all-features"])
        .args(profile)
        .args(deny);
    step::run(&mut all, "clippy (all features)")?;

    // The doc build, so a broken intra-doc link or an undocumented private item
    // fails here rather than only in CI. `cargo doc` has no `--` for handing
    // flags to rustdoc — unlike clippy — so denying warnings goes through the
    // environment, as CI does it. Every feature, because that is the build the
    // published documentation is generated from: an item behind an optional
    // feature is one whose docs only that combination compiles.
    let mut doc = Command::new("cargo");
    doc.args([
        "doc",
        "--workspace",
        "--no-deps",
        "--document-private-items",
        "--keep-going",
        "--all-features",
    ])
    .args(profile)
    .env("RUSTDOCFLAGS", "-D warnings");
    step::run(&mut doc, "doc")?;

    let mut fmt = Command::new("cargo");
    fmt.args(["fmt", "--all", "--", "--check"]);
    step::run(&mut fmt, "fmt")?;

    Ok(())
}

/// Fail with the command that installs it when `tool` is not on `PATH`.
///
/// Neither tool is a cargo subcommand, so unlike nextest nothing else here
/// installs them; without this the step fails on a bare "No such file or
/// directory", which names the symptom but not the fix.
fn require(tool: &str, install: &str) -> Result<(), String> {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|_| ())
        .map_err(|error| {
            format!(
                "running `{tool} --version`: {error}\n\
                 install it with `{install}`"
            )
        })
}

/// The directory the task's tools work on, which is the workspace root.
///
/// Asked of cargo rather than read from the working directory, so that a task
/// started from a crate's own directory covers the whole workspace: these
/// tools read their configuration from the directory they are pointed at, and
/// `typos.toml` and `tombi.toml` live at the root.
fn workspace_root() -> Result<PathBuf, String> {
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

/// The workspace's own packages that have a library target, from
/// `cargo metadata`.
///
/// Asked of cargo rather than read out of the root manifest: the members are
/// globs (`crates/*`) the manifest does not expand, and a package added to a
/// new location should be linted without anyone remembering to list it here.
/// A package with no library — a binary or a benchmark crate — is left out
/// because `--lib` is an error for it rather than an empty lint.
fn libraried_packages() -> Result<Vec<String>, String> {
    let mut metadata = Command::new("cargo");
    metadata.args(["metadata", "--format-version", "1", "--no-deps"]);
    let output = metadata
        .output()
        .map_err(|error| format!("running `cargo metadata`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parsing `cargo metadata`'s output: {error}"))?;
    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "`cargo metadata` listed no packages".to_owned())?;

    let mut names = Vec::with_capacity(packages.len());
    for package in packages {
        // A dependency is not this workspace's to lint, and only a workspace
        // package has no source, which is what marks one: a registry or path
        // dependency names where it came from.
        if package
            .get("source")
            .is_some_and(|source| !source.is_null())
        {
            continue;
        }
        if !has_library_target(package) {
            continue;
        }
        let name = package
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "a workspace package has no name".to_owned())?;
        names.push(name.to_owned());
    }
    Ok(names)
}

/// Whether `package` builds a library `--lib` can name.
///
/// The kinds are cargo's own: a library is `lib`, or one of the crate types
/// a `[lib]` section may instead ask for.
fn has_library_target(package: &serde_json::Value) -> bool {
    const LIBRARY_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];

    package
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|targets| {
            targets.iter().any(|target| {
                target
                    .get("kind")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|kinds| {
                        kinds.iter().any(|kind| {
                            kind.as_str()
                                .is_some_and(|kind| LIBRARY_KINDS.contains(&kind))
                        })
                    })
            })
        })
}
