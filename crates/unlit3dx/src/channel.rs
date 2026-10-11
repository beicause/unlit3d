//! Single-slot snapshot mailboxes: the newest snapshot wins.
//!
//! A snapshot is how a world reports its state to the others without ever
//! calling back: the producer replaces the slot's contents, the consumers
//! read whatever is newest, and no older snapshot is ever queued behind a
//! slow reader. A reader that skips a snapshot misses nothing but history.

use std::sync::{Arc, RwLock};

/// Publishes snapshots: each [`publish`](Self::publish) replaces the
/// previous one.
#[derive(Clone, Debug)]
pub struct SnapshotWriter<T> {
    /// The single slot the newest snapshot occupies.
    slot: Arc<RwLock<Option<Arc<T>>>>,
}

/// Reads the newest published snapshot, or nothing before the first one.
#[derive(Clone, Debug)]
pub struct SnapshotReader<T> {
    /// The single slot the newest snapshot occupies, shared with the writer.
    slot: Arc<RwLock<Option<Arc<T>>>>,
}

/// Pair a writer with the reader that sees what it publishes.
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
    /// Publish a snapshot, returning the one it replaces.
    pub fn publish(&self, snapshot: T) -> Option<Arc<T>> {
        self.slot
            .write()
            .expect("the snapshot slot is not poisoned")
            .replace(Arc::new(snapshot))
    }
}

impl<T> SnapshotReader<T> {
    /// The newest snapshot, shared with the writer's thread.
    pub fn latest(&self) -> Option<Arc<T>> {
        self.slot
            .read()
            .expect("the snapshot slot is not poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader always sees the newest snapshot and never an older one.
    #[test]
    fn the_newest_snapshot_wins() {
        let (writer, reader) = snapshot_pair::<u32>();
        assert!(reader.latest().is_none());
        writer.publish(1);
        assert_eq!(reader.latest().map(|snapshot| *snapshot), Some(1));
        writer.publish(2);
        assert_eq!(reader.latest().map(|snapshot| *snapshot), Some(2));
    }

    /// The snapshot a publish replaces is the one that was there before.
    #[test]
    fn publish_returns_the_replaced_snapshot() {
        let (writer, _reader) = snapshot_pair::<u32>();
        writer.publish(1);
        assert_eq!(writer.publish(2).map(|snapshot| *snapshot), Some(1));
        assert_eq!(writer.publish(3).map(|snapshot| *snapshot), Some(2));
    }
}
