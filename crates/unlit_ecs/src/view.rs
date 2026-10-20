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

use crate::archetype::{Archetype, ScopeSummary};
use crate::bundle::{ArchetypeBuilder, Bundle};
use crate::command::Commands;
use crate::entity::{Entity, Location};
use crate::info::{ArchetypeInfo, EntityInfo, WorldInfo};
#[cfg(debug_assertions)]
use crate::query::assert_no_duplicates;
use crate::query::{Query, QueryIter};
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

    /// Where a visible entity's components live.
    #[must_use]
    pub fn location(&self, entity: Entity) -> Option<Location> {
        if !self.contains(entity) {
            return None;
        }
        self.world.location(entity)
    }

    // -- queries -----------------------------------------------------------

    /// Iterate the visible entities that match `Q`.
    ///
    /// The query is a type, so which components to hand out and which
    /// archetypes to visit are written in the turbofish. See [`crate::query`]
    /// for the items a query is made of.
    ///
    /// ````rust
    /// # use unlit_ecs::{Entity, World, bundle};
    /// let mut world = World::new();
    /// world.root_scope_mut().spawn(bundle!(1u32));
    ///
    /// for (entity, value) in world.root_scope().query::<(Entity, &u32)>() {
    ///     println!("{entity:?} carries {value}");
    /// }
    /// ````
    pub fn query<Q: Query>(&self) -> QueryIter<'w, Q> {
        QueryIter::new(self.world, self.scope)
    }

    /// What `Q` yields for `entity`, when it is visible here and its archetype
    /// matches.
    ///
    /// This is the one-entity form of [`Scope::query`]: the row the entity is
    /// stored at is looked up directly instead of the archetypes being walked.
    /// `None` means the entity is out of this scope's reach or its archetype
    /// does not satisfy the query.
    ///
    /// A lookup by handle does not filter the scaffolding: the root and the
    /// shared scope answer like any other entity, which is what lets the
    /// resources they carry be read. Iterating and counting go through a scope
    /// and leave the pair out; see [`Scope::len`].
    ///
    /// ````rust
    /// # use unlit_ecs::{Has, World, bundle};
    /// let mut world = World::new();
    /// let entity = world.root_scope_mut().spawn(bundle!(1u32));
    ///
    /// assert_eq!(*world.root_scope().query_one::<&u32>(entity).unwrap(), 1);
    /// assert!(world.root_scope().query_one::<Has<u32>>(entity).unwrap());
    /// assert!(!world.root_scope().query_one::<Has<bool>>(entity).unwrap());
    /// assert!(world.root_scope().query_one::<&bool>(entity).is_none());
    /// ````
    #[must_use]
    pub fn query_one<Q: Query>(&self, entity: Entity) -> Option<Q::Item<'w>> {
        #[cfg(debug_assertions)]
        assert_no_duplicates::<Q>(core::panic::Location::caller());
        let location = self.location(entity)?;
        let archetype = self.world.archetype_at(location.archetype())?;
        if !self.row_visible(archetype, location.row()) || !Q::matches(archetype) {
            return None;
        }
        let state = Q::fetch_state(archetype);
        Some(Q::fetch(&state, location.row()))
    }

    /// The archetype with this id, when this scope sees every row of it.
    ///
    /// A partially visible archetype answers `None`: handing out its entity
    /// list would report rows the scope cannot otherwise reach. So does an
    /// empty one, which has no row to see. A caller that only wants the shape
    /// of an archetype uses [`Scope::archetype_infos`], whose length is the
    /// visible one.
    #[must_use]
    pub fn archetype(&self, id: u32) -> Option<&'w Archetype> {
        let archetype = self.world.archetype_at(id)?;
        if archetype.is_empty() || !self.fully_visible(archetype) {
            return None;
        }
        Some(archetype)
    }

    /// Every archetype this scope sees in full, in id order.
    ///
    /// Empty archetypes are skipped: they hold no row, so there is nothing to
    /// iterate. The one archetype holding the tree's scaffolding is included,
    /// since its rows do exist; [`Scope::len`] is what leaves them out of the
    /// count.
    pub fn archetypes(&self) -> impl Iterator<Item = &'w Archetype> {
        self.world
            .archetypes_slice()
            .iter()
            .filter(|archetype| !archetype.is_empty() && self.fully_visible(archetype))
    }

    // -- the tree ----------------------------------------------------------

    /// The scope of a visible scope, or `None` when it is out of reach.
    #[must_use]
    pub fn scope(&self, scope: Entity) -> Option<Scope<'w>> {
        if !self.contains(scope) {
            return None;
        }
        Some(Scope::new(self.world, scope))
    }

    /// The parent of a visible entity.
    #[must_use]
    pub fn parent_scope(&self, entity: Entity) -> Option<Entity> {
        if !self.contains(entity) {
            return None;
        }
        self.world.parent_scope(entity)
    }

    /// The direct children of a visible entity.
    #[must_use]
    pub fn children(&self, entity: Entity) -> Vec<Entity> {
        if !self.contains(entity) {
            return Vec::new();
        }
        self.world.children(entity)
    }

    // -- counting and diagnostics ------------------------------------------

    /// The number of visible entities, the two roots excluded.
    ///
    /// An empty world is empty; the root and the shared scope are scaffolding
    /// and are reported by [`WorldInfo::root`] and [`WorldInfo::shared`]
    /// instead of being counted.
    #[must_use]
    pub fn len(&self) -> usize {
        // The query already skips the scaffolding and applies the visibility
        // rules, so counting this way keeps the number and the iteration in
        // agreement for every scope, the root included.
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
        let mut archetypes = 0;
        for (index, archetype) in self.world.archetypes_slice().iter().enumerate() {
            // Only an archetype with a row this scope can see has a shape to
            // report: an empty one, or one whose rows all live elsewhere,
            // would name components that nothing here carries.
            if self.visible_len(index as u32, archetype) == 0 {
                continue;
            }
            archetypes += 1;
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
            archetypes,
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
            .filter_map(|(index, archetype)| {
                let len = self.visible_len(index as u32, archetype);
                (len > 0).then(|| ArchetypeInfo {
                    index: index as u32,
                    len,
                    components: archetype
                        .types()
                        .iter()
                        .filter_map(|&type_id| self.world.type_name(type_id))
                        .collect(),
                })
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
            let start = self.world.content_start(index as u32);
            for (row, &entity) in archetype.entities()[start..].iter().enumerate() {
                let row = start + row;
                if !self.row_visible(archetype, row) {
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
    fn archetype_visible(&self, archetype: &Archetype) -> bool {
        archetype
            .scope_summary()
            .any_visible(self.world, self.scope)
    }

    /// Whether every row of `archetype` is visible from this scope.
    fn fully_visible(&self, archetype: &Archetype) -> bool {
        archetype
            .scope_summary()
            .all_visible(self.world, self.scope)
    }

    /// Whether row `row` of `archetype` is visible.
    fn row_visible(&self, archetype: &Archetype, row: usize) -> bool {
        archetype
            .scope_summary()
            .row_visible(archetype, self.world, self.scope, row)
    }

    /// The number of rows of `archetype` this scope can see, its own content
    /// only: the scaffolding is never counted.
    fn visible_len(&self, index: u32, archetype: &Archetype) -> usize {
        // The scaffolding shares an archetype with every bare entity, so its
        // rows are removed rather than the whole archetype: a caller's own bare
        // entity may sit in it too. It takes the first rows of that archetype,
        // which is what makes the subtraction a boundary rather than a filter.
        let scaffold = self.world.content_start(index);
        match archetype.scope_summary() {
            ScopeSummary::Empty => 0,
            ScopeSummary::Uniform(_) => {
                if self.fully_visible(archetype) {
                    // The whole archetype is this one scope, so its rows are
                    // the scope's — except a shared row, which reaches the scope
                    // from outside it and so belongs to no other archetype.
                    archetype.len() - scaffold
                } else {
                    // A scope lives in its parent, which is above it, so the
                    // only row of that archetype it can see is its own -- and
                    // the scaffolding's own row is not content.
                    usize::from(
                        archetype.entities()[scaffold..].contains(&self.scope)
                            && !self.world.is_scaffolding(self.scope),
                    )
                }
            }
            ScopeSummary::Mixed => (scaffold..archetype.len())
                .filter(|&row| self.row_visible(archetype, row))
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
        self.spawn(ArchetypeBuilder::new())
    }

    /// Spawn `bundle` on `entity`, a handle from [`World::reserve_entity`].
    ///
    /// Panics when the handle already refers to a live entity.
    pub fn spawn_at<B: Bundle>(&mut self, entity: Entity, bundle: B) {
        self.world.spawn_at_in(entity, self.scope, bundle);
    }

    /// Despawn `entity` and its whole subtree.
    ///
    /// Returns `false` when the entity is not alive, is not owned by this
    /// scope, or is one of the tree's two scaffolding entities. A resource in
    /// the shared subtree is the root's to remove, so only a root scope takes
    /// it; the shared scope itself is scaffolding and stays.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        if self.world.is_scaffolding(entity) {
            return false;
        }
        if !self.world.owns(self.scope, entity) {
            return false;
        }
        self.world.despawn_in(entity)
    }

    /// Make `child` a child of `parent`.
    ///
    /// Both must be owned by this scope, neither may be a root or the shared
    /// scope, and `parent` may not be inside `child`'s subtree. An entity in
    /// the shared subtree is the root's to move, so only a root scope relinks
    /// it.
    pub fn link(&mut self, child: Entity, parent: Entity) -> bool {
        if !self.world.owns(self.scope, child) || !self.world.owns(self.scope, parent) {
            return false;
        }
        self.world.link_in(child, parent, self.scope)
    }

    /// Detach `child` from its parent and put it in this scope.
    ///
    /// Returns `false` when the entity is not alive, is not owned by this
    /// scope, or is scaffolding: an entity in the shared subtree moves only for
    /// the root.
    pub fn unlink(&mut self, child: Entity) -> bool {
        if !self.world.owns(self.scope, child) {
            return false;
        }
        self.world.unlink_in(child, self.scope, self.scope)
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
