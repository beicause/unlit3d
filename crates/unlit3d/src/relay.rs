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
//!   the size an overlay is laid out at. A reader is not a queue consumer, so a
//!   slow one cannot fall behind, and one that subscribes late still sees the
//!   newest publish.
//! - A [`Stream`](crate::relay::Stream) queues events separately for each subscriber. A lagging
//!   subscriber drains the accepted events in order and then observes overflow;
//!   neither the producer nor other subscribers wait for it.

use std::convert::Infallible;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use arc_swap::ArcSwapOption;
use async_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use unlit_ecs::sync;

/// The latest-value slot shared by a producer and its readers.
///
/// Cloning a producer shares the slot. It closes when the last producer is
/// dropped or when any producer explicitly ends it. The last value is retained.
pub struct Snapshot<T, E = Infallible> {
    producer: Arc<SnapshotProducer<T, E>>,
}

struct SnapshotProducer<T, E>(Arc<SnapshotSlot<T, E>>);

struct SnapshotSlot<T, E> {
    value: ArcSwapOption<T>,
    readers: AtomicUsize,
    end: Mutex<Option<SnapshotEnd<E>>>,
}

enum SnapshotEnd<E> {
    Closed,
    Failed(E),
}

impl<T> Snapshot<T> {
    /// A slot nobody is subscribed to yet, with no failure payload.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T, E> Snapshot<T, E> {
    /// Replace the value and return the number of readers, or zero if ended.
    pub fn publish(&self, value: T) -> usize {
        let slot = &self.producer.0;
        let end = sync::lock(&slot.end);
        if end.is_some() {
            return 0;
        }
        let replaced = slot.value.swap(Some(Arc::new(value)));
        drop(end);
        drop(replaced);
        slot.readers.load(Ordering::Relaxed)
    }

    /// Subscribe with an independent cursor. The first poll sees any value
    /// already published, unless the slot has ended.
    pub fn reader(&self) -> SnapshotReader<T, E> {
        let slot = &self.producer.0;
        slot.readers.fetch_add(1, Ordering::Relaxed);
        SnapshotReader {
            slot: Arc::clone(slot),
            seen: Weak::new(),
        }
    }

    /// Whether a reader is subscribed to this slot.
    pub fn has_readers(&self) -> bool {
        self.producer.0.readers.load(Ordering::Relaxed) > 0
    }

    /// Close the slot. The first terminal state wins.
    pub fn close(&self) {
        sync::lock(&self.producer.0.end).get_or_insert(SnapshotEnd::Closed);
    }

    /// End the slot with a failure. The first terminal state wins.
    pub fn fail(&self, error: E) {
        sync::lock(&self.producer.0.end).get_or_insert(SnapshotEnd::Failed(error));
    }
}

impl<T, E> Drop for SnapshotProducer<T, E> {
    fn drop(&mut self) {
        sync::lock(&self.0.end).get_or_insert(SnapshotEnd::Closed);
    }
}

impl<T, E> Clone for Snapshot<T, E> {
    fn clone(&self) -> Self {
        Self {
            producer: Arc::clone(&self.producer),
        }
    }
}

impl<T, E> Default for Snapshot<T, E> {
    fn default() -> Self {
        Self {
            producer: Arc::new(SnapshotProducer(Arc::new(SnapshotSlot {
                value: ArcSwapOption::empty(),
                readers: AtomicUsize::new(0),
                end: Mutex::new(None),
            }))),
        }
    }
}

impl<T, E> fmt::Debug for Snapshot<T, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Snapshot").finish_non_exhaustive()
    }
}

/// The state of a snapshot relative to one reader's previous poll.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotPoll<T, E = Infallible> {
    /// Nothing has been published yet.
    Uninitialized,
    /// No publish since the last poll.
    Unchanged,
    /// The newest publish, including publishes that repeat the same value.
    Updated(Arc<T>),
    /// The producer closed without failing.
    Closed,
    /// The producer ended with this failure.
    Failed(E),
}

/// A reader of the newest published value. Cloning copies its poll cursor;
/// subsequent polls by each clone are independent.
pub struct SnapshotReader<T, E = Infallible> {
    slot: Arc<SnapshotSlot<T, E>>,
    seen: Weak<T>,
}

