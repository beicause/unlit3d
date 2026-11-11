//! `cargo xtask check`: the typo and TOML checks, clippy, the doc build, then a
//! formatting check.

use std::process::Command;

use crate::step;

/// Run the typo and TOML checks, clippy twice, the doc build, then the
/// formatting check.
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
/// Clippy runs twice, because each compiles a different set of code: the
/// library crates with no default features at all, and the whole workspace
/// with every feature, which is what the doc build and the full test run
/// compile.
///
/// The no-features pass is the one the test run cannot stand in for. Those
/// crates' built-in backends are optional, so this is the combination that
/// proves the general facilities stand on their own — and the one a false
/// optional feature, whose dependants never really compiled without it, fails
/// to compile. The crates are named one by one rather than picked with
/// `--workspace`, because `--no-default-features` reaches only the packages
/// the command names: for every other package the features come from the
/// dependency edges that reach it, and feature unification applies all of
/// them. Every application crate asks `unlit3d` for `ui` and `winit`, so a
/// workspace-wide run would compile the libraries with exactly the backends
/// this pass exists to leave out.
///
/// Every run denies warnings, as CI does. Without it a warning here is a
/// warning CI turns into an error, which is the same gap in a different guise.
pub fn run(release: bool) -> Result<(), String> {
    let root = step::workspace_root()?;

    // Both tools take the directory to work on as their only argument, and both
    // are run from here rather than from the working directory: a task may be
    // started from anywhere inside the workspace, and `typos` reads the
    // `typos.toml` beside the files it is pointed at. `tombi` keeps no config
    // file: it validates every TOML file against the schemas its catalog names.
    require("typos")?;
    require("tombi")?;

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

    let mut bare = Command::new("cargo");
    bare.args(["clippy"]);
    for package in BARE_PACKAGES {
        bare.args(["-p", package]);
    }
    bare.args(["--no-default-features", "--all-targets"])
        .args(profile)
        .args(deny);
    step::run(&mut bare, "clippy (no features)")?;

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

/// The crates the no-features clippy run names, one by one.
///
/// These are the library crates with optional backends: their general
/// facilities have to compile and lint on their own, without the backends a
/// caller may leave out. Naming them keeps the feature graph to the edges
/// between the four, which ask for nothing, because the application crates
/// are left out — each of them asks `unlit3d` for `ui` and `winit`, and
/// feature unification would hand those to the libraries too. The same reason
/// rules out `--workspace`: `--no-default-features` does not follow a
/// dependency edge.
const BARE_PACKAGES: [&str; 4] = ["unlit_ecs", "unlit_wgpu", "unlit3d", "unlit3dx"];

/// Fail, naming the tool, when `tool` is not usable.
///
/// Neither tool ships with cargo, so a missing one reaches the step as a bare
/// "No such file or directory" under a name that says what the step wanted;
/// this reports which tool is absent instead. How to install it is left to
/// the contributor: both are distributed outside cargo, so a command named
/// here would be a guess.
fn require(tool: &str) -> Result<(), String> {
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .map_err(|error| format!("`{tool}` is not on PATH: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{tool} --version` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
