//! Subscriptions: what a world reports to the others, and how they read it.
//!
//! A subscription is a job that installs a collector in another world. That
//! world runs the collector once per tick, after the state it reads has been
//! updated, and the collector hands back what it found. Two kinds of thing can
//! come back, and they differ in whether history matters:
//!
//! - A **snapshot** keeps the newest value only. Publishing replaces the
//!   slot, so a reader that skips a tick misses history, never the present.
//!   That is the shape state wants: poses, a playback position, the size an overlay
//!   is laid out at.
//! - A **stream** keeps every value, once per reader. The collector appends
//!   what it found to each reader's queue, and the reader folds that queue
//!   into its own tick with [`StreamReader::pump`](unlit3d::relay::StreamReader::pump). That is the shape events
//!   want: input, collisions, a sequence ending. A reader that fills its queue
//!   drains the accepted events before observing overflow.
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

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use unlit_ecs::EntityAccessError;
use unlit_ecs::World;
use unlit_ecs::bundle;
use unlit_ecs::sync;

use crate::job::{JobSender, SendError};

use crate::turn::WorldError;

pub use unlit3d::relay::{Snapshot, SnapshotPoll, SnapshotReader, Stream, StreamEnd, StreamReader};

impl JobSender {
    /// Watch a snapshot of this world.
    ///
    /// `collect` runs on the world's own thread once per tick, after its state
    /// has been updated, and whatever it returns is published to the reader.
    /// Returning [`None`] leaves the newest snapshot in place, which is how a
    /// collector stays quiet while nothing has changed.
    ///
    /// The subscription is a job: it is refused with the queue's error when
    /// the queue is full or the world has stopped. Being queued is not being
    /// installed — the job runs on the world's next turn, or never, if the
    /// world stops first — so the reader is not handed over here: the
    /// [`Subscription`] returned carries it and answers
    /// [`poll`](Subscription::poll) with it once the world has installed the
    /// collector.
    ///
    /// # Panics
    ///
    /// A collector that publishes into its own world — by calling
    /// [`publish_collected`] — panics on the borrow it takes of the
    /// subscription list. A collector only reads the world.
    pub fn watch<T, F>(
        &self,
        collect: F,
    ) -> Result<Subscription<SnapshotReader<T, WorldError>>, SendError>
    where
        T: Send + Sync + 'static,
        F: FnMut(&World) -> Option<T> + Send + 'static,
    {
        let snapshot = Snapshot::default();
        let reader = snapshot.reader();
        let state = Arc::new(Mutex::new(InstallState::Pending));
        let receipt = Receipt {
            state: Arc::clone(&state),
        };
        self.send(Box::new(move |world: &mut World| {
            let entry: Box<dyn Entry> = Box::new(SnapshotCollector { collect, snapshot });
            receipt.settle(install(world, entry));
        }))?;
        Ok(Subscription { reader, state })
    }

    /// Subscribe to a stream of this world.
    ///
    /// `collect` runs on the world's own thread once per tick and appends
    /// whatever it found to `out`, which is reused across ticks. The reader
    /// drains its queue with [`StreamReader::pump`]. If that queue hits
    /// `capacity`, the reader drains the accepted prefix and reports overflow.
    /// A world failure ends the subscription with the original [`WorldError`].
    ///
    /// The subscription is a job: it is refused with the queue's error when
    /// the queue is full or the world has stopped, and the reader is handed
    /// over by [`poll`](Subscription::poll) once the world has run the job,
    /// for the reasons [`watch`](JobSender::watch) gives.
    ///
    /// # Panics
    ///
    /// A collector that publishes into its own world — by calling
    /// [`publish_collected`] — panics on the borrow it takes of the
    /// subscription list. A collector only reads the world.
    pub fn subscribe<T, F>(
        &self,
        capacity: NonZeroUsize,
        collect: F,
    ) -> Result<Subscription<StreamReader<T, WorldError>>, SendError>
    where
        T: Clone + Send + 'static,
        F: FnMut(&World, &mut Vec<T>) + Send + 'static,
    {
        let stream = Stream::default();
        let reader = stream.subscribe(capacity);
        let state = Arc::new(Mutex::new(InstallState::Pending));
        let receipt = Receipt {
            state: Arc::clone(&state),
        };
        self.send(Box::new(move |world: &mut World| {
            let entry: Box<dyn Entry> = Box::new(StreamCollector {
                collect,
                stream,
                buffer: Vec::new(),
            });
            receipt.settle(install(world, entry));
        }))?;
        Ok(Subscription { reader, state })
    }
}

