//! Repository task runner.
//!
//! Every task is a `cargo xtask <name>` invocation. The tasks a contributor
//! reaches for day to day:
//!
//! * `cargo xtask check` — clippy and fmt, the bar a commit is held to.
//! * `cargo xtask test` — the test suite, through nextest.
//! * `cargo xtask run-wasm` — build and serve the web example.
//!
//! The crate is not a workspace member; it is wired up through the `xtask`
//! alias in `.cargo/config.toml`, so the task runner's dependencies never
//! weigh on the workspace the tasks act on.

mod check;
mod http;
mod run_wasm;
mod step;
mod test;

use argh::FromArgs;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Args = argh::from_env();
    match args.task {
        Task::Check(task) => check::run(task.release),
        Task::Test(task) => test::run(task.release),
        Task::RunWasm(task) => run_wasm::run(&task),
    }
}

/// The repository task runner.
#[derive(FromArgs)]
struct Args {
    /// the task to run.
    #[argh(subcommand)]
    task: Task,
}

/// The tasks `cargo xtask` offers.
///
/// argh derives each variant's subcommand name from its payload struct, which
/// must itself derive `FromArgs` and be declared as a subcommand.
#[derive(FromArgs)]
#[argh(subcommand)]
enum Task {
    /// Run clippy over the whole workspace, then check formatting.
    Check(CheckArgs),
    /// Run the workspace's tests.
    Test(TestArgs),
    /// Build and serve the web example.
    RunWasm(RunWasmArgs),
}

/// Arguments of `cargo xtask check`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "check",
    description = "run clippy over the whole workspace, then check formatting"
)]
struct CheckArgs {
    /// build in release mode.
    #[argh(switch)]
    release: bool,
}

/// Arguments of `cargo xtask test`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "test",
    description = "run the workspace's tests through nextest, then the doctests"
)]
struct TestArgs {
    /// build in release mode.
    #[argh(switch)]
    release: bool,
}

/// Arguments of `cargo xtask run-wasm`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "run-wasm",
    description = "build the web example and serve it for a browser"
)]
struct RunWasmArgs {
    /// build and bindgen only; do not serve.
    #[argh(switch)]
    no_serve: bool,
    /// build in release mode.
    #[argh(switch)]
    release: bool,
    /// extra arguments for the cargo build.
    #[argh(positional, greedy)]
    cargo_args: Vec<String>,
}