impl<T, E> SnapshotReader<T, E> {
    /// The newest value, even after termination. Does not advance the cursor.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<T>> {
        self.slot.value.load_full()
    }

    /// Check for a newer publish. A terminal state takes precedence; use
    /// [`latest`](Self::latest) to read the last value after termination.
    #[must_use]
    pub fn poll(&mut self) -> SnapshotPoll<T, E>
    where
        E: Clone,
    {
        let end = sync::lock(&self.slot.end);
        match &*end {
            Some(SnapshotEnd::Closed) => return SnapshotPoll::Closed,
            Some(SnapshotEnd::Failed(error)) => return SnapshotPoll::Failed(error.clone()),
            None => {}
        }
        let Some(value) = self.latest() else {
            return SnapshotPoll::Uninitialized;
        };
        if std::ptr::eq(self.seen.as_ptr(), Arc::as_ptr(&value)) {
            return SnapshotPoll::Unchanged;
        }
        self.seen = Arc::downgrade(&value);
        SnapshotPoll::Updated(value)
    }
}

impl<T, E> Clone for SnapshotReader<T, E> {
    fn clone(&self) -> Self {
        self.slot.readers.fetch_add(1, Ordering::Relaxed);
        Self {
            slot: Arc::clone(&self.slot),
            seen: self.seen.clone(),
        }
    }
}

impl<T, E> Drop for SnapshotReader<T, E> {
    fn drop(&mut self) {
        self.slot.readers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl<T, E> fmt::Debug for SnapshotReader<T, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotReader")
            .finish_non_exhaustive()
    }
}

/// Why a stream reader stopped accepting new values.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamEnd<E = Infallible> {
    /// The producer closed without failing.
    Closed,
    /// The producer ended with this failure.
    Failed(E),
    /// The reader's queue filled. Its queued values are still readable.
    Overflow,
}

/// A producer that publishes to a separate queue for each subscriber.
/// Cloning shares the producer, including its terminal state.
pub struct Stream<T, E = Infallible> {
    inner: Arc<Mutex<StreamInner<T, E>>>,
}

struct StreamInner<T, E> {
    senders: Vec<StreamSender<T, E>>,
    terminal: Option<StreamEnd<E>>,
}

struct StreamSender<T, E> {
    queue: Sender<T>,
    terminal: Arc<OnceLock<StreamEnd<E>>>,
}

impl<T> Stream<T> {
    /// A stream nobody is subscribed to yet, with no failure payload.
    pub fn new() -> Self {
        Self::default()
    }

    // Platform input is a local inbox, not a cross-world subscription. It has
    // to keep releases and focus changes even while its consumer is paused.
    pub(crate) fn input_inbox(&self) -> StreamReader<T> {
        self.attach(unbounded())
    }
}

impl<T, E: Clone> Stream<T, E> {
    /// A reader with its own queue. When it fills, only this reader is ended.
    pub fn subscribe(&self, capacity: NonZeroUsize) -> StreamReader<T, E> {
        self.attach(bounded(capacity.get()))
    }

    fn attach(&self, (queue, receiver): (Sender<T>, Receiver<T>)) -> StreamReader<T, E> {
        let terminal = Arc::new(OnceLock::new());
        let mut inner = sync::lock(&self.inner);
        let mut sender = StreamSender {
            queue,
            terminal: Arc::clone(&terminal),
        };
        match &inner.terminal {
            Some(end) => sender.finish(end.clone()),
            None => inner.senders.push(sender),
        }
        StreamReader {
            inner: Arc::new(ReaderInner {
                queue: receiver,
                batch: Mutex::new(Arc::default()),
                terminal,
            }),
        }
    }

    /// Close the stream without discarding queued values.
    pub fn close(&self) {
        self.finish(StreamEnd::Closed);
    }

    /// Fail the stream without discarding queued values.
    pub fn fail(&self, error: E) {
        self.finish(StreamEnd::Failed(error));
    }

    fn finish(&self, end: StreamEnd<E>) {
        let mut inner = sync::lock(&self.inner);
        if inner.terminal.is_none() {
            for mut sender in inner.senders.drain(..) {
                sender.finish(end.clone());
            }
            inner.terminal = Some(end);
        }
    }
}

impl<T, E> StreamSender<T, E> {
    fn finish(&mut self, end: StreamEnd<E>) {
        let _ = self.terminal.set(end);
        self.queue.close();
    }
}

impl<T: Clone, E> Stream<T, E> {
    /// Publish one value and return the number of subscribers still receiving.
    pub fn publish(&self, value: T) -> usize {
        self.publish_all([value])
    }

