//! The iterator a query hands out.

use crate::archetype::{Archetype, ScopeSummary};
use crate::borrow::Claims;
use crate::entity::Entity;
use crate::query::{ArchetypeFilter, Ask, PlanBuilder, Query};
use crate::world::World;

/// Iterates the entities a [`Query`] matches.
///
/// The plan of the archetype being visited is resolved once, before its rows
/// are read, so the components are looked up a handful of times per archetype
/// rather than once per row. Each read then borrows the plan for one row, which
/// is what keeps the exclusive references of one row from aliasing those of the
/// next.
///
/// The iterator is lent by the query holder that owns the claims, so an item
/// cannot outlive it: the items borrow from the holder as well, which is what
/// lets a caller hold as many as the holder's claims allow and no more.
pub struct QueryIter<'q, 'w, Q: Query> {
    world: &'w World,
    /// The scope the query looks through.
    scope: Entity,
    /// Narrows which of the archetypes the query matches are visited.
    filter: &'q dyn ArchetypeFilter,
    archetype: usize,
    row: usize,
    /// What the archetype being visited resolves its components to.
    /// [`None`] until that archetype is accepted.
    plan: Option<Q::Plan<'w>>,
    /// Set once the archetype being visited has been accepted and its visibility
    /// decided, so that the per-row loop repeats neither.
    resolved: bool,
    /// Set while the archetype being visited is uniform: whether the one scope
    /// its rows live in is visible, which is the answer every row shares.
    /// [`None`] for a mixed archetype, and until the archetype is resolved.
    ///
    /// Resolved here rather than per row because deciding it walks the scope
    /// tree, and a uniform archetype answers the same thing for every row.
    uniform_visible: Option<bool>,
    /// The holder's claims, which the archetypes accepted here are added to.
    claims: &'q mut Claims<'w>,
    /// Where a pass records the components it asks an archetype for. Owned by
    /// the holder, so a second pass over the same world reuses it.
    asks: &'q mut Vec<Ask>,
    /// Where the query was asked for, reported when a claim conflicts with one
    /// already held.
    origin: &'static core::panic::Location<'static>,
}

impl<'q, 'w, Q: Query> QueryIter<'q, 'w, Q> {
    pub(crate) fn new(
        world: &'w World,
        scope: Entity,
        filter: &'q dyn ArchetypeFilter,
        claims: &'q mut Claims<'w>,
        asks: &'q mut Vec<Ask>,
        origin: &'static core::panic::Location<'static>,
    ) -> Self {
        Self {
            world,
            scope,
            filter,
            archetype: 0,
            row: world.content_start(0),
            plan: None,
            resolved: false,
            uniform_visible: None,
            claims,
            asks,
            origin,
        }
    }

    /// Whether `archetype` is one this query may visit.
    ///
    /// This is only the cheap half: whether the query can resolve its plan on
    /// the archetype is decided by `Q::prepare` itself, once, when that
    /// archetype is resolved. Resolving here as well would reach the columns
    /// twice per archetype.
    fn archetype_matches(&self, archetype: &Archetype) -> bool {
        self.filter.accept(archetype)
            && archetype
                .scope_summary()
                .any_visible(self.world, self.scope)
    }

    /// Whether `row` of `archetype` is visible from this query's scope.
    fn row_matches(&self, archetype: &Archetype, row: usize) -> bool {
        match self.uniform_visible {
            // The whole archetype lives in one scope, so the tree walk is done
            // once for it. The one row that can still be visible without it is
            // the caller's own: a scope lives in its parent, and that row sits
            // in the parent's archetype, not in the caller's.
            Some(visible) => visible || archetype.entities()[row] == self.scope,
            None => archetype
                .scope_summary()
                .row_visible(archetype, self.world, self.scope, row),
        }
    }
}

impl<'q, 'w: 'q, Q: Query + 'q> Iterator for QueryIter<'q, 'w, Q> {
    type Item = Q::Item<'q>;

    fn next(&mut self) -> Option<Self::Item> {
        let archetypes = self.world.archetypes().as_slice();
        loop {
            let archetype = archetypes.get(self.archetype)?;
            if self.row >= archetype.len() {
                // Done with this archetype; move to the next and decide it
                // lazily. Its scaffolding, if any, is skipped the same way: by
                // starting the scan past it rather than testing every row.
                self.archetype += 1;
                self.row = self.world.content_start(self.archetype as u32);
                self.plan = None;
                self.resolved = false;
                self.uniform_visible = None;
                continue;
            }
            if !self.resolved {
                if !self.archetype_matches(archetype) {
                    self.archetype += 1;
                    self.row = self.world.content_start(self.archetype as u32);
                    continue;
                }
                // The buffer is the holder's, so every archetype visited after
                // the first reuses its capacity instead of allocating. It is
                // cleared first: the asks of the archetype before this one are
                // not asks on this one.
                let mut asks = core::mem::take(&mut *self.asks);
                asks.clear();
                let mut builder = PlanBuilder::new(archetype, asks);
                let plan = Q::prepare(&mut builder);
                *self.asks = builder.into_asks();
                let Some(plan) = plan else {
                    self.archetype += 1;
                    self.row = self.world.content_start(self.archetype as u32);
                    continue;
                };
                self.claims.claim_archetype(
                    self.archetype as u32,
                    archetype,
                    self.asks,
                    self.origin,
                );
                self.plan = Some(plan);
                let summary = archetype.scope_summary();
                self.uniform_visible = match summary {
                    // [`all_visible`] on a uniform archetype is exactly "the
                    // scope its rows live in is visible", the row-invariant
                    // half of [`row_visible`].
                    ScopeSummary::Uniform(_) => Some(summary.all_visible(self.world, self.scope)),
                    _ => None,
                };
                self.resolved = true;
            }
            let row = self.row;
            self.row += 1;
            if !self.row_matches(archetype, row) {
                continue;
            }
            let plan = self.plan.as_mut()?;
            // SAFETY: the plan lives in this holder and [`next`] hands out one
            // row at a time, so no two items of this iterator name the same cell.
            // [`read`] takes the plan exclusively, so one call yields at most
            // one exclusive reference per column. 'q comes from the holder's own
            // [`&mut self`], so every item dies before the holder does.
            let plan: &'q mut Q::Plan<'w> = unsafe { &mut *(plan as *mut Q::Plan<'w>) };
            return Some(Q::read(plan, row));
        }
    }
}
