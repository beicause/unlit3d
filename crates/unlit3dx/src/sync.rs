//! Poison-proof lock helpers.
//!
//! A lock's contents are only ever plain data here - a flag, a status - so a
//! panic while it was held leaves nothing inconsistent behind. Reading
//! through these helpers instead of `expect`ing keeps one panic from turning
//! a lock into a permanent failure for every later caller.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `mutex`, ignoring poisoning.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