/// A subscription that has been queued for a world but may not be installed yet.
///
/// The job that installs a collector runs on the target world's own turn, so a
/// caller cannot have the reader at the moment it asks for one: a world that
/// stops before that turn never installs it, and a reader handed over anyway
/// would look exactly like one whose world has not ticked. This holds the
/// reader and reports whether the collector was ever put in place.
///
/// Installing is not publishing: a reader answers with nothing until the
/// world's first tick after the install collects a value.
pub struct Subscription<R> {
    reader: R,
    state: Arc<Mutex<InstallState>>,
}

impl<R: Clone> Clone for Subscription<R> {
    /// A copy asks the same question of the same installation, and is one
    /// more reader for the collector to publish to until it is dropped.
    fn clone(&self) -> Self {
        Self {
            reader: self.reader.clone(),
            state: Arc::clone(&self.state),
        }
    }
}

impl<R: Clone> Subscription<R> {
    /// The reader, once the target world has installed the collector.
    ///
    /// Answers [`None`] while the installing job has not run: a caller that
    /// drives the world steps it and asks again, and one that does not drive
    /// it asks on its own next turn. Waiting here would wait on a world only
    /// the caller can advance. A subscriber whose collector will never be
    /// installed answers with the reason instead.
    #[must_use]
    pub fn poll(&self) -> Option<Result<R, InstallError>> {
        let state = *sync::lock(&self.state);
        match state {
            InstallState::Pending => None,
            InstallState::Ready => Some(Ok(self.reader.clone())),
            InstallState::Failed(error) => Some(Err(error)),
        }
    }
}

/// Why a subscription was never installed in its world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    /// The world ended before it ran the job that installs the collector.
    #[error("the world ended before the subscription was installed")]
    Ended,
    /// The subscription outlet the world named was not in reach.
    #[error("the subscription outlet of the world could not be reached: {0}")]
    Outlet(#[from] EntityAccessError),
}

/// What became of the install job, shared by the job and the subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallState {
    /// The world has not run the job yet.
    Pending,
    /// The collector is in the world.
    Ready,
    /// The collector will never be in the world.
    Failed(InstallError),
}

/// The job's side of a subscription: settles the state it shares with the
/// [`Subscription`], and fails it if the job is dropped without running.
///
/// A world that stops closes its queue and drops the jobs still in it, which
/// is how a subscription that will never be installed reports that rather than
/// waiting for a turn that will not come.
struct Receipt {
    state: Arc<Mutex<InstallState>>,
}

impl Receipt {
    /// Record what the install did.
    fn settle(&self, installed: Result<(), InstallError>) {
        *sync::lock(&self.state) = match installed {
            Ok(()) => InstallState::Ready,
            Err(error) => InstallState::Failed(error),
        };
    }
}

impl Drop for Receipt {
    fn drop(&mut self) {
        let mut state = sync::lock(&self.state);
        if matches!(*state, InstallState::Pending) {
            *state = InstallState::Failed(InstallError::Ended);
        }
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
    let scope = world.root_scope();
    let Some(entity) = scope.singleton::<SnapshotOutlet>() else {
        return 0;
    };
    let Ok(mut snapshot_outlet) = scope.try_query_one::<&mut SnapshotOutlet>(entity) else {
        return 0;
    };
    let outlet = snapshot_outlet.get();
    outlet.entries.retain_mut(|entry| entry.publish(world));
    outlet.entries.len()
}

/// One subscription inside a world: what to collect and where to put it.
trait Entry {
    /// Collect and publish one tick's worth; answers whether a reader is alive.
    fn publish(&mut self, world: &World) -> bool;
    fn finish(&self, error: Option<&WorldError>);
}

/// A subscription that keeps the newest value its collector returns.
struct SnapshotCollector<T, F> {
    collect: F,
    snapshot: Snapshot<T, WorldError>,
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

    fn finish(&self, error: Option<&WorldError>) {
        match error {
            Some(error) => self.snapshot.fail(error.clone()),
            None => self.snapshot.close(),
        }
    }
}

/// A subscription that queues everything its collector appends.
struct StreamCollector<T, F> {
    collect: F,
    stream: Stream<T, WorldError>,
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

