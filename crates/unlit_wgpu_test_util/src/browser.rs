//! The browser half of the harness.
//!
//! A page asks for one test by name; this module finds it in the registry and
//! drives it. The verdict travels back through `sessionStorage`, because a
//! wasm export cannot return a value to a page that has already moved on.
//!
//! A panic here is not a failure the runner can catch: wasm has no unwinding,
//! so a panic traps the instance and the page dies with it. The panic hook
//! therefore runs *before* the trap and is the only place the verdict can be
//! decided — which is why a test's expected message is read from the registry
//! rather than from a `#[should_panic]` attribute no one would see.

use wasm_bindgen::prelude::*;

use crate::TestEntry;

#[wasm_bindgen(inline_js = "
  export function test_success() {
    window.sessionStorage.test_success = 'true';
  }

  export function test_failure(message) {
    window.sessionStorage.test_failure = message;
    console.error(message);
  }
")]
extern "C" {
    fn test_success();
    fn test_failure(message: String);
}

/// Run the registered test called `name`, publishing its result for the page.
///
/// The host crate exports this to JavaScript as `run_test`.
pub fn run(tests: Vec<TestEntry>, name: String) {
    let Some(entry) = tests.into_iter().find(|entry| entry.name == name) else {
        test_failure(format!("no test named `{name}` is registered"));
        return;
    };

    install_panic_hook(entry.expected_panic);

    // A test's device comes from a promise, so the body cannot be driven to
    // completion here: the page's own task has to end first, and the result is
    // published when the future finishes.
    wasm_bindgen_futures::spawn_local(async move {
        (entry.body)().await;
        test_success();
    });
}

/// Turn a panic into this test's verdict.
///
/// A message the test declared is a pass; anything else is a failure, whether
/// the test declared nothing or declared something else.
fn install_panic_hook(expected: Option<&'static str>) {
    std::panic::set_hook(Box::new(move |info| {
        let message = info.to_string();
        match expected {
            Some(expected) if message.contains(expected) => test_success(),
            _ => test_failure(message),
        }
    }));
}
