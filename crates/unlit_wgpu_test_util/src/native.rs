//! The native half of the harness: `libtest-mimic` over the registry.
//!
//! Natively a test runs in the process that reports it, which is what
//! `cargo nextest` drives. The runner is `libtest-mimic` rather than libtest's
//! generated harness because the registry, not a set of `#[test]` items, is
//! what a browser can also offer — so one source file feeds both runners and a
//! test keeps a single name in both.
//!
//! `libtest-mimic` has no `#[should_panic]`, so the comparison lives here and
//! matches what the browser's panic hook does: a caught panic passes when its
//! message contains the message the test declared, and fails otherwise.

use core::any::Any;
use core::panic::AssertUnwindSafe;

use crate::TestEntry;

/// Run every registered test, reporting through `libtest-mimic`.
///
/// Never returns: it exits the process with libtest's status.
pub fn run(tests: Vec<TestEntry>) -> ! {
    let args = libtest_mimic::Arguments::from_args();
    // A trial's name is the test's plain name, which is what libtest reports
    // for an integration test's `#[test] fn` — so a filter or a recorded name
    // does not have to learn a new spelling.
    let trials = tests
        .into_iter()
        .map(|entry| libtest_mimic::Trial::test(entry.name, move || run_one(entry)))
        .collect();

    libtest_mimic::run(&args, trials).exit()
}

/// Drive one test's future to completion, deciding it against the panic the
/// test declared.
fn run_one(entry: TestEntry) -> Result<(), libtest_mimic::Failed> {
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        pollster::block_on((entry.body)());
    }));

    match (outcome, entry.expected_panic) {
        (Ok(()), None) => Ok(()),
        (Ok(()), Some(expected)) => Err(libtest_mimic::Failed::from(format!(
            "expected a panic whose message contains {expected:?}, \
             but the test returned without panicking"
        ))),
        // The default hook has already printed the panic and its backtrace, so
        // the message alone is enough to fail the test with.
        (Err(payload), None) => Err(libtest_mimic::Failed::from(format!(
            "the test panicked: {}",
            panic_message(&*payload)
        ))),
        (Err(payload), Some(expected)) => {
            let message = panic_message(&*payload);
            if message.contains(expected) {
                Ok(())
            } else {
                Err(libtest_mimic::Failed::from(format!(
                    "expected a panic whose message contains {expected:?}, \
                     but the message was:\n{message}"
                )))
            }
        }
    }
}

/// The text of a panic payload.
///
/// `panic!` with a literal produces a `&str` and with formatting a `String`;
/// anything else is a caller's own payload, which has no message to compare.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "a panic payload that is neither `&str` nor `String`".to_owned()
    }
}
