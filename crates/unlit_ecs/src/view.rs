//! The scope accessors: the read and write entry points of a world.
//!
//! A scope names the entity it is rooted at. It sees that entity, everything
//! below it, and the shared subtree; it does not see its ancestors or its
//! siblings. There is no way up: a scope cannot name the world it came from, so
//! holding one is not a way to reach the rest.
//!
//! Reading and writing components needs only [`Scope`]. Structural
//! changes -- spawning, despawning, relinking -- need [`ScopeMut`], whose
//! operations are confined to the scope's own subtree.

use crate::archetype::Archetype;
use crate::bundle::Bundle;
use crate::column::{CellRef, CellRefMut};
use crate::command::Commands;
use crate::entity::{Entity, Location};
use crate::info::{ArchetypeInfo, EntityInfo, WorldInfo};
use crate::query::{Query, QueryFilter, QueryIter};
use crate::scope::ScopeOf;
use crate::world::World;

/// A read-only scope: one entity and everything below it.
#[derive(Clone, Copy)]
pub struct Scope<'w> {
    pub(crate) world: &'w World,
    pub(crate) scope: Entity,
}

impl core::fmt::Debug for Scope<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Only the scope is printed: dumping every visible entity would make
        // one assertion message page through the whole subtree.
        f.debug_struct("Scope")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl<'w> Scope<'w> {
    pub(crate) fn new(world: &'w World, scope: Entity) -> Self {
        Self { world, scope }
    }

    /// The entity this scope is rooted at.
    #[must_use]
    pub fn entity(&self) -> Entity {
        self.scope
    }

    /// Whether `entity` is visible from this scope.
    ///
    /// This is the addressability test: the two roots answer `true` so that
    /// their components can be read and written like any other entity's, even
    /// though counting and querying omit them (see [`Scope::len`]).
    #[must_use]
    pub fn contains(&self, entity: Entity) -> bool {
        self.world.visible(self.scope, entity)
    }

    /// Whether `entity` is content rather than the tree's own scaffolding.
    ///
    /// The root and the shared scope are entities so that they can carry
    /// components and be named as scopes, but they are not what a scope is
    /// looking at: counting them would put a constant two on every report and
    /// make an empty world non-empty. Their subtrees are content, though: a
    /// resource spawned under the shared scope is visible to every scope and
    /// belongs in what a scope counts.
    #[must_use]
    pub fn is_content(&self, entity: Entity) -> bool {
        !self.world.is_scaffolding(entity)
    }

    /// Whether `entity` is visible and carries component `C`.
    #[must_use]
    pub fn has<C: 'static>(&self, entity: Entity) -> bool {
        self.get::<C>(entity).is_some()
    }

    /// The component of `entity`, or `None` when it is missing or invisible.
    ///
    /// Panics when the component is already exclusively borrowed.
    #[must_use]
    #[inline]
    pub fn get<C: 'static>(&self, entity: Entity) -> Option<CellRef<'w, C>> {
        self.contains(entity).then(|| self.world.get::<C>(entity))?
    }

    /// The component of `entity`, exclusively.
    ///
    /// Panics when the component is already borrowed, shared or exclusive.
    #[must_use]
    #[inline]
    pub fn get_mut<C: 'static>(&self, entity: Entity) -> Option<CellRefMut<'w, C>> {
        self.contains(entity)
            .then(|| self.world.get_mut::<C>(entity))?
    }

    /// Run `f` on the component of `entity`.
    ///
    /// This is the scoped form of [`Scope::get_mut`]: the borrow ends
    /// with the closure, so nothing else can trip over it. It is also how a
    /// behaviour component is driven: the closure is the behaviour, and the
    /// caller decides which entities to run it on.
    #[must_use]
    pub fn with_mut<C: 'static, R>(
        &self,
        entity: Entity,
        f: impl FnOnce(&mut C) -> R,
    ) -> Option<R> {
        let mut guard = self.get_mut::<C>(entity)?;
        Some(f(&mut guard))
    }

    /// Where a visible entity's components live.
    #[must_use]
    pub fn location(&self, entity: Entity) -> Option<Location> {
        self.contains(entity).then(|| self.world.location(entity))?
    }

    // -- queries -----------------------------------------------------------

    /// Iterate the visible entities matching `Q`.
    pub fn query<Q: Query>(&self) -> QueryIter<'w, Q> {
        QueryIter::new(self.world, self.scope)
    }

    /// Iterate the visible entities matching `Q` that pass the filter `F`.
    pub fn query_filtered<Q: Query, F: QueryFilter>(&self) -> QueryIter<'w, Q, F> {
        QueryIter::new(self.world, self.scope)
    }

    /// Run `f` for every visible entity matching `Q`.
    ///
    /// Each item is fetched and dropped before the next one, so a query may ask
    /// for `&mut` components.
    pub fn for_each<Q: Query, F>(&self, f: F)
    where
        F: FnMut(Q::Item<'_>),
    {
        self.for_each_filtered::<Q, (), F>(f);
    }

    /// Run `f` for every visible entity matching `Q` that passes `T`.
    pub fn for_each_filtered<Q: Query, T: QueryFilter, F>(&self, mut f: F)
    where
        F: FnMut(Q::Item<'_>),
    {
        for archetype in self.world.archetypes_slice() {
            if !Q::matches(archetype) || !T::matches(archetype) {
                continue;
            }
            if !self.archetype_visible(archetype) {
                continue;
            }
            let state = Q::fetch_state(archetype);
            for row in 0..archetype.len() {
                if !self.row_visible(archetype, row) {
                    continue;
                }
                if !self.is_content(archetype.entities()[row]) {
                    continue;
                }
                f(Q::fetch(&state, row));
            }
        }
    }

    /// The archetype with this id, when this scope sees every row of it.
    ///
    /// A partially visible archetype answers `None`: handing out its entity
    /// list would report rows the scope cannot otherwise reach. A caller that
    /// only wants the shape of an archetype uses [`Scope::archetype_infos`],
    /// whose length is the visible one.
    #[must_use]
    pub fn archetype(&self, id: u32) -> Option<&'w Archetype> {
        let archetype = self.world.archetype_at(id)?;
        self.fully_visible(archetype).then_some(archetype)
    }

    /// Every archetype this scope sees in full, in id order.
    pub fn archetypes(&self) -> impl Iterator<Item = &'w Archetype> {
        self.world
            .archetypes_slice()
            .iter()
            .filter(|archetype| self.fully_visible(archetype))
    }

    /// The number of archetypes this scope sees in full.
    #[must_use]
    pub fn archetype_count(&self) -> usize {
        self.archetypes().count()
    }

    // -- the tree ----------------------------------------------------------

    /// The scope of a visible scope, or `None` when it is out of reach.
    #[must_use]
    pub fn scope(&self, scope: Entity) -> Option<Scope<'w>> {
        self.contains(scope).then(|| Scope::new(self.world, scope))
    }

    /// The parent of a visible entity.
    #[must_use]
    pub fn parent_scope(&self, entity: Entity) -> Option<Entity> {
        self.contains(entity)
            .then(|| self.world.parent_scope(entity))?
    }

    /// The direct children of a visible entity.
    #[must_use]
    pub fn children(&self, entity: Entity) -> Vec<Entity> {
        if self.contains(entity) {
            self.world.children(entity)
        } else {
            Vec::new()
        }
    }

    // -- counting and diagnostics ------------------------------------------

    /// The number of visible entities, the two roots excluded.
    ///
    /// An empty world is empty; the root and the shared scope are scaffolding
    /// and are reported by [`WorldInfo::root`] and [`WorldInfo::shared`]
    /// instead of being counted.
    #[must_use]
    pub fn len(&self) -> usize {
        if self.world.is_root(self.scope) {
            // The root sees every row, so the entity table already knows the
            // answer and the two scaffolding entities are all that differ.
            return self.world.live_count().saturating_sub(2);
        }
        self.query::<Entity>().count()
    }

    /// Whether nothing is visible.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The scope's shape: visible counts, the component inventory, and the two
    /// roots.
    #[must_use]
    pub fn info(&self) -> WorldInfo {
        let mut components: Vec<&'static str> = Vec::new();
        for archetype in self.world.archetypes_slice() {
            if !self.archetype_visible(archetype) {
                continue;
            }
            for &type_id in archetype.types() {
                if let Some(name) = self.world.type_name(type_id) {
                    components.push(name);
                }
            }
        }
        components.sort_unstable();
        components.dedup();
        WorldInfo {
            entities: self.len(),
            archetypes: self
                .archetype_infos()
                .iter()
                .filter(|info| info.len > 0)
                .count(),
            components,
            root: self.world.root(),
            shared: self.world.shared(),
        }
    }

    /// Every archetype with at least one visible row, in id order.
    #[must_use]
    pub fn archetype_infos(&self) -> Vec<ArchetypeInfo> {
        self.world
            .archetypes_slice()
            .iter()
            .enumerate()
            .map(|(index, archetype)| ArchetypeInfo {
                index: index as u32,
                len: self.visible_len(index as u32, archetype),
                components: archetype
                    .types()
                    .iter()
                    .filter_map(|&type_id| self.world.type_name(type_id))
                    .collect(),
            })
            .collect()
    }

    /// A visible entity's location and component names.
    #[must_use]
    pub fn entity_info(&self, entity: Entity) -> Option<EntityInfo> {
        let location = self.location(entity)?;
        let archetype = self.world.archetype_at(location.archetype())?;
        Some(EntityInfo {
            entity,
            archetype: location.archetype(),
            row: location.row(),
            components: archetype
                .types()
                .iter()
                .filter_map(|&type_id| self.world.type_name(type_id))
                .collect(),
        })
    }

    /// At most `limit` visible entities' information, in storage order.
    #[must_use]
    pub fn entity_infos(&self, limit: usize) -> Vec<EntityInfo> {
        let mut infos = Vec::new();
        for (index, archetype) in self.world.archetypes_slice().iter().enumerate() {
            if !self.archetype_visible(archetype) {
                continue;
            }
            let components: Vec<&'static str> = archetype
                .types()
                .iter()
                .filter_map(|&type_id| self.world.type_name(type_id))
                .collect();
            for (row, &entity) in archetype.entities().iter().enumerate() {
                if !self.row_visible(archetype, row) || !self.is_content(entity) {
                    continue;
                }
                if infos.len() == limit {
                    return infos;
                }
                infos.push(EntityInfo {
                    entity,
                    archetype: index as u32,
                    row,
                    components: components.clone(),
                });
            }
        }
        infos
    }

    // -- deferred structural changes ---------------------------------------

    /// A queue of structural changes, all of them in this scope.
    ///
    /// Takes the scope by value, so the queue outlives it: the scope is a copy of
    /// a reference, and a queue that borrowed the temporary would be unusable
    /// in the position it is most wanted.
    #[must_use]
    pub fn queue(self) -> Commands<'w> {
        Commands::new(self.world, self.scope)
    }

    // -- visibility --------------------------------------------------------

    /// Whether an archetype has any row this scope can see.
    ///
    /// A `Uniform(scope)` archetype is either entirely visible or has exactly
    /// one visible row: the scope's own entity, which lives in `scope` because
    /// a scope is an entity and its parent is where it lives. Both cases are
    /// decided here so that the row loop stays free of tree walks.
    fn archetype_visible(&self, archetype: &Archetype) -> bool {
        match archetype.scope_summary() {
            crate::archetype::ScopeSummary::Empty => false,
            crate::archetype::ScopeSummary::Uniform(scope) => {
                self.world.visible(self.scope, scope)
                    || self.world.parent_scope(self.scope) == Some(scope)
            }
            crate::archetype::ScopeSummary::Mixed => true,
        }
    }

    /// Whether every row of `archetype` is visible from this scope.
    fn fully_visible(&self, archetype: &Archetype) -> bool {
        match archetype.scope_summary() {
            crate::archetype::ScopeSummary::Empty => true,
            crate::archetype::ScopeSummary::Uniform(scope) => self.world.visible(self.scope, scope),
            crate::archetype::ScopeSummary::Mixed => false,
        }
    }

    /// Whether row `row` of `archetype` is visible.
    fn row_visible(&self, archetype: &Archetype, row: usize) -> bool {
        match archetype.scope_summary() {
            crate::archetype::ScopeSummary::Empty => false,
            crate::archetype::ScopeSummary::Uniform(scope) => {
                self.world.visible(self.scope, scope) || archetype.entities()[row] == self.scope
            }
            crate::archetype::ScopeSummary::Mixed => {
                // The scope's own entity lives in its parent's scope, so its
                // row is visible even though that scope is above the scope.
                archetype.entities()[row] == self.scope
                    || archetype
                        .cell::<ScopeOf>(row)
                        .and_then(|cell| cell.try_borrow().ok().map(|scope| scope.0))
                        .is_some_and(|scope| self.world.visible(self.scope, scope))
            }
        }
    }

    /// The number of rows of `archetype` this scope can see, its own content
    /// only: the scaffolding is never counted.
    fn visible_len(&self, index: u32, archetype: &Archetype) -> usize {
        // The scaffolding shares an archetype with every ScopeOf-only entity,
        // so its rows are removed by identity rather than by skipping the
        // whole archetype: a caller's own bare entity may sit in it too.
        let scaffold = if self.world.is_scaffolding_archetype(index) {
            archetype
                .entities()
                .iter()
                .filter(|entity| self.world.is_scaffolding(**entity))
                .count()
        } else {
            0
        };
        match archetype.scope_summary() {
            crate::archetype::ScopeSummary::Empty => 0,
            crate::archetype::ScopeSummary::Uniform(scope) => {
                if self.world.visible(self.scope, scope) {
                    // The whole archetype is this one scope, so its rows are
                    // the scope's — except a shared row, which reaches the scope
                    // from outside it and so belongs to no other archetype.
                    archetype.len() - scaffold
                } else {
                    // A scope lives in its parent, which is above it, so the
                    // only row of that archetype it can see is its own.
                    archetype
                        .entities()
                        .iter()
                        .filter(|entity| **entity == self.scope)
                        .count()
                }
            }
            crate::archetype::ScopeSummary::Mixed => (0..archetype.len())
                .filter(|&row| self.row_visible(archetype, row))
                .filter(|&row| self.is_content(archetype.entities()[row]))
                .count(),
        }
    }
}

