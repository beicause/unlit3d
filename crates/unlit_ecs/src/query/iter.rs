//! The iterator a query hands out.

use crate::archetype::{Archetype, ScopeSummary};
use crate::entity::Entity;
#[cfg(debug_assertions)]
use crate::query::assert_no_duplicates;
use crate::query::{ArchetypeFilter, Query};
use crate::world::World;

/// Iterates the entities a [`Query`] matches.
///
/// The per-archetype state is resolved once, before the rows of that archetype
/// are visited, so the components are looked up a handful of times per archetype
/// rather than once per row. An item borrows its component from the world, so two
/// items may be alive at once; asking for one exclusively twice without dropping
/// the first borrow panics, so a row that writes has to be dropped before the
/// next one is fetched.
pub struct QueryIter<'w, Q: Query, F: ArchetypeFilter = ()> {
    world: &'w World,
    /// The scope the query looks through.
    scope: Entity,
    /// Narrows which of the archetypes the query matches are visited.
    filter: F,
    archetype: usize,
    row: usize,
    /// What the archetype being visited resolves its components to.
    /// [`None`] until that archetype is accepted.
    state: Option<Q::Fetch<'w>>,
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
}

impl<'w, Q: Query> QueryIter<'w, Q, ()> {
    pub(crate) fn new(world: &'w World, scope: Entity) -> Self {
        #[cfg(debug_assertions)]
        assert_no_duplicates::<Q>(core::panic::Location::caller());
        Self {
            world,
            scope,
            filter: (),
            archetype: 0,
            row: world.content_start(0),
            state: None,
            resolved: false,
            uniform_visible: None,
        }
    }
}

impl<'w, Q: Query, F: ArchetypeFilter> QueryIter<'w, Q, F> {
    /// Visit only the archetypes the filter accepts.
    ///
    /// The filter sees an archetype before any of its rows is fetched and before
    /// any cell is borrowed, so rejecting an archetype costs one test rather than
    /// one per row, and works even when that archetype's components are already
    /// borrowed elsewhere. See [`ArchetypeFilter`].
    ///
    /// It may be applied part-way through an iteration, and then narrows the
    /// archetype being visited too: what the filter rejects is not fetched, and
    /// the rows already yielded stay yielded.
    #[must_use]
    pub fn by<G: ArchetypeFilter>(self, filter: G) -> QueryIter<'w, Q, G> {
        QueryIter {
            world: self.world,
            scope: self.scope,
            filter,
            archetype: self.archetype,
            row: self.row,
            // The archetype being visited is decided again, since the old
            // decision was made without this filter.
            state: None,
            resolved: false,
            uniform_visible: None,
        }
    }

    /// Whether `archetype` is one this query visits.
    fn archetype_matches(&self, archetype: &Archetype) -> bool {
        Q::matches(archetype)
            && self.filter.accept(archetype)
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

impl<'w, Q: Query, F: ArchetypeFilter> Iterator for QueryIter<'w, Q, F> {
    type Item = Q::Item<'w>;

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
                self.state = None;
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
                self.state = Some(Q::fetch_state(archetype));
                let summary = archetype.scope_summary();
                self.uniform_visible = match summary {
                    // `all_visible` on a uniform archetype is exactly "the
                    // scope its rows live in is visible", the row-invariant
                    // half of `row_visible`.
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
            let state = self.state.as_ref()?;
            return Some(Q::fetch(state, row));
        }
    }
}
