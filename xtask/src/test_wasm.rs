//! `cargo xtask test-wasm`: run the GPU tests in a real browser.
//!
//! The tests themselves are the same files `cargo nextest` runs natively — see
//! `unlit_wgpu_test_util`'s registry — but built for `wasm32-unknown-unknown`
//! and driven from a page. The pieces are:
//!
//! 1. The wasm test binaries, built by `cargo nextest list` so that the same
//!    target list, features and names are used as natively.
//! 2. `wasm-bindgen`, which turns each into a JS module exporting `run_test`.
//! 3. The test page, which loads one module and calls that export.
//! 4. A Playwright runner, which opens the page once per test.
//! 5. The *same* native test binaries, with `UNLIT3D_WASM_TEST` set, which turns
//!    them into a proxy: `cargo nextest` drives them, and each trial asks the
//!    runner to run that test in the browser. Nextest's listing, filtering and
//!    exit code then all work as they do for an ordinary test suite.
//!
//! Steps 1-4 set up the browser; step 5 is the part nextest actually runs.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{TestWasmArgs, step};

/// The packages whose tests can run in a browser.
///
/// Named rather than taken from the workspace, because these are the ones whose
/// test targets are wasm-test binaries. The example's own snapshot tests are
/// among them: its scenes run in the browser the same way the crates' tests do.
const PACKAGES: &[&str] = &["unlit_wgpu", "unlit3d", "unlit3d_examples"];

/// The target the tests are built for.
const TARGET: &str = "wasm32-unknown-unknown";

/// Where the page, the built modules and `wasm_paths.json` are assembled.
const DIST: &str = "tests/wasm/dist";

/// Where the built example is assembled for the runner to serve.
///
/// The runner serves the example from its own tree rather than reaching into
/// `target/`, so one directory holds everything the page needs and the runner
/// has no reason to know how the build laid it out.
const EXAMPLE_DIST: &str = "tests/wasm/example";

/// The page the runner serves, copied into [`DIST`] as-is.
const WEB: &str = "tests/wasm/web";

/// The Playwright runner, which serves [`DIST`] and drives a browser.
const RUNNER: &str = "tests/wasm/runner";

/// The environment variable that collects mismatched snapshot frames.
///
/// Defined by `unlit_wgpu_test_util` for the host pass, and read by the Node
/// runner for the browser one — which is the process that can write, since the
/// page has no filesystem. Repeated here as a literal because this is a
/// separate crate that does not depend on the harness.
const MISMATCH_DIR_ENV: &str = "UNLIT3D_SNAPSHOT_MISMATCH_DIR";

/// Where this pass collects the frames that failed their comparison.
///
/// A directory of its own, so a browser run's frames are not mixed in with the
/// host run's: the two can legitimately differ, and telling them apart is the
/// first thing a reader wants to do.
const MISMATCH_DIR: &str = "target/snapshot-mismatches-wasm";

/// The environment variable that puts a test binary in proxy mode.
///
/// Defined by `unlit_wgpu_test_util`, and repeated here as a literal because
/// this is a separate crate that does not depend on it — the same reason
/// `test.rs` repeats the device-tier variable.
const WASM_TEST_ENV: &str = "UNLIT3D_WASM_TEST";

/// The port the runner listens on; also fixed in the harness's proxy.
const PORT: u16 = 3000;

/// The runner's own address.
const BASE_URL: &str = "http://127.0.0.1:3000";

/// How long the runner is given to start listening.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the runner is given to close its browser and exit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the GPU tests in a browser.
///
/// Each phase is announced before it starts. Several of them compile for
/// minutes, and without a banner the terminal would sit silent for that whole
/// time, which reads as a hang rather than as work.
pub fn run(args: &TestWasmArgs) -> Result<(), String> {
    let started = Instant::now();

    println!("==> installing the runner's dependencies");
    install_runner()?;

    println!("==> checking wasm-bindgen");
    check_wasm_bindgen()?;

    println!("==> building the workspace for wasm");
    build_workspace()?;

    println!("==> building the example");
    let example = crate::run_wasm::build_example(false, &[])?;
    copy_example(&example)?;

    println!("==> building the wasm test binaries and binding them");
    let modules = build_wasm_tests()?;

    println!("==> assembling the test page ({} modules)", modules.len());
    write_page(&modules)?;

    println!("==> starting the browser runner");
    let mut runner = Runner::start(args.show)?;
    runner.wait_until_listening()?;

    println!("==> running the example in the browser");
    check_example()?;

    println!("==> running the tests in the browser");
    let result = run_through_nextest();
    runner.stop();

    match &result {
        Ok(()) => println!("==> all wasm tests passed in {:?}", started.elapsed()),
        Err(_) => eprintln!("==> wasm tests failed after {:?}", started.elapsed()),
    }
    result
}