/// A mutable scope: one entity and everything below it.
pub struct ScopeMut<'w> {
    world: &'w mut World,
    scope: Entity,
}

impl<'w> ScopeMut<'w> {
    pub(crate) fn new(world: &'w mut World, scope: Entity) -> Self {
        Self { world, scope }
    }

    /// The entity this scope is rooted at.
    #[must_use]
    pub fn entity(&self) -> Entity {
        self.scope
    }

    /// The read-only form of this scope.
    #[must_use]
    pub fn as_scope(&self) -> Scope<'_> {
        Scope::new(self.world, self.scope)
    }

    /// Spawn `bundle` in this scope.
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        self.world.spawn_in(self.scope, bundle)
    }

    /// Spawn an entity with no components in this scope.
    pub fn spawn_empty(&mut self) -> Entity {
        self.spawn(())
    }

    /// Spawn `bundle` on `entity`, a handle from [`World::reserve_entity`].
    ///
    /// Panics when the handle already refers to a live entity.
    pub fn spawn_at<B: Bundle>(&mut self, entity: Entity, bundle: B) {
        self.world.spawn_at_in(entity, self.scope, bundle);
    }

    /// Despawn `entity` and its whole subtree.
    ///
    /// Returns `false` when the entity is not alive or not owned by this scope.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        if self.world.is_root(entity) || self.world.is_shared(entity) {
            return false;
        }
        if !self.world.owns(self.scope, entity) {
            return false;
        }
        self.world.despawn_in(entity)
    }

    /// Make `child` a child of `parent`.
    ///
    /// Both must be owned by this scope, neither may be a root, and `parent`
    /// may not be inside `child`'s subtree.
    pub fn link(&mut self, child: Entity, parent: Entity) -> bool {
        if !self.world.owns(self.scope, child) || !self.world.owns(self.scope, parent) {
            return false;
        }
        self.world.link_in(child, parent)
    }

    /// Detach `child` from its parent and put it in this scope.
    pub fn unlink(&mut self, child: Entity) -> bool {
        if !self.world.owns(self.scope, child) {
            return false;
        }
        self.world.unlink_in(child, self.scope)
    }

    /// The mutable scope of a visible scope.
    #[must_use]
    pub fn scope_mut(&mut self, scope: Entity) -> Option<ScopeMut<'_>> {
        if !self.world.visible(self.scope, scope) {
            return None;
        }
        Some(ScopeMut::new(self.world, scope))
    }

    /// Apply every queued command.
    pub fn apply(&mut self) {
        self.world.apply();
    }
}
