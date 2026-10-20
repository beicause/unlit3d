//! Ordering bookkeeping for the frame: which passes record, and in what order
//! the sources inside one pass record.
//!
//! Both levels are the same problem — a set of entities each carrying an order
//! component — so both use the same machinery here. An order states where its
//! owner belongs relative to its peers; equal orders fall back to entity order
//! and are reported as ambiguous, because which of the two draws first is then
//! not something the world said.
//!
//! This is crate-internal on purpose: a caller's vocabulary is
//! [`FramePassOrder`](crate::pass::FramePassOrder) and
//! [`FrameSourceOrder`](crate::source::FrameSourceOrder), and there is nothing
//! to gain from exposing the generic form they are built on.

use core::fmt::Debug;

use unlit_ecs::Entity;

/// One order value, as an ordered entity states it.
pub(crate) trait OrderKey: Copy + Ord + Debug {
    /// The order type's name, as the ambiguity warning spells it.
    const ORDER: &'static str;
    /// What the ordered entities are, as the ambiguity warning spells it.
    const SUBJECT: &'static str;
    /// The order's value, as the ambiguity warning prints it.
    fn value(self) -> i32;
}

/// One group of entities that declared the same order.
pub(crate) type AmbiguousGroup<K> = (K, Vec<Entity>);

/// Entities in record order, and the ambiguous groups among them.
///
/// Resolved once per frame, so it is kept as a reusable buffer rather than
/// handed back as fresh `Vec`s: a steady scene then resolves its order without
/// allocating. [`Ordering::resolve`] clears and refills every buffer in place,
/// so the capacity survives from one frame to the next.
#[derive(Debug)]
pub(crate) struct Ordering<K: OrderKey> {
    /// `(order, entity)` for every entity, sorted by both. Scratch: the
    /// entities are copied into [`Ordering::order`] once the run boundaries
    /// have been found.
    pub(crate) keys: Vec<(K, Entity)>,
    /// The entities to record, in the order they record.
    pub(crate) order: Vec<Entity>,
    /// The runs of equal orders among them, in order.
    pub(crate) ambiguous: Vec<AmbiguousGroup<K>>,
}

impl<K: OrderKey> Default for Ordering<K> {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            order: Vec::new(),
            ambiguous: Vec::new(),
        }
    }
}

impl<K: OrderKey> Ordering<K> {
    /// Resolve `keys`, reusing whatever this already holds.
    ///
    /// Sorted by `(order, entity)`, so the entity breaks a tie between equal
    /// orders; every run of more than one equal order is an ambiguity.
    pub(crate) fn resolve(&mut self, keys: impl IntoIterator<Item = (K, Entity)>) {
        self.keys.clear();
        self.keys.extend(keys);
        self.keys
            .sort_unstable_by_key(|(order, entity)| (*order, *entity));

        self.order.clear();
        self.ambiguous.clear();
        let mut start = 0;
        while start < self.keys.len() {
            let order = self.keys[start].0;
            let mut end = start + 1;
            while end < self.keys.len() && self.keys[end].0 == order {
                end += 1;
            }
            if end - start > 1 {
                // A run of equal orders: an ambiguity. Its entities are in
                // entity order already, because that is what the sort used as
                // the tie-break.
                let group = self.keys[start..end]
                    .iter()
                    .map(|(_, entity)| *entity)
                    .collect();
                self.ambiguous.push((order, group));
            }
            start = end;
        }
        self.order
            .extend(self.keys.iter().map(|(_, entity)| *entity));
    }

    /// The entities to record, in the order they record.
    pub(crate) fn entities(&self) -> &[Entity] {
        &self.order
    }

    /// The groups of entities that declared the same order, in order.
    ///
    /// Equal orders are not an error — two entities that really are
    /// interchangeable may share one — but they are worth reporting; see
    /// [`OrderWarnings`].
    pub(crate) fn ambiguous(&self) -> &[AmbiguousGroup<K>] {
        &self.ambiguous
    }
}

/// Reports entities that declared the same order, without repeating itself.
///
/// Equal orders are not an error — two entities that really are
/// interchangeable may share one — so this warns rather than panicking. What it
/// must not do is warn on every frame: a steady scene would flood the log. It
/// therefore remembers the groups it last reported and speaks up again only
/// when they change, which is exactly when entities were added, removed or
/// reordered.
#[derive(Debug)]
pub(crate) struct OrderWarnings<K: OrderKey> {
    reported: Vec<AmbiguousGroup<K>>,
}

impl<K: OrderKey> Default for OrderWarnings<K> {
    fn default() -> Self {
        Self {
            reported: Vec::new(),
        }
    }
}

impl<K: OrderKey> OrderWarnings<K> {
    /// Warn about `ambiguous` unless it is what was already reported.
    ///
    /// Returns whether anything was reported, which is what the tests assert
    /// on; the log line names the group's size, its order and its entities.
    pub(crate) fn check(&mut self, ambiguous: &[AmbiguousGroup<K>]) -> bool {
        if self.reported == ambiguous {
            return false;
        }
        for (order, group) in ambiguous {
            log::warn!(
                "{} {} declared the same {}({}): entities {:?}; recording them in an unspecified order — give them distinct orders to choose explicitly",
                group.len(),
                K::SUBJECT,
                K::ORDER,
                order.value(),
                group,
            );
        }
        self.reported = ambiguous.to_vec();
        !ambiguous.is_empty()
    }
}
