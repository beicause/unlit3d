//! Subscriptions: what a world reports to the others, and how they read it.
//!
//! A subscription is a job that installs a collector in another world. That
//! world runs the collector once per tick, after the state it reads has been
//! updated, and the collector hands back what it found. Two kinds of thing can
//! come back, and they differ in whether history matters:
//!
//! - A **snapshot** keeps the newest value only. Publishing replaces the
//!   slot, so a reader that skips a tick misses history, never the present.
//!   That is the shape state wants: poses, a playback position, the size a UI
//!   is laid out at.
//! - A **stream** keeps every value, once per reader. The collector appends
//!   what it found to each reader's queue, and the reader folds that queue
//!   into its own tick with [`StreamReader::pump`](unlit3d::relay::StreamReader::pump). That is the shape events
//!   want: input, collisions, a sequence ending. A reader that ticks slower
//!   than the producer still sees every value, in order.
//!
//! A third shape is a question, and it is not here: [`Pending`](crate::request::Pending)
//! sends one job per request and folds each answer back under the key the
//! caller asked with. That is the shape of something built on request rather
//! than observed: an entity, a set of assets, a body spawned for one ball.
//!
//! Nothing is collected unasked: a world runs the collectors of its
//! subscriptions only while someone holds a reader, and a dropped reader is
//! pruned on the next tick. Both kinds are installed with a job, so every
//! world offers them the same way, built-in or not, and the reading side
//! never blocks.

use unlit_ecs::World;
use unlit_ecs::{Entity, bundle};

use crate::job::{JobSender, SendError};

pub use unlit3d::relay::{Snapshot, SnapshotReader, Stream, StreamReader};

impl JobSender {
    /// Watch a snapshot of this world.
    ///
    /// `collect` runs on the world's own thread once per tick, after its state
    /// has been updated, and whatever it returns is published to the reader.
    /// Returning [`None`] leaves the newest snapshot in place, which is how a
    /// collector stays quiet while nothing has changed.
    ///
    /// The subscription is a job: it is refused with the queue's error when
    /// the queue is full or the world has stopped, and the returned reader
    /// then never sees a snapshot.
    ///
    /// # Panics
    ///
    /// A collector that publishes into its own world — by calling
    /// [`publish_collected`] — panics on the borrow it takes of the
    /// subscription list. A collector only reads the world.
    pub fn watch<T, F>(&self, collect: F) -> Result<SnapshotReader<T>, SendError>
    where
        T: Send + Sync + 'static,
        F: FnMut(&World) -> Option<T> + Send + 'static,
    {
        let snapshot = Snapshot::new();
        let reader = snapshot.reader();
        self.send(Box::new(move |world: &mut World| {
            let entry: Box<dyn Entry> = Box::new(SnapshotCollector { collect, snapshot });
            install(world, entry);
        }))?;
        Ok(reader)
    }

    /// Subscribe to a stream of this world.
    ///
    /// `collect` runs on the world's own thread once per tick and appends
    /// whatever it found to `out`, which is reused across ticks. Every value
    /// reaches every reader's queue; the reader drains its queue in its own
    /// tick with [`StreamReader::pump`].
    ///
    /// The subscription is a job: it is refused with the queue's error when
    /// the queue is full or the world has stopped, and the returned reader
    /// then never sees a value.
    ///
    /// # Panics
    ///
    /// A collector that publishes into its own world — by calling
    /// [`publish_collected`] — panics on the borrow it takes of the
    /// subscription list. A collector only reads the world.
    pub fn subscribe<T, F>(&self, collect: F) -> Result<StreamReader<T>, SendError>
    where
        T: Clone + Send + 'static,
        F: FnMut(&World, &mut Vec<T>) + Send + 'static,
    {
        let stream = Stream::new();
        let reader = stream.subscribe();
        self.send(Box::new(move |world: &mut World| {
            let entry: Box<dyn Entry> = Box::new(StreamCollector {
                collect,
                stream,
                buffer: Vec::new(),
            });
            install(world, entry);
        }))?;
        Ok(reader)
    }
}

/// Run every subscription the world carries, publishing what each collects.
///
/// Returns how many subscriptions are still alive, so a world can skip the
/// work its collectors would have done when there are none. A reader that has
/// been dropped is pruned here: the world stops collecting for it on the next
/// tick.
///
/// A runner that drives a world — its thread, the host stepping it, or the
/// host draining a sub-world — calls this itself at the end of every turn,
/// after the state a collector reads has been updated, so a tick body
/// publishes nothing by hand. That is the runner's contract rather than a
/// guard: a host that drives a world of its own calls this once per turn, and
/// calling it twice would run every collector twice.
pub fn publish_collected(world: &World) -> usize {
    let mut snapshot_outlet_rows = world.root_scope().query::<&mut SnapshotOutlet>();

    let Some(outlet) = snapshot_outlet_rows.iter().next() else {
        return 0;
    };
    outlet.entries.retain_mut(|entry| entry.publish(world));
    outlet.entries.len()
}

/// One subscription inside a world: what to collect and where to put it.
trait Entry {
    /// Collect and publish one tick's worth; answers whether a reader is alive.
    fn publish(&mut self, world: &World) -> bool;
}

/// A subscription that keeps the newest value its collector returns.
struct SnapshotCollector<T, F> {
    collect: F,
    snapshot: Snapshot<T>,
}

