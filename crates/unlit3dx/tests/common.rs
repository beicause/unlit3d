//! Helpers the `unlit3dx` suite's modules share.
//!
//! A subscription crosses from the world that collects it to the reader on the
//! host, so every module waits on one the same way: a deadline in whole
//! milliseconds, and the first snapshot the collector published.

use std::time::Duration;

use unlit3dx::prelude::*;

/// Waits until `probe` answers `true`, giving up after `millis` milliseconds.
///
/// Answering `false` says the wait ran out; a caller decides whether that is a
/// failure or the point of the test.
pub fn wait_for(mut probe: impl FnMut() -> bool, millis: u64) -> bool {
    for _ in 0..millis {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// The reader of a subscription the world has installed.
///
/// The two failures are named apart: a subscription the world has not taken up
/// yet, and an outlet the collector can no longer reach.
pub fn installed<R: Clone>(subscription: Subscription<R>) -> R {
    subscription
        .poll()
        .expect("the world installed the subscription")
        .expect("the subscription outlet is reachable")
}
