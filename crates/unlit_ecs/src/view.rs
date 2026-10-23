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
use crate::bundle::Bundle;
use crate::command::Commands;
use crate::entity::{Entity, Location};
use crate::error::{ArchetypeAccessError, EntityAccessError};
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
    ///
    /// Answers [`EntityAccessError::NotAlive`] for a handle that names no live
    /// entity, and [`EntityAccessError::NotVisible`] for one that lives outside
    /// this scope.
    pub fn location(&self, entity: Entity) -> Result<Location, EntityAccessError> {
        let location = self.world.location(entity)?;
        if !self.contains(entity) {
            return Err(EntityAccessError::NotVisible {
                entity,
                scope: self.scope,
            });
        }
        Ok(location)
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
    /// world.root_scope_mut().spawn(bundle![1u32]);
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
    /// A lookup that finds nothing says why: the entity is not alive, out of
    /// this scope's reach, or its row does not satisfy the query. The last one
    /// covers two cases a caller answers the same way, so it is one error: the
    /// archetype does not match `Q`, or the row is in the shared subtree, which
    /// a non-root scope reads through [`Scope::query`] but not handle by handle.
    ///
    /// A lookup by handle does not filter the scaffolding: the root and the
    /// shared scope answer like any other entity, which is what lets the
    /// resources they carry be read. Iterating and counting go through a scope
    /// and leave the pair out; see [`Scope::len`].
    ///
    /// ````rust
    /// # use unlit_ecs::{Has, World, bundle};
    /// let mut world = World::new();
    /// let entity = world.root_scope_mut().spawn(bundle![1u32]);
    ///
    /// assert_eq!(*world.root_scope().query_one::<&u32>(entity).unwrap(), 1);
    /// assert!(world.root_scope().query_one::<Has<u32>>(entity).unwrap());
    /// assert!(!world.root_scope().query_one::<Has<bool>>(entity).unwrap());
    /// assert!(world.root_scope().query_one::<&bool>(entity).is_err());
    /// ````
    pub fn query_one<Q: Query>(&self, entity: Entity) -> Result<Q::Item<'w>, EntityAccessError> {
        #[cfg(debug_assertions)]
        assert_no_duplicates::<Q>(core::panic::Location::caller());
        let location = self.location(entity)?;
        let archetype = self.world.archetypes().get(location.archetype())?;
        if !self.row_visible(archetype, location.row()) || !Q::matches(archetype) {
            return Err(EntityAccessError::NotMatched { entity });
        }
        let state = Q::fetch_state(archetype);
        Ok(Q::fetch(&state, location.row()))
    }

    /// The archetype with this id, when this scope sees every row of it.
    ///
    /// A partially visible archetype is turned down: handing out its entity
    /// list would report rows the scope cannot otherwise reach. So is an empty
    /// one, which has no row to see. A caller that only wants the shape of an
    /// archetype uses [`Scope::archetype_infos`], whose length is the visible
    /// one; a caller that has an entity and wants the archetype its row lives
    /// in uses [`Scope::archetype_of`], which asks about the row rather than
    /// about the whole archetype.
    pub fn archetype(&self, id: u32) -> Result<&'w Archetype, ArchetypeAccessError> {
        let archetype = self.world.archetypes().get(id)?;
        if archetype.is_empty() {
            return Err(ArchetypeAccessError::Empty { id });
        }
        if !self.fully_visible(archetype) {
            return Err(ArchetypeAccessError::NotVisible {
                id,
                scope: self.scope,
            });
        }
        Ok(archetype)
    }

    /// The archetype an entity's row lives in, when the entity is visible here.
    ///
    /// This is the counterpart of [`Scope::location`]: it answers for one
    /// visible row, so it also answers for an archetype [`Scope::archetype`]
    /// turns down. Two scopes whose entities share a component set share the
    /// archetype and each sees only its own rows of it; asking about one row is
    /// well defined, while handing out the archetype as a whole is not.
    ///
    /// The archetype is the storage, not a view of it: [`Archetype::entities`]
    /// lists the rows of every scope, the ones this scope cannot see included.
    /// [`Scope::entity_info`] and [`Scope::entity_infos`] are what filter by
    /// visibility.
    pub fn archetype_of(&self, entity: Entity) -> Result<&'w Archetype, EntityAccessError> {
        let location = self.location(entity)?;
        self.world
            .archetypes()
            .get(location.archetype())
            .map_err(EntityAccessError::from)
    }

    /// Every archetype this scope sees in full, in id order.
    ///
    /// Empty archetypes are skipped: they hold no row, so there is nothing to
    /// iterate. The one archetype holding the tree's scaffolding is included,
    /// since its rows do exist; [`Scope::len`] is what leaves them out of the
    /// count.
    pub fn archetypes(&self) -> impl Iterator<Item = &'w Archetype> {
        self.world
            .archetypes()
            .as_slice()
            .iter()
            .filter(|archetype| !archetype.is_empty() && self.fully_visible(archetype))
    }

    // -- the tree ----------------------------------------------------------

    /// The scope of a visible scope.
    ///
    /// A scope is an entity like any other, so this is turned down the same way
    /// [`Scope::location`] is.
    pub fn scope(&self, scope: Entity) -> Result<Scope<'w>, EntityAccessError> {
        self.location(scope)?;
        Ok(Scope::new(self.world, scope))
    }

    /// The parent of a visible entity.
    pub fn parent_scope(&self, entity: Entity) -> Result<Entity, EntityAccessError> {
        self.location(entity)?;
        self.world.parent_scope(entity)
    }

    /// The direct children of a visible entity.
    ///
    /// Answers [`EntityAccessError::NotAlive`] or
    /// [`EntityAccessError::NotVisible`] rather than an empty list when the
    /// entity is not there to ask about.
    pub fn children(&self, entity: Entity) -> Result<Vec<Entity>, EntityAccessError> {
        self.location(entity)?;
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
        for (index, archetype) in self.world.archetypes().as_slice().iter().enumerate() {
            // Only an archetype with a row this scope can see has a shape to
            // report: an empty one, or one whose rows all live elsewhere,
            // would name components that nothing here carries.
            if self.visible_len(index as u32, archetype) == 0 {
                continue;
            }
            archetypes += 1;
            components.extend_from_slice(archetype.names());
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
            .archetypes()
            .as_slice()
            .iter()
            .enumerate()
            .filter_map(|(index, archetype)| {
                let len = self.visible_len(index as u32, archetype);
                (len > 0).then(|| ArchetypeInfo {
                    index: index as u32,
                    len,
                    components: archetype.names().to_vec(),
                })
            })
            .collect()
    }

    /// What a visible entity is called, read from the row that stores it.
    ///
    /// A handle rebuilt from bare bits carries no name of its own; the row does.
    /// See [`World::entity_name`].
    pub fn entity_name(&self, entity: Entity) -> Result<&'static str, EntityAccessError> {
        self.location(entity)?;
        self.world.entity_name(entity)
    }

    /// A visible entity's location and component names.
    ///
    /// The handle reported, and the name beside it, both come from the row that
    /// stores the entity, so a handle that arrived as bare bits is answered with
    /// the label its spawn site gave it.
    pub fn entity_info(&self, entity: Entity) -> Result<EntityInfo, EntityAccessError> {
        let location = self.location(entity)?;
        let archetype = self.world.archetypes().get(location.archetype())?;
        let stored = *archetype
            .entities()
            .get(location.row())
            .expect("a live entity's location names a row of its archetype");
        Ok(EntityInfo {
            entity: stored,
            name: stored.name(),
            archetype: location.archetype(),
            row: location.row(),
            components: archetype.names().to_vec(),
        })
    }

    /// At most `limit` visible entities' information, in storage order.
    #[must_use]
    pub fn entity_infos(&self, limit: usize) -> Vec<EntityInfo> {
        let mut infos = Vec::new();
        for (index, archetype) in self.world.archetypes().as_slice().iter().enumerate() {
            if !self.archetype_visible(archetype) {
                continue;
            }
            let components: Vec<&'static str> = archetype.names().to_vec();
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
                    name: entity.name(),
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
    ///
    /// The entity is named after the source position of the `bundle!` that
    /// built `bundle`, which is what its `Debug` prints. A builder put together
    /// by hand carries no source position, so an entity spawned from one has no
    /// name; build it with [`ArchetypeBuilder::named`], or spawn it with
    /// [`ScopeMut::spawn_named`].
    ///
    /// [`ArchetypeBuilder::named`]: crate::ArchetypeBuilder::named
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        self.world.spawn_in(self.scope, bundle)
    }

    /// Spawn `bundle` in this scope under `name`, whatever the bundle was
    /// built from.
    ///
    /// [`ScopeMut::spawn`] names the entity after the `bundle!` that built it,
    /// so this is for the objects whose name has to say more than a source
    /// position does.
    pub fn spawn_named<B: Bundle>(&mut self, name: &'static str, bundle: B) -> Entity {
        self.world.spawn_named_in(self.scope, name, bundle)
    }

    /// Spawn `bundle` on `entity`, a handle from [`World::reserve_entity`].
    ///
    /// Answers [`EntityAccessError::NotReserved`] when the handle is not a live
    /// reservation: it has already been spawned on, was released, or was never
    /// reserved to begin with.
    pub fn spawn_at<B: Bundle>(
        &mut self,
        entity: Entity,
        bundle: B,
    ) -> Result<(), EntityAccessError> {
        self.world.spawn_at_in(entity, self.scope, bundle)
    }

    /// Despawn `entity` and its whole subtree.
    ///
    /// Children are removed before the parent they lived in, so a child's
    /// despawn callback still sees living ancestors.
    ///
    /// Answers [`EntityAccessError::NotAlive`] for a handle that names no live
    /// entity, `NotOwned` for one outside this scope's subtree, `NotMovable`
    /// for one the shared subtree only lends this scope - the root is the
    /// scope that may remove it - and `Scaffolding` for the root and the
    /// shared scope themselves.
    pub fn despawn(&mut self, entity: Entity) -> Result<(), EntityAccessError> {
        self.world.despawn_in(entity, self.scope)
    }

    /// Make `child` a child of `parent`.
    ///
    /// Both must be owned by this scope, neither may be a root or the shared
    /// scope, and `parent` may not be inside `child`'s subtree. An entity in
    /// the shared subtree is the root's to move, so only a root scope relinks
    /// it.
    ///
    /// The same errors as [`ScopeMut::despawn`], plus
    /// [`EntityAccessError::WouldCycle`] when `parent` is `child` itself or
    /// sits inside its subtree: that move would cut the tree in two.
    pub fn link(&mut self, child: Entity, parent: Entity) -> Result<(), EntityAccessError> {
        self.world.link_in(child, parent, self.scope)
    }

    /// Detach `child` from its parent and put it in this scope.
    ///
    /// An entity already in this scope has nowhere to move to, and that is
    /// answered with `Ok(())` rather than an error: what the caller asked for
    /// is already true.
    ///
    /// The same errors as [`ScopeMut::despawn`], plus
    /// [`EntityAccessError::WouldCycle`] when `child` is this scope itself,
    /// which would make it its own parent.
    pub fn unlink(&mut self, child: Entity) -> Result<(), EntityAccessError> {
        self.world.unlink_in(child, self.scope, self.scope)
    }

    /// The mutable scope of a visible scope.
    pub fn scope_mut(&mut self, scope: Entity) -> Result<ScopeMut<'_>, EntityAccessError> {
        self.as_scope().location(scope)?;
        Ok(ScopeMut::new(self.world, scope))
    }

    /// Apply every queued command.
    ///
    /// See [`World::apply`] for what a failing command is answered with.
    pub fn apply(&mut self) -> Result<(), EntityAccessError> {
        self.world.apply()
    }
}
