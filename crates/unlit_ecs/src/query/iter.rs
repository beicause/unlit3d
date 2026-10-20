//! The iterator a query hands out.

use crate::archetype::{Archetype, ScopeSummary};
use crate::entity::Entity;
use crate::query::{Query, Row};
use crate::world::World;

/// Iterates the entities a [`Query`] matches.
///
/// An item borrows the world through its [`Row`], so two items from one
/// iterator may be alive at the same time. Asking for a component exclusively
/// twice without dropping the first borrow panics, so a row that writes has to
/// be dropped before the next one is fetched.
pub struct QueryIter<'w, 'q> {
    world: &'w World,
    /// The scope the query looks through.
    scope: Entity,
    /// What to fetch, and which archetypes to visit.
    query: &'q Query,
    archetype: usize,
    row: usize,
    /// Whether the archetype being visited has been accepted and its visibility
    /// decided, so that the per-row loop repeats neither.
    resolved: bool,
    /// Set while the archetype being visited is uniform: whether the one scope
    /// its rows live in is visible, which is the answer every row shares.
    /// `None` for a mixed archetype, and until the archetype is resolved.
    ///
    /// Resolved here rather than per row because deciding it walks the scope
    /// tree, and a uniform archetype answers the same thing for every row.
    uniform_visible: Option<bool>,
}

impl<'w, 'q> QueryIter<'w, 'q> {
    pub(crate) fn new(world: &'w World, scope: Entity, query: &'q Query) -> Self {
        query.assert_consistent();
        Self {
            world,
            scope,
            query,
            archetype: 0,
            row: 0,
            resolved: false,
            uniform_visible: None,
        }
    }

    /// Whether the archetype at `self.archetype` is one this query visits.
    fn archetype_matches(&self) -> bool {
        let Some(archetype) = self.world.archetypes_slice().get(self.archetype) else {
            return false;
        };
        if !self.query.matches_archetype(archetype) {
            return false;
        }
        archetype
            .scope_summary()
            .any_visible(self.world, self.scope)
    }

    /// Whether row `row` of `archetype` is visible from this query's scope.
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

impl<'w: 'q, 'q> Iterator for QueryIter<'w, 'q> {
    type Item = (Entity, Row<'q>);

    fn next(&mut self) -> Option<Self::Item> {
        let archetypes = self.world.archetypes_slice();
        loop {
            let archetype = archetypes.get(self.archetype)?;
            if self.row >= archetype.len() {
                // Done with this archetype; move to the next and decide it
                // lazily.
                self.archetype += 1;
                self.row = 0;
                self.resolved = false;
                self.uniform_visible = None;
                continue;
            }
            if !self.resolved {
                if !self.archetype_matches() {
                    self.archetype += 1;
                    self.row = 0;
                    continue;
                }
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
            let entity = archetype.entities()[row];
            if self.world.is_scaffolding(entity) {
                // The tree's two roots are addressable but not content; what
                // a caller spawns under the shared one is content.
                continue;
            }
            return Some((entity, Row::new(archetype, self.query, row)));
        }
    }
}
