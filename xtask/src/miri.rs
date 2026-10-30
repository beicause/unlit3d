//! `cargo xtask miri`: the ECS's tests under Miri, which is what checks the
//! `unsafe` in its storage.

use crate::step;

/// Run the ECS's tests, unit and integration and doctests alike, under Miri.
///
/// The world keeps its component values in `UnsafeCell`s and hands out plain
/// references to them through `&World`, so the aliasing rules the compiler
/// would otherwise enforce are the borrow ledger's to keep at runtime. A
/// mistake there is undefined behaviour rather than a panic, and no test can
/// see it: the release build reads the same bytes as the debug one. Miri is the
/// interpreter that does check it, so this is the gate for that code, and
/// `cargo xtask test` cannot stand in for it.
///
/// Miri runs the suite its own way: every test is interpreted, which is one to
/// two orders of magnitude slower, so it names the one crate whose storage is
/// unsafe instead of the workspace. `--all-features` is deliberate — the
/// optional `mcp` module reads components through the same queries, so it
/// belongs in the pass that audits them.
///
/// The toolchain is nightly's: Miri is not stable, and its sysroot is built
/// against the nightly it ships with. `rustup component add miri --toolchain
/// nightly` is what installs it; a missing one is reported as the step failing,
/// with Miri's own message naming the component.
pub fn run(_task: &crate::MiriArgs) -> Result<(), String> {
    let mut command = std::process::Command::new("cargo");
    command.args([
        "+nightly",
        "miri",
        "test",
        "-p",
        "unlit_ecs",
        "--all-features",
        "--no-fail-fast",
    ]);
    step::run(&mut command, "miri")
}