    /// Publish in order. A full queue keeps its accepted prefix and receives
    /// no further values. Publishing never waits for a subscriber to pump.
    pub fn publish_all(&self, values: impl IntoIterator<Item = T>) -> usize {
        let mut inner = sync::lock(&self.inner);
        inner.senders.retain(|sender| !sender.queue.is_closed());
        for value in values {
            if inner.senders.is_empty() {
                break;
            }
            inner
                .senders
                .retain_mut(|sender| match sender.queue.try_send(value.clone()) {
                    Ok(()) => true,
                    Err(TrySendError::Closed(_)) => false,
                    Err(TrySendError::Full(_)) => {
                        sender.finish(StreamEnd::Overflow);
                        false
                    }
                });
        }
        inner.senders.len()
    }
}

impl<T, E> Stream<T, E> {
    /// Whether a subscription is still accepting values.
    pub fn has_readers(&self) -> bool {
        let mut inner = sync::lock(&self.inner);
        inner.senders.retain(|sender| !sender.queue.is_closed());
        !inner.senders.is_empty()
    }
}

impl<T, E> Clone for Stream<T, E> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T, E> Default for Stream<T, E> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StreamInner {
                senders: Vec::new(),
                terminal: None,
            })),
        }
    }
}

impl<T, E> fmt::Debug for Stream<T, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Stream").finish_non_exhaustive()
    }
}

/// An event reader with one queue and batch shared by all its clones.
pub struct StreamReader<T, E = Infallible> {
    inner: Arc<ReaderInner<T, E>>,
}

struct ReaderInner<T, E> {
    queue: Receiver<T>,
    batch: Mutex<Arc<Vec<T>>>,
    terminal: Arc<OnceLock<StreamEnd<E>>>,
}

impl<T, E> StreamReader<T, E> {
    /// Replace the previous batch with the events queued at the start of the
    /// pump. Return the terminal state only on an empty batch, so queued
    /// events are delivered before closure, failure, or overflow.
    #[must_use]
    pub fn pump(&self) -> Option<StreamEnd<E>>
    where
        E: Clone,
    {
        let mut batch = sync::lock(&self.inner.batch);
        if Arc::get_mut(&mut batch).is_none() {
            *batch = Arc::default();
        }
        let values = Arc::get_mut(&mut batch).expect("the batch is exclusively owned");
        values.clear();
        for _ in 0..self.inner.queue.len() {
            if let Ok(value) = self.inner.queue.try_recv() {
                values.push(value);
            }
        }
        if values.is_empty() && self.inner.queue.is_closed() && self.inner.queue.is_empty() {
            return Some(
                self.inner
                    .terminal
                    .get()
                    .cloned()
                    .unwrap_or(StreamEnd::Closed),
            );
        }
        None
    }

    /// Read this tick's events in order, without consuming them or holding a
    /// lock while `f` runs.
    pub fn with_batch<R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        let batch = Arc::clone(&sync::lock(&self.inner.batch));
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

impl<T, E> Clone for StreamReader<T, E> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T, E> fmt::Debug for StreamReader<T, E> {
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
    fn snapshots_report_updates_per_reader_and_keep_the_last_value_on_failure() {
        let snapshot = Snapshot::<u32, &str>::default();
        let mut first = snapshot.reader();
        let mut second = snapshot.reader();
        assert_eq!(first.poll(), SnapshotPoll::Uninitialized);
        snapshot.publish(1);
        assert_eq!(first.poll(), SnapshotPoll::Updated(Arc::new(1)));
        assert_eq!(first.poll(), SnapshotPoll::Unchanged);
        assert_eq!(second.poll(), SnapshotPoll::Updated(Arc::new(1)));
        snapshot.publish(1);
        assert_eq!(first.poll(), SnapshotPoll::Updated(Arc::new(1)));
        snapshot.fail("collector failed");
        snapshot.publish(2);
        assert_eq!(first.poll(), SnapshotPoll::Failed("collector failed"));
        assert_eq!(first.latest().as_deref(), Some(&1));
    }

    #[test]
    fn a_snapshot_closes_when_the_last_producer_leaves() {
        let snapshot = Snapshot::<u32>::new();
        let other = snapshot.clone();
        let mut reader = snapshot.reader();
        drop(snapshot);
        assert_eq!(reader.poll(), SnapshotPoll::Uninitialized);
        other.publish(1);
        drop(other);
        assert_eq!(reader.poll(), SnapshotPoll::Closed);
        assert_eq!(reader.latest().as_deref(), Some(&1));
    }

