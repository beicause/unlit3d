//! The world: entities, their components, and the scopes they live in.
//!
//! Components live in `UnsafeCell`s so that a shared `&World` can reach them,
//! and a borrow ledger written through that same shared reference catches two
//! access paths reaching one value when at least one of them writes. The world
//! is therefore `!Send`: it stays on the thread that created it, which is where
//! a renderer or any other `!Send` state lives.
//!
//! Reading and writing components needs only `&World`. Structural changes --
//! spawning and despawning -- need `&mut World`. A callback that only has a shared world can queue those
//! changes instead with [`crate::Commands`] and apply them later with
//! [`World::apply`].
//!
//! Every entity lives in a scope, and a scope is another entity: the one the
//! world's tree says it lives in. The world therefore holds a tree, and the
//! entry points that read or write it are scopes of a scope rather than
//! methods on the world -- [`Scope`] and [`ScopeMut`]. A scope sees
//! its own subtree and nothing beside it; only `&World` reaches the root.
//!
//! An entity's components are fixed when it is spawned; there is no way to add
//! or remove one later. To change the set of components, despawn the entity
//! and spawn a new one.
//!
//! A borrow conflict is a panic, not a compile error: asking for a component
//! that is already borrowed, or fetching the same component as `&mut` twice
//! in one query, reports the component's name and stops. This is the trade for
//! not doing access analysis. An optional component is optional only about the
//! component being absent; a cell that cannot be borrowed panics as well.

use core::any::TypeId;
use core::cell::{Ref, RefCell, RefMut};
use std::collections::VecDeque;

use crate::archetype::{Archetype, Archetypes};
use crate::borrow::{BorrowLedger, Rows};
use crate::bundle::{Bundle, BundleComponent, DynamicBundle};
use crate::column::AnyColumn;
use crate::command::Command;
use crate::entity::{Entities, Entity, Location};
use crate::error::EntityAccessError;
use crate::hash::TypeIdHashMap;
use crate::lifecycle::{Lifecycle, LifecycleCallback};
use crate::query::Mode;
use crate::scope::ScopeTree;
use crate::view::{Scope, ScopeMut};

/// Entities and their components.
pub struct World {
    /// Where each handle lives, and the allocator behind it.
    ///
    /// Still a cell: [`World::reserve_entity`] hands out a handle through
    /// `&World` so that [`Commands::spawn`](crate::Commands::spawn) can answer one
    /// before the queue is applied, and a reservation is a real allocation, not
    /// a queued intent. Everything else that touches this table runs under
    /// `&mut World`, so the two never overlap.
    entities: RefCell<Entities>,
    archetypes: Archetypes,
    /// Column constructors, one per component type that ever entered the world.
    ctors: TypeIdHashMap<fn() -> Box<dyn AnyColumn>>,
    /// The name of every component type that ever entered the world.
    names: TypeIdHashMap<&'static str>,
    /// Structural changes waiting for the next [`World::apply`].
    ///
    /// A cell for the same reason: a command is queued from a callback that
    /// only holds `&World`.
    commands: RefCell<VecDeque<Box<dyn Command>>>,
    /// Where every live entity lives.
    ///
    /// Not a cell: every write to it happens under `&mut World`, and a reader
    /// borrowing it from `&World` keeps that shared borrow alive, which is what
    /// excludes one. The visibility walk reads it on every row, so the absence of
    /// a runtime flag there is the point.
    tree: ScopeTree,
    /// Who holds which component values.
    ///
    /// A cell because a claim is registered by a query constructed from `&World`
    /// and dropped by its holder's `Drop`, neither of which has the world
    /// mutably.
    borrow: RefCell<BorrowLedger>,
    /// The entity that owns the world, whose scope sees everything.
    root: Entity,
    /// The entity whose subtree every scope sees.
    shared: Entity,
    /// The archetype holding the two scaffolding entities.
    ///
    /// They are the only entities that exist without being content, so a scope
    /// that counts rows has to know where they are. They take the first
    /// [`World::SCAFFOLDING_ROWS`] rows of it.
    scaffolding: u32,
}

