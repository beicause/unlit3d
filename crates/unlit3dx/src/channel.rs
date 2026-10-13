//! Single-slot snapshot mailboxes: the newest snapshot wins.
//!
//! A snapshot is how a world reports its state to the others without ever
//! calling back: the producer replaces the slot's contents, the consumers
//! read whatever is newest, and no older snapshot is ever queued behind a
//! slow reader. A reader that skips a snapshot misses nothing but history.

use std::sync::{Arc, RwLock};

use crate::sync;

/// The writing half of a snapshot mailbox.
#[derive(Clone, Debug)]
pub struct SnapshotWriter<T> {
    slot: Arc<RwLock<Option<Arc<T>>>>,
}

/// The reading half of a snapshot mailbox.
#[derive(Clone, Debug)]
pub struct SnapshotReader<T> {
    slot: Arc<RwLock<Option<Arc<T>>>>,
}

/// A writer and a reader sharing one slot.
#[must_use]
pub fn snapshot_pair<T>() -> (SnapshotWriter<T>, SnapshotReader<T>) {
    let slot = Arc::new(RwLock::new(None));
    (
        SnapshotWriter {
            slot: Arc::clone(&slot),
        },
        SnapshotReader { slot },
    )
}

impl<T> SnapshotWriter<T> {
    /// Replace the slot's contents, returning the snapshot it displaced.
    pub fn publish(&self, snapshot: T) -> Option<Arc<T>> {
        sync::write(&self.slot).replace(Arc::new(snapshot))
    }
}

impl<T> SnapshotReader<T> {
    /// The newest snapshot, or [`None`] if none was ever published.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<T>> {
        sync::read(&self.slot).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Publishing twice leaves the newest snapshot in the slot.
    #[test]
    fn the_newest_snapshot_wins() {
        let (writer, reader) = snapshot_pair();
        writer.publish(1);
        writer.publish(2);
        assert_eq!(*reader.latest().expect("a snapshot is published"), 2);
    }

    /// Publishing returns the snapshot it replaced.
    #[test]
    fn publish_returns_the_replaced_snapshot() {
        let (writer, _reader) = snapshot_pair();
        assert!(writer.publish(1).is_none());
        assert_eq!(*writer.publish(2).expect("the first snapshot"), 1);
    }

    /// A panic while a snapshot is held does not poison the mailbox.
    #[test]
    fn a_panic_while_publishing_does_not_poison_the_mailbox() {
        let (writer, reader) = snapshot_pair();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = sync::write(&writer.slot);
            panic!("while holding the write lock");
        }));
        writer.publish(1);
        assert_eq!(*reader.latest().expect("the mailbox still works"), 1);
    }
}