    #[test]
    fn overflow_ends_only_the_lagging_reader_after_its_queued_values() {
        let stream = Stream::<u32>::new();
        let slow = stream.subscribe(NonZeroUsize::MIN);
        let fast = stream.subscribe(NonZeroUsize::MIN);
        assert_eq!(stream.publish(1), 2);
        assert_eq!(fast.pump(), None);
        assert_eq!(stream.publish(2), 1);
        assert_eq!(fast.pump(), None);
        assert_eq!(fast.with_batch(|batch| batch.to_vec()), [2]);
        assert_eq!(slow.pump(), None);
        assert_eq!(slow.with_batch(|batch| batch.to_vec()), [1]);
        assert_eq!(slow.pump(), Some(StreamEnd::Overflow));
        assert_eq!(slow.pump(), Some(StreamEnd::Overflow));
    }

    #[test]
    fn a_stream_drains_its_queue_before_reporting_failure() {
        let stream = Stream::<u32, &str>::default();
        let reader = stream.subscribe(NonZeroUsize::MIN);
        stream.publish(1);
        stream.fail("collector failed");
        assert_eq!(stream.publish(2), 0);
        assert_eq!(reader.pump(), None);
        assert_eq!(reader.with_batch(|batch| batch.to_vec()), [1]);
        assert_eq!(reader.pump(), Some(StreamEnd::Failed("collector failed")));
        let late = stream.subscribe(NonZeroUsize::MIN);
        assert_eq!(late.pump(), Some(StreamEnd::Failed("collector failed")));
    }

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
        let one = stream.subscribe(NonZeroUsize::new(8).unwrap());
        let other = stream.subscribe(NonZeroUsize::new(8).unwrap());
        assert_eq!(stream.publish(1), 2);
        assert_eq!(stream.publish(2), 2);
        assert_eq!(one.pump(), None);
        assert_eq!(other.pump(), None);
        assert_eq!(one.with_batch(|batch| batch.to_vec()), vec![1, 2]);
        assert_eq!(other.with_batch(|batch| batch.to_vec()), vec![1, 2]);
    }

    #[test]
    fn a_pump_replaces_the_previous_batch() {
        let stream = Stream::new();
        let reader = stream.subscribe(NonZeroUsize::new(8).unwrap());
        stream.publish(1);
        assert_eq!(reader.pump(), None);
        stream.publish(2);
        assert_eq!(reader.pump(), None);
        assert_eq!(reader.with_batch(|batch| batch.to_vec()), vec![2]);
    }

    #[test]
    fn a_stream_prunes_a_reader_that_went_away() {
        let stream = Stream::new();
        let reader = stream.subscribe(NonZeroUsize::new(8).unwrap());
        assert!(stream.has_readers());
        drop(reader);
        assert!(!stream.has_readers());
        assert_eq!(stream.publish(1), 0);
    }

    /// A callback that reads another stream - or this one again - runs with
    /// no lock held, so it cannot deadlock against itself.
    #[test]
    fn a_batch_callback_may_read_a_batch() {
        let stream = Stream::new();
        let other = Stream::new();
        let reader = stream.subscribe(NonZeroUsize::new(8).unwrap());
        let other_reader = other.subscribe(NonZeroUsize::new(8).unwrap());
        stream.publish(1);
        other.publish(2);
        assert_eq!(reader.pump(), None);
        assert_eq!(other_reader.pump(), None);
        let nested = reader.with_batch(|batch| {
            let inner = reader.with_batch(|batch| batch.to_vec());
            let elsewhere = other_reader.with_batch(|batch| batch.to_vec());
            (batch.to_vec(), inner, elsewhere)
        });
        assert_eq!(nested, (vec![1], vec![1], vec![2]));
    }

    #[test]
    fn a_reader_clone_shares_the_queue() {
        let stream = Stream::new();
        let reader = stream.subscribe(NonZeroUsize::new(8).unwrap());
        let clone = reader.clone();
        stream.publish(1);
        assert_eq!(reader.pump(), None);
        assert_eq!(clone.with_batch(|batch| batch.to_vec()), vec![1]);
        drop(reader);
        assert!(stream.has_readers());
    }
}
