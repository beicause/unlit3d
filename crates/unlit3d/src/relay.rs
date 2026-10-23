//! The two shapes a value takes when it leaves a world: a slot that keeps
//! the newest one, and a queue that keeps every one.
//!
//! A world is owned by the thread that ticks it, so nothing inside it can be
//! read from another thread. What crosses is a handle: a producer keeps
//! publishing, and every reader sees the value on its own thread without a
//! job or a reply.
//!
//! - A [`Snapshot`](crate::relay::Snapshot) holds one value. Publishing
//!   replaces it, so a reader that misses a publish never learns what was
//!   there, and that is exactly right for state: poses, a playback position,
//!   the size a UI is laid out at. A reader is not a queue consumer, so a
//!   slow one cannot fall behind, and one that subscribes late still sees the
//!   newest publish.
//! - A [`Stream`](crate::relay::Stream) holds every value, once per reader.
//!   Publishing appends to each reader's queue, and a reader folds the queue
//!   into its tick with [`StreamReader::pump`](crate::relay::StreamReader::pump).
//!   That is right for events: input, collisions, a sequence ending. A reader
//!   that ticks slower than the producer still sees every value, in order.
//!
//! Neither side blocks: a publish swaps a pointer or publishes to an unbounded
//! queue, and a reader takes what is there. Dropping a reader unsubscribes
//! it: a stream prunes it when nobody is left, and a snapshot keeps its value
//! for whoever subscribes next.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use arc_swap::ArcSwapOption;
use async_channel::{Receiver, Sender, unbounded};

/// The producing end of a [`Snapshot`]: keeps the newest value for however
/// many readers are subscribed.
///
/// Cloning shares the slot. It keeps the newest value whether or not anyone
/// is subscribed, so a reader that arrives after a publish still finds it
/// instead of waiting for the next one.
pub struct Snapshot<T> {
    slot: Arc<SnapshotSlot<T>>,
}

struct SnapshotSlot<T> {
    value: ArcSwapOption<T>,
    readers: AtomicUsize,
}

impl<T> Snapshot<T> {
    /// A slot nobody is subscribed to yet.
    pub fn new() -> Self {
        Self {
            slot: Arc::new(SnapshotSlot {
                value: ArcSwapOption::new(None),
                readers: AtomicUsize::new(0),
            }),
        }
    }

    /// Replace the value, and answer how many readers are subscribed.
    ///
    /// The value is kept even with none subscribed, so one that subscribes
    /// later still finds the newest publish. A collector uses the answer to
    /// prune its own subscription: at zero, nobody is left to read it.
    pub fn publish(&self, value: T) -> usize {
        self.slot.value.store(Some(Arc::new(value)));
        self.slot.readers.load(Ordering::Relaxed)
    }

    /// A reader of this slot. The snapshot stays a slot as long as any
    /// reader or the producing handle is alive.
    pub fn reader(&self) -> SnapshotReader<T> {
        self.slot.readers.fetch_add(1, Ordering::Relaxed);
        SnapshotReader {
            slot: Arc::clone(&self.slot),
        }
    }

    /// Whether a reader is subscribed.
    pub fn has_readers(&self) -> bool {
        self.slot.readers.load(Ordering::Relaxed) > 0
    }
}

impl<T> Clone for Snapshot<T> {
    fn clone(&self) -> Self {
        Self {
            slot: Arc::clone(&self.slot),
        }
    }
}

impl<T> Default for Snapshot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for Snapshot<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("readers", &self.slot.readers.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The reading end of a [`Snapshot`]: always the newest published value.
pub struct SnapshotReader<T> {
    slot: Arc<SnapshotSlot<T>>,
}

impl<T> SnapshotReader<T> {
    /// The newest value, or `None` before the first publish.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<T>> {
        self.slot.value.load_full()
    }
}

impl<T> Clone for SnapshotReader<T> {
    fn clone(&self) -> Self {
        self.slot.readers.fetch_add(1, Ordering::Relaxed);
        Self {
            slot: Arc::clone(&self.slot),
        }
    }
}

impl<T> Drop for SnapshotReader<T> {
    fn drop(&mut self) {
        self.slot.readers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl<T> fmt::Debug for SnapshotReader<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotReader")
            .finish_non_exhaustive()
    }
}

/// The producing end of a [`Stream`]: pushes every value to every reader.
///
/// Cloning shares the readers. A publish to a stream nobody reads is
/// dropped, and the dead senders are pruned then.
pub struct Stream<T> {
    inner: Arc<StreamInner<T>>,
}

struct StreamInner<T> {
    senders: Mutex<Vec<Sender<T>>>,
}

