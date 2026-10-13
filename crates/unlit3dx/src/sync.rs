//! Poison-proof lock helpers.
//!
//! A lock's contents are only ever plain data here - a flag, a slot - so a
//! panic while it was held leaves nothing inconsistent behind. Reading
//! through these helpers instead of `expect`ing keeps one panic from turning
//! a lock into a permanent failure for every later caller.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Lock `mutex`, ignoring poisoning.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Take a shared lock, ignoring poisoning.
pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// Take an exclusive lock, ignoring poisoning.
pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