/// Install the runner's JS dependencies, and a browser to drive.
///
/// Reuses a browser when `CHROME_PATH` names one, which is how a developer who
/// already has Chrome avoids a second copy.
fn install_runner() -> Result<(), String> {
    let mut npm = Command::new(npm());
    npm.args(["install", "--no-fund", "--no-audit"])
        .current_dir(RUNNER);
    step::run(&mut npm, "installing the wasm test runner's dependencies")?;

    if std::env::var_os("CHROME_PATH").is_some() {
        println!("using the browser named by CHROME_PATH; not installing one");
        return Ok(());
    }

    let mut npx = Command::new(npx());
    npx.args(["playwright", "install", "chromium"])
        .current_dir(RUNNER);
    step::run(&mut npx, "installing Chromium for Playwright")
}

/// Reject a `wasm-bindgen` whose version differs from the library's.
///
/// The CLI and the `wasm-bindgen` crate must match, and a mismatch produces a
/// version error from deep inside the generated JS that says nothing about the
/// cause, so it is caught here with the command that fixes it.
fn check_wasm_bindgen() -> Result<(), String> {
    let expected = resolved_wasm_bindgen()?;
    let output = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .map_err(|error| {
            format!(
                "running `wasm-bindgen --version`: {error}\n\
                 install it with `cargo install wasm-bindgen-cli --version {expected}`"
            )
        })?;
    let actual = String::from_utf8_lossy(&output.stdout);
    if !actual.contains(expected.as_str()) {
        return Err(format!(
            "the `wasm-bindgen` CLI does not match the `wasm-bindgen` the workspace \
             builds against.\n\
             the CLI is {}, but the workspace resolves {expected}.\n\
             fix it with `cargo install wasm-bindgen-cli --version {expected}`",
            actual.trim()
        ));
    }
    Ok(())
}

/// The `wasm-bindgen` version the workspace resolves, from `cargo metadata`.
///
/// Asked of cargo rather than read out of `Cargo.lock`, which is not tracked
/// here: a fresh checkout has none until something builds, and a developer
/// whose lockfile is stale would be told to install the wrong CLI.
fn resolved_wasm_bindgen() -> Result<String, String> {
    let mut metadata = Command::new("cargo");
    metadata.args(["metadata", "--format-version", "1", "--all-features"]);
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
    value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package.get("name").and_then(serde_json::Value::as_str) == Some("wasm-bindgen")
            })
        })
        .and_then(|package| package.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "`cargo metadata` lists no `wasm-bindgen` package".to_owned())
}

/// Build every wasm target in the workspace, not just the test binaries.
///
/// The test binaries above are built with `--all-features`, which the example
/// cannot take on this target: its `snapshot` feature reaches the harness's
/// readback and WebP scoring, which are native-only. So the workspace is built
/// here under the default features, which is what the browser example and the
/// Android library actually ship — and is what proves a change has not broken
/// them for wasm at all.
fn build_workspace() -> Result<(), String> {
    // Not `--all-targets`: the example crate is a `cdylib` as well as a binary,
    // and on `wasm32-unknown-unknown` the cdylib is not what a browser runs.
    // The binary is; the library exists for Android, so only the targets that
    // never produce it are built here.
    //
    // `xtask` is excluded because it is a host-only task runner: its HTTP server
    // reaches `async-io` and so `errno`, which does not build for this target.
    // Nothing in the browser or the APK depends on it.
    //
    // `unlit3d_mcp` is excluded for the same reason: it speaks the Model Context
    // Protocol over a native stdio pipe, and rmcp's server side pulls `uuid` v4,
    // whose wasm randomness source this crate does not ask for. A browser drives
    // the world through the example's own UI, not through the protocol.
    let mut build = Command::new("cargo");
    build
        .args(["build", "--workspace", "--exclude", "xtask"])
        .args(["--exclude", "unlit3d_mcp"])
        .args(["--target", TARGET])
        .args(["--bins", "--tests", "--benches", "--examples"]);
    step::run(&mut build, "building the workspace for wasm")
}

