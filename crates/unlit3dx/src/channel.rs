//! Snapshot mailboxes: a world reports its state to whoever subscribed.
//!
//! A snapshot is how a world reports state to the others without ever calling
//! back: the producer replaces the slot's contents, the consumers read
//! whatever is newest, and no older snapshot is ever queued behind a slow
//! reader. A reader that skips a snapshot misses nothing but history, which is
//! what makes a mailbox right for state and wrong for events: an event that
//! must not be missed belongs in a queue the collector fills, not in the slot.
//!
//! Nothing is published unasked: a world collects a snapshot only while
//! someone holds the reader that subscribed for it. A subscription is a job
//! like any other, so every world offers it the same way, built-in or not.
//!
//! The slot is an `ArcSwapOption`, so a producer never waits on a reader: it
//! swaps the newest snapshot in, and a reader already holding the previous one
//! keeps reading that one to the end.

use std::fmt;
use std::sync::{Arc, Weak};

use arc_swap::ArcSwapOption;
use unlit_ecs::World;

use crate::job::{JobSender, SendError};
use unlit_ecs::{bundle, query};

/// The reading half of a snapshot mailbox, and the subscription that fills it.
///
/// A world runs the collector its subscription carries once per tick and
/// publishes what it returns. The subscription lives as long as this reader:
/// once every clone of it is dropped, the world stops collecting for it.
pub struct SnapshotReader<T> {
    slot: Arc<ArcSwapOption<T>>,
}

// Both are hand-written: a reader is a handle to a slot, so it is cloneable
// and debuggable whether or not the payload it carries is.
impl<T> Clone for SnapshotReader<T> {
    fn clone(&self) -> Self {
        Self {
            slot: Arc::clone(&self.slot),
        }
    }
}

impl<T> fmt::Debug for SnapshotReader<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotReader")
            .field("published", &self.slot.load().is_some())
            .finish()
    }
}

impl<T> SnapshotReader<T> {
    /// The newest snapshot, or [`None`] if none was ever published.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<T>> {
        self.slot.load_full()
    }
}

impl JobSender {
    /// Subscribe to a snapshot of this world.
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
    /// [`publish_subscribed`] — panics on the borrow it takes of the
    /// subscription list. A collector only reads the world.
    pub fn subscribe<T, F>(&self, collect: F) -> Result<SnapshotReader<T>, SendError>
    where
        T: Send + Sync + 'static,
        F: FnMut(&World) -> Option<T> + Send + 'static,
    {
        let slot = Arc::new(ArcSwapOption::empty());
        let reader = SnapshotReader {
            slot: Arc::clone(&slot),
        };
        let weak = Arc::downgrade(&slot);
        self.send(Box::new(move |world: &mut World| {
            let entry: Box<dyn Entry> = Box::new(Collector {
                collect,
                slot: weak,
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
/// Call this from the world's own tick, after the state a collector reads has
/// been updated.
pub fn publish_subscribed(world: &World) -> usize {
    let Some((_, row)) = world.root_scope().query(query!(&mut SnapshotOutlet)).next() else {
        return 0;
    };
    let mut outlet = row.write::<SnapshotOutlet>();
    outlet.entries.retain_mut(|entry| entry.publish(world));
    outlet.entries.len()
}

/// One subscription inside a world: what to collect and where to put it.
trait Entry {
    /// Collect and publish one snapshot; answers whether the reader is alive.
    fn publish(&mut self, world: &World) -> bool;
}

/// A subscription's collector and a way back to the mailbox it fills.
struct Collector<T, F> {
    collect: F,
    slot: Weak<ArcSwapOption<T>>,
}

impl<T, F> Entry for Collector<T, F>
where
    T: Send + Sync + 'static,
    F: FnMut(&World) -> Option<T> + 'static,
{
    fn publish(&mut self, world: &World) -> bool {
        let Some(slot) = self.slot.upgrade() else {
            return false;
        };
        if let Some(snapshot) = (self.collect)(world) {
            slot.store(Some(Arc::new(snapshot)));
        }
        true
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
        .query(query!(&SnapshotOutlet))
        .next()
        .map(|(entity, _)| entity);
    match existing {
        Some(entity) => world
            .root_scope()
            .get_mut::<SnapshotOutlet>(entity)
            .expect("the subscription outlet exists")
            .entries
            .push(entry),
        None => {
            world.root_scope_mut().spawn(bundle!(SnapshotOutlet {
                entries: vec![entry],
            },));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Install a collector into a fresh world, returning the reader it fills.
    fn subscribe_in(
        world: &mut World,
        collect: impl FnMut(&World) -> Option<u32> + 'static,
    ) -> SnapshotReader<u32> {
        let slot = Arc::new(ArcSwapOption::empty());
        let reader = SnapshotReader {
            slot: Arc::clone(&slot),
        };
        install(
            world,
            Box::new(Collector {
                collect,
                slot: Arc::downgrade(&slot),
            }),
        );
        reader
    }

    /// A world with no subscription has nothing to run.
    #[test]
    fn a_world_without_subscriptions_publishes_nothing() {
        assert_eq!(publish_subscribed(&World::new()), 0);
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

        assert_eq!(publish_subscribed(&world), 1);
        assert_eq!(publish_subscribed(&world), 1);
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

        publish_subscribed(&world);
        publish_subscribed(&world);
        assert_eq!(*reader.latest().expect("the first snapshot"), 1);
    }

    /// Dropping the reader prunes the subscription on the next tick.
    #[test]
    fn dropping_the_reader_prunes_the_subscription() {
        let mut world = World::new();
        let reader = subscribe_in(&mut world, |_| Some(1));
        assert_eq!(publish_subscribed(&world), 1);
        drop(reader);
        assert_eq!(publish_subscribed(&world), 0);
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

        publish_subscribed(&world);
        let first = reader.latest().expect("the first snapshot");
        assert_eq!(*first, 1);

        publish_subscribed(&world);
        let second = reader.latest().expect("the second snapshot");
        assert_eq!(*second, 2);
        assert_eq!(*first, 1, "the held snapshot did not change under it");
    }
}
