#![doc = include_str!("../README.md")]
// A task runner's output is the point of running it: the child commands it
// wraps inherit the terminal, and the progress it reports itself goes to
// stdout and stderr. The workspace lints deny both everywhere else.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "reporting to the terminal is this binary's job"
)]

mod build_android;
mod check;
mod http;
mod print_gpu;
mod publish;
mod run_wasm;
mod step;
mod test;
mod test_wasm;

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
        Task::TestWasm(task) => test_wasm::run(&task),
        Task::RunWasm(task) => run_wasm::run(&task),
        Task::BuildAndroid(task) => build_android::run(&task),
        Task::Publish(task) => publish::run(&task),
        Task::PrintGpu(task) => print_gpu::run(&task),
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
    /// Run clippy and the doc build over the whole workspace, then check
    /// formatting.
    Check(CheckArgs),
    /// Run the workspace's tests.
    Test(TestArgs),
    /// Run the GPU tests in a real browser, through wasm and WebGL2.
    TestWasm(TestWasmArgs),
    /// Build and serve the web examples.
    RunWasm(RunWasmArgs),
    /// Build the Android example's shared library and the APK around it.
    BuildAndroid(BuildAndroidArgs),
    /// Publish the workspace's crates to crates.io, in dependency order.
    Publish(PublishArgs),
    /// Print the GPU device the tests draw with, as JSON.
    PrintGpu(PrintGpuArgs),
}

/// Arguments of `cargo xtask check`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "check",
    description = "run clippy and the doc build over the whole workspace, then check formatting"
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

/// Arguments of `cargo xtask test-wasm`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "test-wasm",
    description = "run the GPU tests in a real browser through wasm and WebGL2"
)]
struct TestWasmArgs {
    /// show the browser window, to watch a failure happen.
    #[argh(switch)]
    show: bool,
}

/// Arguments of `cargo xtask run-wasm`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "run-wasm",
    description = "build the web examples and serve them for a browser"
)]
struct RunWasmArgs {
    /// which examples to build and serve.
    #[argh(option, default = "WebApp::All")]
    app: WebApp,
    /// build and bindgen only; do not serve.
    #[argh(switch)]
    no_serve: bool,
    /// extra arguments for the cargo build.
    #[argh(positional, greedy)]
    cargo_args: Vec<String>,
}

/// Which web examples a wasm task covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, argh::FromArgValue)]
enum WebApp {
    /// the unlit3d example alone.
    Unlit3d,
    /// the unlit3dx example alone.
    Unlit3dx,
    /// the landing page and both examples.
    #[default]
    All,
}

/// Arguments of `cargo xtask build-android`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "build-android",
    description = "build the Android example's shared library and the APK around it"
)]
struct BuildAndroidArgs {
    /// build a release APK instead of a debug one.
    #[argh(switch)]
    release: bool,
}

/// Arguments of `cargo xtask print-gpu`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "print-gpu",
    description = "print the GPU device the tests draw with, as JSON"
)]
struct PrintGpuArgs {
    /// report only this tier: webgpu, webgl2 or native. Omit to report every
    /// tier `cargo xtask test` runs at.
    #[argh(option)]
    tier: Option<String>,
}

/// Arguments of `cargo xtask publish`.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "publish",
    description = "publish the workspace's crates to crates.io, in dependency order"
)]
struct PublishArgs {
    /// extra arguments for every `cargo publish`; pass them after `--`.
    #[argh(positional, greedy)]
    cargo_args: Vec<String>,
}