/// Build every wasm test binary, bindgen it, and return the module names.
fn build_wasm_tests() -> Result<Vec<String>, String> {
    // `cargo nextest list` is what knows which binaries the test targets are,
    // and it builds them as a side effect. Asking cargo directly would mean
    // repeating the target list and features, which is how the two drift.
    //
    // This is the slow step — every test binary for the target, from scratch on
    // a cold cache. Its progress goes to stderr, so stderr is inherited rather
    // than captured: capturing it would leave the terminal blank for minutes,
    // and the failure path below would report it only after the fact. stdout is
    // captured because that is where the JSON goes.
    let mut list = Command::new("cargo");
    list.args(["nextest", "list"])
        .args(PACKAGES.iter().flat_map(|package| ["-p", package]))
        .args(["--tests", "--all-features"])
        .args(["--list-type", "binaries-only", "--target", TARGET])
        .args(["--message-format=json", "-v"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let output = list
        .output()
        .map_err(|error| format!("listing the wasm test binaries: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "listing the wasm test binaries failed: {}",
            output.status
        ));
    }

    let binaries = parse_binaries(&String::from_utf8_lossy(&output.stdout))?;
    if binaries.is_empty() {
        return Err("no wasm test binaries were built".to_owned());
    }

    let dist = Path::new(DIST);
    std::fs::create_dir_all(dist).map_err(|error| format!("creating {DIST}: {error}"))?;

    let mut modules = Vec::new();
    for (module, path) in binaries {
        let mut bindgen = Command::new("wasm-bindgen");
        bindgen
            .arg(&path)
            .args(["--target", "web", "--no-typescript"])
            .arg("--out-dir")
            .arg(dist)
            .arg("--out-name")
            .arg(&module);
        step::run(&mut bindgen, &format!("wasm-bindgen for {module}"))?;
        modules.push(module);
    }
    modules.sort();
    Ok(modules)
}

/// The module name and wasm path of each test binary, from nextest's JSON.
///
/// The name is the target's, because that is what the harness's proxy sends and
/// what the page uses to pick a module: `env!("CARGO_CRATE_NAME")` in the test
/// target's own `gpu_test_main!`.
fn parse_binaries(json: &str) -> Result<Vec<(String, PathBuf)>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("parsing cargo nextest's output: {error}"))?;
    let binaries = value
        .get("rust-binaries")
        .and_then(serde_json::Value::as_object)
        .ok_or("cargo nextest's output has no `rust-binaries`")?;

    let mut found = Vec::new();
    for binary in binaries.values() {
        // Only test targets export `run_test`; a library has no page to call.
        let is_test = binary.get("kind").and_then(serde_json::Value::as_str) == Some("test");
        let Some(name) = binary
            .get("binary-name")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(path) = binary
            .get("binary-path")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if is_test && path.ends_with(".wasm") {
            found.push((name.to_owned(), PathBuf::from(path)));
        }
    }
    if found.is_empty() {
        return Err("cargo nextest listed no wasm test binaries".to_owned());
    }
    Ok(found)
}

/// Copy the page into the served directory and write the module map.
fn write_page(modules: &[String]) -> Result<(), String> {
    let dist = Path::new(DIST);
    let web = Path::new(WEB);
    for entry in std::fs::read_dir(web).map_err(|error| format!("reading {WEB}: {error}"))? {
        let entry = entry.map_err(|error| format!("reading {WEB}: {error}"))?;
        let target = dist.join(entry.file_name());
        std::fs::copy(entry.path(), &target)
            .map_err(|error| format!("copying to {}: {error}", target.display()))?;
    }

    // The page fetches this to turn the module name from the query string into
    // a script path, so it never builds a URL out of untrusted input.
    let paths: serde_json::Map<String, serde_json::Value> = modules
        .iter()
        .map(|module| {
            (
                module.clone(),
                serde_json::Value::String(format!("./{module}.js")),
            )
        })
        .collect();
    let path = dist.join("wasm_paths.json");
    let json = serde_json::to_string_pretty(&paths)
        .map_err(|error| format!("serializing wasm_paths.json: {error}"))?;
    std::fs::write(&path, json).map_err(|error| format!("writing {}: {error}", path.display()))
}