/// The exclusive borrow of a world's command queue.
pub(crate) type CommandsGuard<'w> = RefMut<'w, VecDeque<Box<dyn Command>>>;

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// How many rows the two scaffolding entities take.
    ///
    /// [`World::new`] pushes both before any caller can spawn, and a removal only
    /// ever moves the last row of an archetype into the hole it leaves, so the
    /// pair stays at the front for the world's whole life.
    const SCAFFOLDING_ROWS: usize = 2;

    /// A world holding its root and shared scope, and nothing else.
    ///
    /// Both are ordinary entities: they are addressable, carry components and
    /// can be opened as scopes, and they are what a shared resource hangs from.
    /// Neither is counted, queried, despawned or linked; see [`Scope::len`],
    /// which reports the entities a scope sees with the pair left out.
    pub fn new() -> Self {
        let mut world = Self {
            entities: RefCell::new(Entities::default()),
            archetypes: Archetypes::new(),
            ctors: TypeIdHashMap::default(),
            names: TypeIdHashMap::default(),
            commands: RefCell::new(VecDeque::new()),
            tree: ScopeTree::default(),
            borrow: RefCell::new(BorrowLedger::default()),
            // Both are overwritten below, before the world leaves this
            // function and anything can read them.
            root: Entity::from_raw(0, 0),
            shared: Entity::from_raw(0, 0),
            scaffolding: 0,
        };
        // The root's scope is itself, which is where walking up the tree stops.
        // It is recorded before the shared scope is inserted, because that one
        // hangs from it. Neither goes through the spawn path, which reads the
        // root that is being written here.
        let root = world.entities_write().alloc("root");
        world.root = root;
        world.insert_row(root, root, crate::DynamicBundle::new());
        let shared = world.entities_write().alloc("shared");
        world.shared = shared;
        world.insert_row(shared, root, crate::DynamicBundle::new());
        world.scaffolding = world
            .location(root)
            .expect("the root has a row")
            .archetype();
        debug_assert_eq!(
            world.archetypes().as_slice()[world.scaffolding as usize].entities(),
            &[root, shared][..],
            "the scaffolding takes the first rows of its archetype",
        );
        world
    }

    // -- the tree ----------------------------------------------------------

    /// The entity that owns the world, whose scope sees everything.
    #[must_use]
    pub fn root(&self) -> Entity {
        self.root
    }

    /// The entity whose subtree every scope sees.
    #[must_use]
    pub fn shared(&self) -> Entity {
        self.shared
    }

    /// Whether `entity` is in the shared subtree, itself included.
    #[must_use]
    pub fn is_shared(&self, entity: Entity) -> bool {
        entity == self.shared || self.is_descendant(self.shared, entity)
    }

    /// Whether `entity` is one of the tree's two scaffolding entities.
    ///
    /// This is narrower than [`World::is_shared`], which covers the whole
    /// shared subtree: the entities a caller spawns there are content, and a
    /// scope counts and queries them like any other.
    #[must_use]
    pub fn is_scaffolding(&self, entity: Entity) -> bool {
        entity == self.root || entity == self.shared
    }

    /// Whether `entity` is strictly below `ancestor` in the tree.
    #[must_use]
    pub fn is_descendant(&self, ancestor: Entity, entity: Entity) -> bool {
        self.tree.is_descendant(ancestor, entity)
    }

    /// Whether a scope of `caller` sees `candidate`.
    ///
    /// The shared subtree reaches every scope, the root sees everything, and a
    /// scope sees itself and everything below it. A handle that is not alive is
    /// visible from nowhere, the root included: seeing a place is not the same
    /// as there being something in it.
    #[must_use]
    pub fn visible(&self, caller: Entity, candidate: Entity) -> bool {
        if !self.entities_read().contains(candidate) {
            return false;
        }
        // The root is the hot case: it sees everything, so it is answered
        // before the parent-chain walks below.
        caller == self.root
            || candidate == caller
            || self.is_shared(candidate)
            || self.is_descendant(caller, candidate)
    }

    /// Whether a scope of `caller` may despawn or relink `entity`.
    ///
    /// Ownership is stricter than visibility: the shared subtree is readable
    /// from everywhere but only the root may move or remove it.
    #[must_use]
    pub fn owns(&self, caller: Entity, entity: Entity) -> bool {
        self.check_owns(caller, entity).is_ok()
    }

    /// Why a scope of `caller` may not despawn or relink `entity`.
    ///
    /// The same rule as [`World::owns`], answered with the reason instead of
    /// one bit: an entity outside the subtree is another scope's to change, and
    /// one in the shared subtree is the root's alone.
    fn check_owns(&self, caller: Entity, entity: Entity) -> Result<(), EntityAccessError> {
        if caller == self.root {
            return Ok(());
        }
        if self.is_shared(entity) {
            return Err(EntityAccessError::NotMovable {
                entity,
                scope: caller,
            });
        }
        if entity != caller && !self.is_descendant(caller, entity) {
            return Err(EntityAccessError::NotOwned {
                entity,
                scope: caller,
            });
        }
        Ok(())
    }

    /// The parent of a live entity.
    pub fn parent_scope(&self, entity: Entity) -> Result<Entity, EntityAccessError> {
        self.tree.parent(entity)
    }

    /// The direct children of `entity`.
    ///
    /// Answers [`EntityAccessError::NotAlive`] for a handle that names no live
    /// entity, which an empty list would otherwise hide.
    pub fn children(&self, entity: Entity) -> Result<Vec<Entity>, EntityAccessError> {
        self.location(entity)?;
        Ok(self.tree.children(entity).to_vec())
    }

    /// The first row of `archetype` that holds content rather than scaffolding.
    ///
    /// This is where a query starts scanning an archetype, and what a count
    /// leaves out. It is a row boundary rather than a per-row test so that
    /// walking a row costs no identity check: the two roots share their
    /// archetype with every entity spawned bare, which would otherwise need one
    /// comparison per row.
    #[inline]
    #[must_use]
    pub(crate) fn content_start(&self, archetype: u32) -> usize {
        if archetype == self.scaffolding {
            Self::SCAFFOLDING_ROWS
        } else {
            0
        }
    }

    // -- scopes -------------------------------------------------------------

    /// The scope of the root: everything in the world.
    #[must_use]
    pub fn root_scope(&self) -> Scope<'_> {
        Scope::new(self, self.root)
    }

    /// The scope of `scope`.
    pub fn scope(&self, scope: Entity) -> Result<Scope<'_>, EntityAccessError> {
        self.tree.parent(scope)?;
        Ok(Scope::new(self, scope))
    }

    /// The mutable scope of the root: everything in the world.
    #[must_use]
    pub fn root_scope_mut(&mut self) -> ScopeMut<'_> {
        ScopeMut::new(self, self.root)
    }

    /// The mutable scope of `scope`.
    pub fn scope_mut(&mut self, scope: Entity) -> Result<ScopeMut<'_>, EntityAccessError> {
        self.tree.parent(scope)?;
        Ok(ScopeMut::new(self, scope))
    }

    // -- handles -----------------------------------------------------------

    /// A handle to an entity that does not exist yet, named `name`.
    ///
    /// The handle becomes live when a bundle is put on it, either with a
    /// scope's spawn or by applying a queued [`crate::Commands::spawn`].
    /// Reserving is where the caller says what the entity is, so the bundle
    /// put on it later does not rename it.
    pub fn reserve_entity(&self, name: &'static str) -> Entity {
        self.entities_write().reserve(name)
    }

    /// Release a handle from [`World::reserve_entity`] that will never be
    /// spawned.
    ///
    /// A reserved handle that is dropped without ever being spawned keeps its
    /// index out of the pool; releasing it puts the index back, so a later
    /// spawn reuses it. Returns whether `entity` was such a handle.
    pub fn release_entity(&mut self, entity: Entity) -> bool {
        if !self.entities_read().is_reserved(entity) {
            return false;
        }
        self.entities_write().free(entity)
    }

    // -- structural changes ------------------------------------------------

    /// Spawn `bundle` in `scope`.
    ///
    /// The entity is named after the bundle: a type of the caller's own names it
    /// after itself, and a `bundle!` names it after the source position that
    /// wrote the bundle. A dynamic bundle made by hand carries no name, so an
    /// entity spawned from one has none.
    pub(crate) fn spawn_in<B: Bundle>(&mut self, scope: Entity, bundle: B) -> Entity {
        let name = bundle.name();
        self.spawn_bundle_in(scope, name, bundle.into_bundle())
    }

    /// Spawn a bundle under `name`, whatever name the bundle carries.
    pub(crate) fn spawn_named_in<B: Bundle>(
        &mut self,
        scope: Entity,
        name: &'static str,
        bundle: B,
    ) -> Entity {
        self.spawn_bundle_in(scope, name, bundle.into_bundle())
    }

    fn spawn_bundle_in(
        &mut self,
        scope: Entity,
        name: &'static str,
        bundle: DynamicBundle,
    ) -> Entity {
        let entity = self.entities_write().alloc(name);
        self.insert_row(entity, scope, bundle);
        self.finish_spawn(entity);
        entity
    }

    /// Spawn `bundle` on `entity`, a handle from [`World::reserve_entity`], in
    /// `scope`.
    ///
    /// Answers [`EntityAccessError::NotReserved`] when the handle is not a live
    /// reservation from [`World::reserve_entity`]: spawning on a stale or
    /// never-reserved handle would leave a row nobody can reach or free.
    pub(crate) fn spawn_at_in<B: Bundle>(
        &mut self,
        entity: Entity,
        scope: Entity,
        bundle: B,
    ) -> Result<(), EntityAccessError> {
        if !self.entities_write().take_reserved(entity) {
            return Err(EntityAccessError::NotReserved { entity });
        }
        // The handle was named when it was reserved, which is where the caller
        // said what the entity is; the bundle does not rename it.
        self.insert_row(entity, scope, bundle);
        self.finish_spawn(entity);
        Ok(())
    }

    /// Despawn `entity` and its whole subtree, on behalf of `caller`.
    ///
    /// Children are removed before the parent they lived in, so a child's
    /// despawn callback still sees living ancestors. The answer is how many
    /// entities the despawn took with it: the subtree is the scope's to know
    /// about, and a caller that counts what it removed can say so.
    pub(crate) fn despawn_in(
        &mut self,
        entity: Entity,
        caller: Entity,
    ) -> Result<usize, EntityAccessError> {
        if self.is_scaffolding(entity) {
            return Err(EntityAccessError::Scaffolding { entity });
        }
        self.location(entity)?;
        self.check_owns(caller, entity)?;
        let doomed = self.tree.subtree_post_order(entity);
        for victim in &doomed {
            self.remove_entity(*victim);
        }
        Ok(doomed.len())
    }

    /// Make `child` a child of `parent` on behalf of `caller`.
    ///
    /// Neither end may be a root or the shared scope itself, both must be
    /// alive, and `parent` may not be inside `child`'s subtree: moving a scope
    /// into its own descendant would cut the tree in two. The shared subtree is
    /// readable from everywhere but it is the root's to rearrange, so only a
    /// root caller may move an entity into or out of it.
    pub(crate) fn link_in(
        &mut self,
        child: Entity,
        parent: Entity,
        caller: Entity,
    ) -> Result<(), EntityAccessError> {
        if self.is_scaffolding(child) {
            return Err(EntityAccessError::Scaffolding { entity: child });
        }
        if self.is_scaffolding(parent) {
            return Err(EntityAccessError::Scaffolding { entity: parent });
        }
        self.location(child)?;
        self.location(parent)?;
        self.check_owns(caller, child)?;
        self.check_owns(caller, parent)?;
        if child == parent || self.is_descendant(child, parent) {
            return Err(EntityAccessError::WouldCycle { child, parent });
        }
        self.write_scope(child, parent);
        Ok(())
    }

    /// Detach `child` from its parent and put it in `scope`, on behalf of
    /// `caller`.
    ///
    /// `child` may not be the scope itself: that would make it its own parent,
    /// which is how the tree says "this is a root". Scaffolding does not move,
    /// and only a root caller may take an entity out of the shared subtree.
    pub(crate) fn unlink_in(
        &mut self,
        child: Entity,
        scope: Entity,
        caller: Entity,
    ) -> Result<(), EntityAccessError> {
        if self.is_scaffolding(child) {
            return Err(EntityAccessError::Scaffolding { entity: child });
        }
        if child == scope {
            // Its own parent is how the tree says "this is a root".
            return Err(EntityAccessError::WouldCycle {
                child,
                parent: scope,
            });
        }
        self.location(child)?;
        self.location(scope)?;
        self.check_owns(caller, child)?;
        if self.parent_scope(child)? == scope {
            return Ok(());
        }
        self.write_scope(child, scope);
        Ok(())
    }

    /// Move a live entity into `scope` and reindex it.
    fn write_scope(&mut self, entity: Entity, scope: Entity) {
        let location = self
            .location(entity)
            .unwrap_or_else(|_| panic!("{entity:?} has no row to move"));
        let old = self
            .tree
            .parent(entity)
            .unwrap_or_else(|_| panic!("{entity:?} is alive but not in the tree"));
        if old == scope {
            return;
        }
        self.archetypes
            .get_mut(location.archetype())
            .expect("a live entity's location names a registered archetype")
            .move_scope(location.row(), old, scope);
        self.tree_mut().attach(entity, scope);
    }

    // -- the row itself ----------------------------------------------------

    /// Insert a row for `entity` in `scope`, made of the components `bundle`
    /// carries.
    fn insert_row<B: Bundle>(&mut self, entity: Entity, scope: Entity, bundle: B) {
        let components = bundle.into_bundle().finish();
        for component in &components {
            self.ctors
                .entry(component.type_id)
                .or_insert(component.column);
            self.names
                .entry(component.type_id)
                .or_insert(component.name);
        }

        let archetype = self.archetype_for_components(&components);
        let target = self
            .archetypes
            .get_mut(archetype)
            .expect("the archetype was just found or registered");
        for component in components {
            target.push_erased(component.type_id, component.value);
        }
        target.push_entity(entity, scope);
        let row = (target.len() - 1) as u32;
        self.entities_write()
            .set_location(entity, Location { archetype, row });
        self.tree_mut().attach(entity, scope);
    }

    /// Run the despawn callbacks of `entity` and drop its row and handle.
    fn remove_entity(&mut self, entity: Entity) {
        let Ok(location) = self.location(entity) else {
            return;
        };
        self.run_lifecycle::<crate::OnDespawn>(entity);
        self.tree_mut().detach(entity);
        self.remove_row(location);
        self.free_entity(entity);
    }

    /// Run the spawn callbacks of an entity whose row is already in place.
    pub(crate) fn finish_spawn(&mut self, entity: Entity) {
        self.run_lifecycle::<crate::OnSpawn>(entity);
    }

    /// Run the callback of the lifecycle component `C` the entity carries.
    ///
    /// The scope handed to it is the one the entity lives in, that of its
    /// parent: a callback that wants the entity's own subtree descends into the
    /// entity itself.
    fn run_lifecycle<C: 'static + Lifecycle>(&mut self, entity: Entity) {
        let Ok(scope) = self.parent_scope(entity) else {
            return;
        };
        // The callback is moved out of the row for the run, so a callback that
        // reaches for its own lifecycle component reads it instead of finding
        // it held open by its own run.
        let Some(callback) = self.take_lifecycle::<C>(entity) else {
            return;
        };
        let scope = Scope::new(self, scope);
        let mut callback = callback;
        callback(&scope, entity);
        self.restore_lifecycle::<C>(entity, callback);
    }

    /// Move the callback of the lifecycle component `C` out of the entity's row.
    fn take_lifecycle<C: 'static + Lifecycle>(
        &mut self,
        entity: Entity,
    ) -> Option<LifecycleCallback> {
        let location = self.location(entity).ok()?;
        let archetype = self.archetypes.get_mut(location.archetype()).ok()?;
        let column = archetype.column_mut::<C>()?;
        let cell = column.cells_mut().get_mut(location.row())?;
        // SAFETY: `&mut World` excludes every query and every other reference to
        // the world's components, so nothing else can be looking at this row.
        Some(unsafe { &mut *cell.get() }.take())
    }

    /// Put back a callback moved out by the world's own run of it.
    fn restore_lifecycle<C: 'static + Lifecycle>(
        &mut self,
        entity: Entity,
        callback: LifecycleCallback,
    ) {
        let location = self
            .location(entity)
            .expect("a lifecycle callback was moved out of a live entity's row");
        let archetype = self
            .archetypes
            .get_mut(location.archetype())
            .expect("a live entity's location names a registered archetype");
        let column = archetype
            .column_mut::<C>()
            .expect("the column the callback came from is still registered");
        let cell = column
            .cells_mut()
            .get_mut(location.row())
            .expect("the row still exists");
        // SAFETY: as above, `&mut World` excludes every other reference.
        unsafe { &mut *cell.get() }.restore(callback)
    }

    // -- queries and diagnostics -------------------------------------------

    /// The name of a component type that has entered the world, as
    /// [`core::any::type_name`] reported it, or `None` when no entity has ever
    /// carried it.
    #[must_use]
    pub fn type_name(&self, type_id: TypeId) -> Option<&'static str> {
        self.names.get(&type_id).copied()
    }

    // -- deferred structural changes ---------------------------------------

    /// Apply every queued command, in order.
    ///
    /// Commands queued while applying are applied as well, after the ones
    /// already in the queue. A command that fails does not stop the ones after
    /// it: the queue is drained either way, and the caller is answered with the
    /// first error.
    pub fn apply(&mut self) -> Result<(), EntityAccessError> {
        let mut first = Ok(());
        loop {
            let Some(command) = self.commands_write().pop_front() else {
                return first;
            };
            let result = command.apply(self);
            if first.is_ok() {
                first = result;
            }
        }
    }

    // -- internals ---------------------------------------------------------

    #[inline]
    pub(crate) fn entities_read(&self) -> Ref<'_, Entities> {
        self.entities
            .try_borrow()
            .unwrap_or_else(|_| panic!("the entity table is already exclusively borrowed"))
    }

    pub(crate) fn entities_write(&self) -> RefMut<'_, Entities> {
        self.entities
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the entity table is already borrowed"))
    }

    #[inline]
    fn tree_mut(&mut self) -> &mut ScopeTree {
        &mut self.tree
    }

    pub(crate) fn commands_write(&self) -> CommandsGuard<'_> {
        self.commands
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the command queue is already borrowed"))
    }

    /// Append a command to the deferred queue.
    pub(crate) fn push_command(&self, command: Box<dyn Command>) {
        self.commands_write().push_back(command);
    }

    /// Register a claim on one component type and answer its id.
    ///
    /// Panics when a claim already held for the same type clashes with it. See
    /// [`crate::borrow`] for what clashes.
    #[track_caller]
    pub(crate) fn claim(
        &self,
        type_id: TypeId,
        name: &'static str,
        mode: Mode,
        rows: Rows,
        origin: &'static core::panic::Location<'static>,
    ) -> u64 {
        self.borrow
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the borrow ledger is already borrowed"))
            .claim(type_id, name, mode, rows, origin)
    }

    /// Drop the claims with these ids, if they are still held.
    pub(crate) fn release(&self, ids: &mut Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        self.borrow_write().release_all(ids);
    }

    fn borrow_write(&self) -> RefMut<'_, BorrowLedger> {
        self.borrow
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the borrow ledger is already borrowed"))
    }

    /// The archetype with exactly the types of `components`, creating it when
    /// it is new. Every type must have a registered column constructor.
    ///
    /// A hit allocates nothing: the types are hashed as the components are
    /// walked and compared in place, so the type sequence is only materialised
    /// when a new archetype has to be registered.
    fn archetype_for_components(&mut self, components: &[BundleComponent]) -> u32 {
        let found = self.archetypes.find_with(
            components.iter().map(|component| component.type_id),
            |stored| {
                stored.len() == components.len()
                    && stored
                        .iter()
                        .zip(components)
                        .all(|(type_id, component)| *type_id == component.type_id)
            },
        );
        if let Some(id) = found {
            return id;
        }
        let types: Box<[TypeId]> = components
            .iter()
            .map(|component| component.type_id)
            .collect();
        let names: Box<[&'static str]> =
            components.iter().map(|component| component.name).collect();
        let columns: Box<[Box<dyn AnyColumn>]> = types
            .iter()
            .map(|type_id| {
                let ctor = self.ctors.get(type_id).unwrap_or_else(|| {
                    panic!(
                        "no column constructor is registered for `{}`",
                        self.type_name(*type_id).unwrap_or("an unnamed type"),
                    )
                });
                ctor()
            })
            .collect();
        self.archetypes
            .register(Archetype::new(types, names, columns))
    }

    /// Where a live entity's components live.
    #[inline]
    pub(crate) fn location(&self, entity: Entity) -> Result<Location, EntityAccessError> {
        self.entities_read().location(entity)
    }

    /// What a live entity is called, read from the row that stores it.
    ///
    /// This is the only durable copy of the label: the allocator keeps no name,
    /// and a handle rebuilt from bare bits carries none.
    pub fn entity_name(&self, entity: Entity) -> Result<&'static str, EntityAccessError> {
        let location = self.location(entity)?;
        Ok(self.archetypes.get(location.archetype())?.entities()[location.row()].name())
    }

    /// The archetypes, including the empty one.
    ///
    /// This is the storage itself, not a view of one scope: it holds every row
    /// of every scope. Reading it through a [`Scope`] is what filters by
    /// visibility.
    #[must_use]
    pub fn archetypes(&self) -> &Archetypes {
        &self.archetypes
    }

    /// Drop the row at `location`, keeping every other location correct.
    pub(crate) fn remove_row(&mut self, location: Location) {
        let archetype = self
            .archetypes
            .get_mut(location.archetype())
            .expect("a live entity's location names a registered archetype");
        if let Some(moved) = archetype.swap_remove(location.row()) {
            self.entities_write().set_location(
                moved,
                Location {
                    archetype: location.archetype(),
                    row: location.row() as u32,
                },
            );
        }
    }

    pub(crate) fn free_entity(&mut self, entity: Entity) -> bool {
        self.entities_write().free(entity)
    }
}

impl core::fmt::Debug for World {
    /// Reports the tree's roots and the archetype pool, not the entities.
    ///
    /// The entity table, the borrow ledger and the command queue live in cells,
    /// and a debug print is most likely to run while one of them is already
    /// borrowed. Borrowing them here would panic inside the panic message that
    /// is being built, so they are left out rather than read on a best effort.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("World")
            .field("root", &self.root)
            .field("shared", &self.shared)
            .field("scaffolding", &self.scaffolding)
            .field("archetypes", &self.archetypes)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct Marker;

    /// The lookup behind the lifecycle hooks, which is its only caller: an
    /// archetype that does not carry the component simply has no hook to run.
    #[test]
    fn a_component_the_archetype_does_not_carry_runs_no_hook() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(crate::bundle![Marker]);
        assert!(
            world
                .take_lifecycle::<crate::lifecycle::OnSpawn>(entity)
                .is_none(),
            "the archetype has no OnSpawn column",
        );
        let dead = world.root_scope_mut().spawn(crate::bundle![Marker]);
        world.root_scope_mut().despawn(dead).unwrap();
        assert!(
            world
                .take_lifecycle::<crate::lifecycle::OnSpawn>(dead)
                .is_none(),
            "the handle is dead",
        );
    }
}
