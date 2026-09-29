//! The native proxy that runs each test in a browser instead of in-process.
//!
//! When [`WASM_TEST_ENV`] is set, the test binary stops being a runner and
//! becomes a *client*: `cargo nextest` still lists and drives it, but each trial
//! makes an HTTP request to the local runner, which opens the page and runs that
//! one test in a real browser. The verdict comes back as the response's status,
//! so nextest reports a browser failure exactly as it reports an in-process one
//! — same listing, same filters, same exit code.
//!
//! The mode is chosen from the environment rather than by a `cfg`.
//!
//! This is why the trial names carry a `[wasm]` marker: the `wasm` nextest
//! profile selects them with `default-filter = "test([wasm])"`, so a single
//! `cargo nextest run` drives only the browser side and never the in-process
//! one.

use crate::TestEntry;

/// The environment variable that puts a test binary in proxy mode.
///
/// Any value selects it. The module the page loads is a compile-time constant
/// of each test binary rather than part of this variable, since one value shared
/// by a whole `cargo nextest run` could not say which binary is asking.
pub const WASM_TEST_ENV: &str = "UNLIT3D_WASM_TEST";

/// The port the browser runner listens on.
///
/// Fixed so the two halves agree without configuration, and because the runner
/// is started and stopped within one `cargo xtask test-wasm`.
const PORT: u16 = 3000;

/// The marker every trial name carries, and what the `wasm` nextest profile
/// filters on.
const WASM_MARKER: &str = "[wasm]";

/// Run every registered test in the browser, reporting through `libtest-mimic`.
///
/// `wasm_module` names the wasm binary the page should load.
///
/// Never returns: it exits the process with libtest's status.
pub fn run_wasm(tests: Vec<TestEntry>, wasm_module: &'static str) -> ! {
    let args = libtest_mimic::Arguments::from_args();
    let trials = tests
        .into_iter()
        .map(|entry| {
            // The marker leads, so the profile's filter matches on it without
            // having to know any test's name.
            let trial_name = format!("{WASM_MARKER} {}", entry.name);
            libtest_mimic::Trial::test(trial_name, move || run_in_browser(wasm_module, entry.name))
        })
        .collect();

    libtest_mimic::run(&args, trials).exit()
}

/// Ask the runner to run one test in a browser, and report what it said.
fn run_in_browser(wasm_module: &str, name: &str) -> Result<(), libtest_mimic::Failed> {
    // A failing test answers with a 5xx and its message in the body, so the
    // status is the verdict and the body is the explanation. Left as a response
    // rather than turned into an error, which would discard the body.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    let mut response = agent
        .get(&format!("http://127.0.0.1:{PORT}/run_test"))
        .query("wasm", wasm_module)
        .query("name", name)
        .call()?;

    if response.status().is_success() {
        return Ok(());
    }
    let message = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| format!("the runner's failure message could not be read: {error}"));
    Err(libtest_mimic::Failed::from(message))
}