impl<T, F> Entry for SnapshotCollector<T, F>
where
    T: Send + Sync + 'static,
    F: FnMut(&World) -> Option<T> + 'static,
{
    fn publish(&mut self, world: &World) -> bool {
        // Asked before the collector runs: a reader that is gone must not
        // cost another collection.
        if !self.snapshot.has_readers() {
            return false;
        }
        if let Some(value) = (self.collect)(world) {
            self.snapshot.publish(value);
        }
        true
    }
}

/// A subscription that queues everything its collector appends.
struct StreamCollector<T, F> {
    collect: F,
    stream: Stream<T>,
    /// Reused across ticks so a collector never allocates its result.
    buffer: Vec<T>,
}

impl<T, F> Entry for StreamCollector<T, F>
where
    T: Clone + Send + 'static,
    F: FnMut(&World, &mut Vec<T>) + 'static,
{
    fn publish(&mut self, world: &World) -> bool {
        // Asked before the collector runs: a reader that is gone must not
        // cost another collection.
        if !self.stream.has_readers() {
            return false;
        }
        self.buffer.clear();
        (self.collect)(world, &mut self.buffer);
        self.stream.publish_all(self.buffer.drain(..)) > 0
    }
}

/// Every subscription a world carries, as one component.
///
/// The first subscription to arrive creates it; it lives as long as the world
/// does, and an entry is pruned once its reader is dropped.
struct SnapshotOutlet {
    entries: Vec<Box<dyn Entry>>,
}

/// Add one subscription to `world`, creating the outlet on first use.
fn install(world: &mut World, entry: Box<dyn Entry>) {
    let existing = world
        .root_scope()
        .query::<(&SnapshotOutlet, Entity)>()
        .iter()
        .next()
        .map(|(_snapshot_outlet, entity)| entity);
    match existing {
        Some(entity) => {
            let mut outlet = world
                .root_scope()
                .query_one::<&mut SnapshotOutlet>(entity)
                .expect("the subscription outlet exists");
            outlet.get().entries.push(entry);
        }
        None => {
            world.root_scope_mut().spawn(bundle![SnapshotOutlet {
                entries: vec![entry],
            },]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Install a collector into a fresh world, returning the reader it fills.
    fn subscribe_in(
        world: &mut World,
        collect: impl FnMut(&World) -> Option<u32> + 'static,
    ) -> SnapshotReader<u32> {
        let snapshot = Snapshot::new();
        let reader = snapshot.reader();
        install(world, Box::new(SnapshotCollector { collect, snapshot }));
        reader
    }

    /// A world with no subscription has nothing to run.
    #[test]
    fn a_world_without_subscriptions_publishes_nothing() {
        assert_eq!(publish_collected(&World::new()), 0);
    }

    /// Each tick publishes what the collector returns, newest wins.
    #[test]
    fn a_subscription_publishes_what_its_collector_returns() {
        let ticks = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&ticks);
        let mut world = World::new();
        let reader = subscribe_in(&mut world, move |_| {
            Some(counted.fetch_add(1, Ordering::SeqCst))
        });

        assert_eq!(publish_collected(&world), 1);
        assert_eq!(publish_collected(&world), 1);
        assert_eq!(*reader.latest().expect("a snapshot"), 1);
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
    }

    /// A collector that answers nothing leaves the newest snapshot in place.
    #[test]
    fn a_collector_that_returns_nothing_leaves_the_newest() {
        let mut first = true;
        let mut world = World::new();
        let reader = subscribe_in(&mut world, move |_| {
            let value = if first { Some(1) } else { None };
            first = false;
            value
        });

        publish_collected(&world);
        publish_collected(&world);
        assert_eq!(*reader.latest().expect("the first snapshot"), 1);
    }

    /// Dropping the reader prunes the subscription on the next tick.
    #[test]
    fn dropping_the_reader_prunes_the_subscription() {
        let mut world = World::new();
        let reader = subscribe_in(&mut world, |_| Some(1));
        assert_eq!(publish_collected(&world), 1);
        drop(reader);
        assert_eq!(publish_collected(&world), 0);
    }

    /// A snapshot already read stays valid and unchanged while later ones are
    /// published: the producer swaps the slot, it does not write into what a
    /// reader holds.
    #[test]
    fn a_read_snapshot_survives_later_publications() {
        let mut tick = 0;
        let mut world = World::new();
        let reader = subscribe_in(&mut world, move |_| {
            tick += 1;
            Some(tick)
        });

        publish_collected(&world);
        let first = reader.latest().expect("the first snapshot");
        assert_eq!(*first, 1);

        publish_collected(&world);
        let second = reader.latest().expect("the second snapshot");
        assert_eq!(*second, 2);
        assert_eq!(*first, 1, "the held snapshot did not change under it");
    }

    /// Every value a stream collector appends reaches every reader, and each
    /// reader folds its own queue on its own tick.
    #[test]
    fn a_stream_subscription_delivers_every_value() {
        let mut tick = 0;
        let mut world = World::new();
        let stream = Stream::new();
        let reader = stream.subscribe();
        install(
            &mut world,
            Box::new(StreamCollector {
                collect: move |_world: &World, out: &mut Vec<u32>| {
                    tick += 1;
                    out.extend([tick, tick * 10]);
                },
                stream,
                buffer: Vec::new(),
            }),
        );

        publish_collected(&world);
        publish_collected(&world);
        reader.pump();
        assert_eq!(
            reader.with_batch(|batch| batch.to_vec()),
            vec![1, 10, 2, 20]
        );

        // A second pump with nothing new replaces the batch rather than
        // repeating the events of the previous tick.
        reader.pump();
        assert!(reader.with_batch(|batch| batch.to_vec()).is_empty());
    }
}