/// Copy the built example into [`EXAMPLE_DIST`], where the runner serves it.
fn copy_example(built: &Path) -> Result<(), String> {
    let target = Path::new(EXAMPLE_DIST);
    // Removed first: a stale module would keep being served after the build
    // that produced it stopped being the one on disk.
    std::fs::remove_dir_all(target).ok();
    std::fs::create_dir_all(target).map_err(|error| format!("creating {EXAMPLE_DIST}: {error}"))?;

    for entry in
        std::fs::read_dir(built).map_err(|error| format!("reading {}: {error}", built.display()))?
    {
        let entry = entry.map_err(|error| format!("reading {}: {error}", built.display()))?;
        let destination = target.join(entry.file_name());
        std::fs::copy(entry.path(), &destination)
            .map_err(|error| format!("copying to {}: {error}", destination.display()))?;
    }
    Ok(())
}

/// Ask the runner to load the example and confirm it drew something.
///
/// The GPU tests cover the rendering paths; this covers the program a browser
/// actually runs, which they never start. A change that compiles but leaves the
/// page blank passes those and fails here.
fn check_example() -> Result<(), String> {
    let response = ureq::get(&format!("{BASE_URL}/run_example"))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|error| format!("running the example in the browser: {error}"))?;

    if response.status().is_success() {
        return Ok(());
    }
    let message = response
        .into_body()
        .read_to_string()
        .unwrap_or_else(|error| format!("the runner's failure message could not be read: {error}"));
    Err(format!("the example did not render:\n{message}"))
}

/// Run the tests through the proxy, so nextest reports the browser's results.
fn run_through_nextest() -> Result<(), String> {
    let mut nextest = Command::new("cargo");
    nextest
        .args(["nextest", "run", "--profile", "wasm"])
        .args(PACKAGES.iter().flat_map(|package| ["-p", package]))
        .args(["--tests", "--all-features"])
        .env(WASM_TEST_ENV, "1");
    step::run(&mut nextest, "the wasm tests in the browser")
}

/// The Playwright runner, stopped when the run is over.
struct Runner(Child);

impl Runner {
    /// Start the runner, with a visible window when `--show` was asked for.
    fn start(show: bool) -> Result<Self, String> {
        let mut child = Command::new("node")
            .arg(Path::new(RUNNER).join("index.js"))
            .args(if show { &["--show"][..] } else { &[][..] })
            .env(MISMATCH_DIR_ENV, MISMATCH_DIR)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| {
                format!("starting the runner: {error}\nrun the dependencies first, or check that `node` is on PATH")
            })?;

        // A runner that dies at startup — a missing dependency, a port already
        // taken — would otherwise look like a timeout.
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("checking the runner: {error}"))?
        {
            return Err(format!("the runner exited immediately with {status}"));
        }
        Ok(Self(child))
    }

    /// Wait for the runner to accept a connection.
    fn wait_until_listening(&mut self) -> Result<(), String> {
        // Hold the port's own lock while probing it. A connect that succeeds
        // only means *something* is listening, and a stale runner from an
        // earlier run would answer the requests with a stale page.
        let start = Instant::now();
        loop {
            if let Some(status) = self
                .0
                .try_wait()
                .map_err(|error| format!("checking the runner: {error}"))?
            {
                return Err(format!("the runner exited with {status} before listening"));
            }
            if TcpStream::connect(("127.0.0.1", PORT)).is_ok() {
                return Ok(());
            }
            if start.elapsed() > STARTUP_TIMEOUT {
                return Err(format!(
                    "the runner did not listen on port {PORT} within {STARTUP_TIMEOUT:?}"
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stop the runner, and its browser with it.
    ///
    /// Through the runner's own shutdown route rather than a signal, so the
    /// browser is closed first: killing the process outright would leave a
    /// browser behind on every run.
    fn stop(&mut self) {
        let _ = ureq::get(&format!("{BASE_URL}/shutdown")).call();
        // The runner exits once its browser has closed; if it does not, the
        // kill below is the fallback.
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // A run that failed before `stop` must not leave the browser behind.
        if self.0.try_wait().ok().flatten().is_none() {
            self.stop();
        }
    }
}

/// The `npm` to run, which is `npm.cmd` on Windows.
fn npm() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

/// The `npx` to run, which is `npx.cmd` on Windows.
fn npx() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}
