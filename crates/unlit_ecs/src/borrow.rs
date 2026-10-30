//! The ledger of who holds a component.
//!
//! Component values live in `UnsafeCell`s, so the compiler cannot police
//! aliasing between two access paths, and this ledger does. A claim is
//! registered before any reference to a value is handed out and is released when
//! the holder goes away; a claim that overlaps one already held panics, naming
//! both sites. Nothing else checks aliasing at run time.
//!
//! A claim covers either every row of one archetype, which is what a query over a
//! scope asks for, or one row, which is what a single-entity lookup asks for.
//! Two claims conflict when they name the same component type, at least one of
//! them writes, and they can reach the same row: two single-row claims when they
//! name the same entity, anything else when they name the same archetype.
//!
//! The archetype granularity is deliberately conservative. A query cannot tell
//! before iterating which rows of a mixed archetype a scope can see, so it claims
//! the whole archetype; a single-entity claim on a row of that archetype is then
//! refused even when the query would not have reached that row. Refusing a pair
//! that would have been fine is the price of never allowing a pair that is not.

use core::any::TypeId;
use core::panic::Location;

use crate::archetype::Archetype;

use crate::entity::Entity;
use crate::query::{Mode, Query};
use crate::world::World;

/// Which rows a claim covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rows {
    /// Every row of one archetype.
    Archetype(u32),
    /// The one row one entity lives at.
    Entity { archetype: u32, entity: Entity },
}

impl Rows {
    fn archetype(self) -> u32 {
        match self {
            Self::Archetype(archetype) => archetype,
            Self::Entity { archetype, .. } => archetype,
        }
    }

    /// Whether two claims can reach the same row.
    fn overlaps(self, other: Self) -> bool {
        match (self, other) {
            (Self::Entity { entity: left, .. }, Self::Entity { entity: right, .. }) => {
                left == right
            }
            _ => self.archetype() == other.archetype(),
        }
    }
}

/// One registered claim.
struct Lease {
    /// Tells this claim apart from every other one, so releasing is exact.
    id: u64,
    type_id: TypeId,
    name: &'static str,
    mode: Mode,
    rows: Rows,
    /// Where the claim was made, reported when a later claim conflicts with it.
    origin: &'static Location<'static>,
}

impl Lease {
    /// The panic for a claim that overlaps this one.
    fn conflict(&self, mode: Mode, origin: &'static Location<'static>) -> ! {
        let held = self.mode.adverb();
        let wanted = mode.verb();
        panic!(
            "component `{}` is already borrowed {held} at {}, and this {wanted} at {origin} \
             cannot share it",
            self.name, self.origin,
        )
    }
}

/// Every claim currently held on a world's components.
#[derive(Default)]
pub(crate) struct BorrowLedger {
    /// The id the next claim is given.
    next: u64,
    held: Vec<Lease>,
}

impl BorrowLedger {
    /// Register a claim, or panic when it overlaps one already held.
    ///
    /// Answers the claim's id, which the holder passes back to
    /// [`BorrowLedger::release`] when it goes away.
    pub(crate) fn claim(
        &mut self,
        type_id: TypeId,
        name: &'static str,
        mode: Mode,
        rows: Rows,
        origin: &'static Location<'static>,
    ) -> u64 {
        for lease in &self.held {
            if lease.type_id == type_id
                && lease.rows.overlaps(rows)
                && (lease.mode == Mode::Write || mode == Mode::Write)
            {
                lease.conflict(mode, origin);
            }
        }
        let id = self.next;
        self.next += 1;
        self.held.push(Lease {
            id,
            type_id,
            name,
            mode,
            rows,
            origin,
        });
        id
    }

    /// Drop the claim with this id, if it is still held.
    pub(crate) fn release(&mut self, id: u64) {
        if let Some(index) = self.held.iter().position(|lease| lease.id == id) {
            self.held.swap_remove(index);
        }
    }

    /// Drop every claim in `ids`, leaving the vector empty.
    pub(crate) fn release_all(&mut self, ids: &mut Vec<u64>) {
        for id in ids.drain(..) {
            self.release(id);
        }
    }
}

/// The claims one holder owns, released together when it is dropped.
///
/// A holder keeps its claims for its whole life rather than for one pass over
/// the rows: an item it handed out borrows from it, so a claim released between
/// two items would let another holder reach a value an item still points at.
pub(crate) struct Claims<'w> {
    world: &'w World,
    ids: Vec<u64>,
    /// The archetypes already claimed, so a second pass over the same query
    /// takes a second claim on none of them.
    archetypes: Vec<u32>,
}

impl<'w> Claims<'w> {
    pub(crate) fn new(world: &'w World) -> Self {
        Self {
            world,
            ids: Vec::new(),
            archetypes: Vec::new(),
        }
    }

    /// Claim what `Q` reaches on one archetype, unless it was claimed already.
    pub(crate) fn claim_archetype<Q: Query>(
        &mut self,
        index: u32,
        archetype: &Archetype,
        origin: &'static Location<'static>,
    ) {
        claim_archetype::<Q>(
            self.world,
            &mut self.ids,
            &mut self.archetypes,
            index,
            archetype,
            origin,
        );
    }

    /// Claim what `Q` reaches on the row of one entity.
    pub(crate) fn claim_row<Q: Query>(
        &mut self,
        index: u32,
        entity: Entity,
        archetype: &Archetype,
        origin: &'static Location<'static>,
    ) {
        claim_row::<Q>(self.world, &mut self.ids, index, entity, archetype, origin);
    }
}

impl Drop for Claims<'_> {
    fn drop(&mut self) {
        self.world.release(&mut self.ids);
    }
}

/// Claim on `archetype` every component `Q` reaches, shared or exclusive.
///
/// Components the archetype does not carry are skipped: an optional item reaches
/// nothing there, and claiming it anyway would refuse another query for no
/// reason.
pub(crate) fn claim_archetype<Q: Query>(
    world: &World,
    ids: &mut Vec<u64>,
    claimed: &mut Vec<u32>,
    index: u32,
    archetype: &Archetype,
    origin: &'static Location<'static>,
) {
    if claimed.contains(&index) {
        return;
    }
    Q::access(&mut |type_id, name, mode| {
        if archetype.column_index(type_id).is_some() {
            let id = world.claim(type_id, name, mode, Rows::Archetype(index), origin);
            ids.push(id);
        }
    });
    claimed.push(index);
}

/// Claim on one entity's row every component `Q` reaches.
pub(crate) fn claim_row<Q: Query>(
    world: &World,
    ids: &mut Vec<u64>,
    index: u32,
    entity: Entity,
    archetype: &Archetype,
    origin: &'static Location<'static>,
) {
    Q::access(&mut |type_id, name, mode| {
        if archetype.column_index(type_id).is_some() {
            let rows = Rows::Entity {
                archetype: index,
                entity,
            };
            let id = world.claim(type_id, name, mode, rows, origin);
            ids.push(id);
        }
    });
}