    fn finish(&self, error: Option<&WorldError>) {
        match error {
            Some(error) => self.stream.fail(error.clone()),
            None => self.stream.close(),
        }
    }
}

pub(crate) fn finish_collected(world: &World, error: Option<&WorldError>) {
    let scope = world.root_scope();
    let Some(entity) = scope.singleton::<SnapshotOutlet>() else {
        return;
    };
    let Ok(mut outlet) = scope.try_query_one::<&SnapshotOutlet>(entity) else {
        return;
    };
    for entry in &outlet.get().entries {
        entry.finish(error);
    }
}

/// Every subscription a world carries, as one component.
///
/// The first subscription to arrive creates it; it lives as long as the world
/// does, and an entry is pruned once its reader is dropped. It claims the
/// [`SnapshotOutlet`] singleton key, so a world carries at most one outlet —
/// subscriptions from every producer share it.
struct SnapshotOutlet {
    entries: Vec<Box<dyn Entry>>,
}

/// Add one subscription to `world`, creating the outlet on first use.
///
/// The key answers only for a holder whose row is still there, and the outlet
/// is not republished or removed by anything else, so a row that is gone under
/// its key is a world torn down in a way this cannot serve. It is reported
/// rather than assumed away: a reader whose collector was never installed has
/// to be told that, or it waits for values on the same terms as one whose
/// world has not ticked yet.
fn install(world: &mut World, entry: Box<dyn Entry>) -> Result<(), InstallError> {
    let existing = world.root_scope().singleton::<SnapshotOutlet>();
    match existing {
        Some(entity) => {
            let mut outlet = world
                .root_scope()
                .try_query_one::<&mut SnapshotOutlet>(entity)?;
            outlet.get().entries.push(entry);
            Ok(())
        }
        None => {
            world.root_scope_mut().spawn(
                bundle![SnapshotOutlet {
                    entries: vec![entry],
                }]
                .singleton::<SnapshotOutlet>(),
            );
            Ok(())
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
    ) -> SnapshotReader<u32, WorldError> {
        let snapshot = Snapshot::default();
        let reader = snapshot.reader();
        install(world, Box::new(SnapshotCollector { collect, snapshot }))
            .expect("a fresh world takes a subscription");
        reader
    }

    /// The outlet a world carries can be removed, and the next subscription
    /// then finds no key to join: it claims the key again rather than failing,
    /// so a reader never waits on an outlet nobody will run.
    #[test]
    fn an_outlet_that_was_removed_is_claimed_again() {
        let mut world = World::new();
        let first = subscribe_in(&mut world, |_| Some(1));
        let outlet = world
            .root_scope()
            .singleton::<SnapshotOutlet>()
            .expect("the first subscription spawned an outlet");
        world
            .root_scope_mut()
            .despawn(outlet)
            .expect("the outlet despawns");

        let second = subscribe_in(&mut world, |_| Some(2));
        assert_eq!(publish_collected(&world), 1);
        assert_eq!(*second.latest().expect("a snapshot"), 2);
        assert!(
            first.latest().is_none(),
            "the reader of the removed outlet gets nothing, as it carries no collector",
        );
    }

    /// A subscription is a job, and being queued is not being installed: the
    /// handle answers nothing until the world's turn runs the job.
    #[test]
    fn a_queued_subscription_answers_nothing() {
        let (sender, receiver) = async_channel::bounded(1);
        let jobs = JobSender::new(sender);
        let subscription = jobs
            .watch(|_| Some(1u32))
            .expect("the queue has room for the subscription");
        assert!(
            subscription.poll().is_none(),
            "a subscription its world has not run yet is still in the queue"
        );

        let mut world = World::new();
        receiver.try_recv().expect("the installing job is queued")(&mut world);
        assert!(
            subscription
                .poll()
                .expect("the job ran")
                .expect("the outlet is reachable")
                .latest()
                .is_none(),
            "installing the collector publishes nothing on its own"
        );
    }

    /// A world that stopped before the job ran took the installation with it:
    /// the handle says the subscription ended rather than leaving a reader
    /// that would never fill.
    #[test]
    fn a_subscription_the_world_stopped_before_installing_ended() {
        let (sender, receiver) = async_channel::bounded(1);
        let jobs = JobSender::new(sender);
        let subscription = jobs
            .watch(|_| Some(1u32))
            .expect("the queue has room for the subscription");

        // What a world does on its way out: the queue is closed and every job
        // still in it goes with it, unrun.
        receiver.close();
        while receiver.try_recv().is_ok() {}

        match subscription.poll().expect("the subscription ended") {
            Ok(_) => panic!("a subscription whose job was dropped was installed"),
            Err(error) => assert_eq!(error, InstallError::Ended),
        }
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
        let stream = Stream::default();
        let reader = stream.subscribe(NonZeroUsize::new(8).unwrap());
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
        )
        .expect("a fresh world takes a subscription");

        publish_collected(&world);
        publish_collected(&world);
        assert_eq!(reader.pump(), None);
        assert_eq!(
            reader.with_batch(|batch| batch.to_vec()),
            vec![1, 10, 2, 20]
        );

        // A second pump with nothing new replaces the batch rather than
        // repeating the events of the previous tick.
        assert_eq!(reader.pump(), None);
        assert!(reader.with_batch(|batch| batch.to_vec()).is_empty());
    }
}