impl<T: Clone> Stream<T> {
    /// A stream nobody is subscribed to yet.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StreamInner {
                senders: Mutex::new(Vec::new()),
            }),
        }
    }

    /// A reader with its own queue, so one reader's pace cannot starve
    /// another's.
    pub fn subscribe(&self) -> StreamReader<T> {
        let (sender, receiver) = unbounded();
        self.inner
            .senders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        StreamReader {
            inner: Arc::new(ReaderInner {
                queue: receiver,
                batch: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Publish one value to every reader, and answer how many are left.
    ///
    /// A reader that dropped its queue is pruned here, so the producing
    /// world learns that nobody is left the next time it publishes.
    pub fn publish(&self, value: T) -> usize {
        self.publish_all([value])
    }

    /// Publish several values, in the order given.
    pub fn publish_all(&self, values: impl IntoIterator<Item = T>) -> usize {
        let mut senders = self
            .inner
            .senders
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        senders.retain(|sender| !sender.is_closed());
        for value in values {
            for sender in senders.iter() {
                // The queue is unbounded, so the only way this fails is a
                // reader that went away since the prune above.
                let _ = sender.try_send(value.clone());
            }
        }
        // A reader may have gone away while the values were being sent, so
        // the count is taken after the sends rather than before them.
        senders.retain(|sender| !sender.is_closed());
        senders.len()
    }

    /// Whether a reader is subscribed, pruning the ones that went away.
    pub fn has_readers(&self) -> bool {
        let mut senders = self
            .inner
            .senders
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        senders.retain(|sender| !sender.is_closed());
        !senders.is_empty()
    }
}

impl<T: Clone> Clone for Stream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: Clone> Default for Stream<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for Stream<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Stream").finish_non_exhaustive()
    }
}

/// The reading end of a [`Stream`]: one queue per reader, folded into the
/// reader's tick.
///
/// A reader is cheap to clone within the world that owns it; every clone
/// shares the same queue and batch, so the world's components read one
/// tick's events between them.
pub struct StreamReader<T> {
    inner: Arc<ReaderInner<T>>,
}

struct ReaderInner<T> {
    queue: Receiver<T>,
    batch: Mutex<Vec<T>>,
}

impl<T> StreamReader<T> {
    /// Start the reader's tick: whatever queued since the last pump becomes
    /// the batch, and the previous batch is dropped.
    ///
    /// Call it once per tick, before anything reads the batch. Calling it
    /// twice in one tick drops what the first call collected.
    pub fn pump(&self) {
        let mut batch = self
            .inner
            .batch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        batch.clear();
        while let Ok(value) = self.inner.queue.try_recv() {
            batch.push(value);
        }
    }

    /// Run `f` over this tick's values, oldest first.
    ///
    /// Reading does not consume: every reader in the world sees the same
    /// batch, so no one can take a value away from the next.
    pub fn with_batch<R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        let batch = self
            .inner
            .batch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        f(&batch)
    }

    /// Append this tick's values, oldest first, to `out`.
    pub fn copy_into(&self, out: &mut Vec<T>)
    where
        T: Clone,
    {
        self.with_batch(|batch| out.extend_from_slice(batch));
    }
}

impl<T> Clone for StreamReader<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> fmt::Debug for StreamReader<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamReader")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_keeps_the_newest_value() {
        let snapshot = Snapshot::new();
        let reader = snapshot.reader();
        assert!(reader.latest().is_none());
        assert_eq!(snapshot.publish(1), 1);
        assert_eq!(snapshot.publish(2), 1);
        assert_eq!(*reader.latest().unwrap(), 2);
    }

    #[test]
    fn a_late_reader_finds_the_value_published_before_it() {
        let snapshot = Snapshot::new();
        assert_eq!(snapshot.publish(1), 0);
        let reader = snapshot.reader();
        assert_eq!(*reader.latest().expect("the publish was kept"), 1);
        assert_eq!(snapshot.publish(2), 1);
        assert_eq!(*reader.latest().expect("the newest publish"), 2);
    }

    #[test]
    fn dropping_the_last_reader_unsubscribes() {
        let snapshot = Snapshot::new();
        let reader = snapshot.reader();
        assert_eq!(snapshot.publish(1), 1);
        drop(reader);
        assert_eq!(snapshot.publish(2), 0);
        assert_eq!(*snapshot.reader().latest().expect("the last publish"), 2);
    }

    #[test]
    fn a_stream_gives_every_reader_the_same_values() {
        let stream = Stream::new();
        let one = stream.subscribe();
        let other = stream.subscribe();
        assert_eq!(stream.publish(1), 2);
        assert_eq!(stream.publish(2), 2);
        one.pump();
        other.pump();
        assert_eq!(one.with_batch(|batch| batch.to_vec()), vec![1, 2]);
        assert_eq!(other.with_batch(|batch| batch.to_vec()), vec![1, 2]);
    }

    #[test]
    fn a_pump_replaces_the_previous_batch() {
        let stream = Stream::new();
        let reader = stream.subscribe();
        stream.publish(1);
        reader.pump();
        stream.publish(2);
        reader.pump();
        assert_eq!(reader.with_batch(|batch| batch.to_vec()), vec![2]);
    }

    #[test]
    fn a_stream_prunes_a_reader_that_went_away() {
        let stream = Stream::new();
        let reader = stream.subscribe();
        assert!(stream.has_readers());
        drop(reader);
        assert!(!stream.has_readers());
        assert_eq!(stream.publish(1), 0);
    }

    #[test]
    fn a_reader_clone_shares_the_queue() {
        let stream = Stream::new();
        let reader = stream.subscribe();
        let clone = reader.clone();
        stream.publish(1);
        reader.pump();
        assert_eq!(clone.with_batch(|batch| batch.to_vec()), vec![1]);
        drop(reader);
        assert!(stream.has_readers());
    }
}
