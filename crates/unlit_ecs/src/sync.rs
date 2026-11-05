//! Poison-proof lock helpers, and the rules the locks are taken by.
//!
//! A lock's contents are only ever plain data here - a flag, a status, a
//! message - so a panic while it was held leaves nothing inconsistent behind.
//! Reading through these helpers instead of `expect`ing keeps one panic from
//! turning a lock into a permanent failure for every later caller. That
//! premise holds only while a lock covers data and nothing else, which is
//! what the rules below are for:
//!
//! - A guard covers a critical section and no user code. A callback, a query
//!   into a world, anything that may panic, runs before the lock is taken or
//!   after it is dropped. Reading a batch out under its lock and running the
//!   caller's closure without it is the shape to copy.
//! - `Debug` and `Display` never wait on a lock. A value is printed from a
//!   panic handler, a log line or a signal handler, sometimes while another
//!   lock is held, so these implementations print the shape of the value
//!   instead of reading it, and a derived one prints what the standard
//!   library's own `Mutex` and `RwLock` impls offer - which is a try, not a
//!   wait.
//! - No lock is taken while another is held, so there is no order to keep
//!   and no way for two of them to wait on each other. A place that needs
//!   two takes the first, copies what it needs out of it, and drops it before
//!   taking the second.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Lock `mutex`, ignoring poisoning.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Take `lock` for reading, ignoring poisoning.
pub fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// Take `lock` for writing, ignoring poisoning.
pub fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
