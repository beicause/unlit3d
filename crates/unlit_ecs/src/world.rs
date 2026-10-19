//! The world: entities, their components, and the scopes they live in.
//!
//! Components live in `RefCell`s, so a component can be read or written
//! through a shared `&World`. The world is therefore `!Send`: it stays on the
//! thread that created it, which is where a renderer or any other `!Send`
//! state lives.
//!
//! Reading and writing components needs only `&World`, because every value
//! lives in a cell. Structural changes -- spawning and despawning -- need
//! `&mut World`. A callback that only has a shared world can queue those
//! changes instead with [`crate::Commands`] and apply them later with
//! [`World::apply`].
//!
//! Every entity lives in a scope, and a scope is another entity: the one its
//! [`ScopeOf`] component names. The world therefore holds a tree, and the
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
use crate::bundle::Bundle;
use crate::column::AnyColumn;
use crate::command::Command;
use crate::entity::{Entities, Entity, Location};
use crate::hash::TypeIdHashMap;
use crate::lifecycle::Lifecycle;
use crate::scope::{ScopeOf, ScopeTree};
use crate::view::{Scope, ScopeMut};

/// Entities and their components.
pub struct World {
    entities: RefCell<Entities>,
    archetypes: Archetypes,
    /// Column constructors, one per component type that ever entered the world.
    ctors: TypeIdHashMap<fn() -> Box<dyn AnyColumn>>,
    /// The name of every component type that ever entered the world.
    names: TypeIdHashMap<&'static str>,
    commands: RefCell<VecDeque<Box<dyn Command>>>,
    /// The scope tree, derived from the [`ScopeOf`] components.
    tree: RefCell<ScopeTree>,
    /// The entity that owns the world, whose scope sees everything.
    root: Entity,
    /// The entity whose subtree every scope sees.
    shared: Entity,
    /// The archetype holding the two scaffolding entities.
    ///
    /// They are the only entities that exist without being content, so a scope
    /// that counts rows has to know where they are.
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
    /// A world holding its root and shared scope, and nothing else.
    ///
    /// Both are ordinary entities: they appear in the root scope's count and
    /// can carry components. Neither can be despawned or linked.
    pub fn new() -> Self {
        let mut world = Self {
            entities: RefCell::new(Entities::default()),
            archetypes: Archetypes::new(),
            ctors: TypeIdHashMap::default(),
            names: TypeIdHashMap::default(),
            commands: RefCell::new(VecDeque::new()),
            tree: RefCell::new(ScopeTree::default()),
            root: Entity::PLACEHOLDER,
            shared: Entity::PLACEHOLDER,
            scaffolding: 0,
        };
        // The root's scope is itself, which is where walking up the tree stops.
        // It is recorded before the shared scope is inserted, because that one
        // hangs from it.
        let root = world.insert_root_row();
        world.root = root;
        let shared = world.insert_root_row();
        world.shared = shared;
        world.scaffolding = world
            .location(root)
            .expect("the root has a row")
            .archetype();
        world
    }

    /// Insert one of the two entities the tree hangs from.
    ///
    /// Neither exists yet when [`World::new`] runs, so the root link is written
    /// here rather than through the spawn path, which reads it.
    fn insert_root_row(&mut self) -> Entity {
        let entity = self.entities_write().alloc();
        let scope = if self.root == Entity::PLACEHOLDER {
            entity
        } else {
            self.root
        };
        self.insert_row(entity, scope, ());
        entity
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

    /// Whether `entity` is the root.
    #[must_use]
    pub fn is_root(&self, entity: Entity) -> bool {
        entity == self.root
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
        self.tree_read().is_descendant(ancestor, entity)
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
        self.is_shared(candidate)
            || caller == self.root
            || candidate == caller
            || self.is_descendant(caller, candidate)
    }

    /// Whether a scope of `caller` may despawn or relink `entity`.
    ///
    /// Ownership is stricter than visibility: the shared subtree is readable
    /// from everywhere but only the root may move or remove it.
    #[must_use]
    pub fn owns(&self, caller: Entity, entity: Entity) -> bool {
        caller == self.root || entity == caller || self.is_descendant(caller, entity)
    }

    /// The parent of a live entity, or `None`.
    #[must_use]
    pub fn parent_scope(&self, entity: Entity) -> Option<Entity> {
        self.tree_read().parent(entity)
    }

    /// The direct children of `entity`.
    #[must_use]
    pub fn children(&self, entity: Entity) -> Vec<Entity> {
        self.tree_read().children(entity).to_vec()
    }

    /// Rebuild the scope index from the [`ScopeOf`] components.
    ///
    /// The components are the truth and the index is derived, so this is the
    /// repair a caller runs if it ever suspects the two have drifted.
    pub fn rebuild_children(&mut self) {
        let pairs: Vec<(Entity, Entity)> = self
            .root_scope()
            .query::<(Entity, &ScopeOf)>()
            .map(|(entity, (_, scope))| (entity, scope.0))
            .collect();
        self.tree_write().rebuild(pairs.into_iter());
    }

    /// The number of live entities, the tree's scaffolding included.
    #[must_use]
    pub(crate) fn live_count(&self) -> usize {
        self.entities_read().len()
    }

    /// Whether `archetype` holds the tree's two scaffolding entities.
    #[must_use]
    pub(crate) fn is_scaffolding_archetype(&self, archetype: u32) -> bool {
        archetype == self.scaffolding
    }

    // -- scopes -------------------------------------------------------------

    /// The scope of the root: everything in the world.
    #[must_use]
    pub fn root_scope(&self) -> Scope<'_> {
        Scope::new(self, self.root)
    }

    /// The scope of `scope`, or `None` when there is no such scope.
    #[must_use]
    pub fn scope(&self, scope: Entity) -> Option<Scope<'_>> {
        self.tree_read().parent(scope)?;
        Some(Scope::new(self, scope))
    }

    /// The mutable scope of the root: everything in the world.
    #[must_use]
    pub fn root_scope_mut(&mut self) -> ScopeMut<'_> {
        ScopeMut::new(self, self.root)
    }

    /// The mutable scope of `scope`, or `None` when there is no such scope.
    #[must_use]
    pub fn scope_mut(&mut self, scope: Entity) -> Option<ScopeMut<'_>> {
        self.tree_read().parent(scope)?;
        Some(ScopeMut::new(self, scope))
    }

    // -- handles -----------------------------------------------------------

    /// A handle to an entity that does not exist yet.
    ///
    /// The handle becomes live when a bundle is put on it, either with a
    /// scope's spawn or by applying a queued [`crate::Commands::spawn`].
    pub fn reserve_entity(&self) -> Entity {
        self.entities_write().reserve()
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
    pub(crate) fn spawn_in<B: Bundle>(&mut self, scope: Entity, bundle: B) -> Entity {
        let entity = self.entities_write().alloc();
        self.insert_row(entity, scope, bundle);
        self.finish_spawn(entity);
        entity
    }

    /// Spawn `bundle` on `entity`, a handle from [`World::reserve_entity`], in
    /// `scope`.
    ///
    /// Panics when the handle already refers to a live entity. This is a hard
    /// check rather than a debug assertion: a second spawn would leave the
    /// entity with two rows, one of them unreachable and impossible to free.
    pub(crate) fn spawn_at_in<B: Bundle>(&mut self, entity: Entity, scope: Entity, bundle: B) {
        assert!(
            !self.entities_read().contains(entity),
            "{entity:?} is already spawned"
        );
        self.entities_write().take_reserved(entity);
        self.insert_row(entity, scope, bundle);
        self.finish_spawn(entity);
    }

    /// Despawn `entity` and its whole subtree.
    ///
    /// Children are removed before the parent they lived in, so a child's
    /// despawn callback still sees living ancestors. Returns whether the entity
    /// was alive.
    pub(crate) fn despawn_in(&mut self, entity: Entity) -> bool {
        if !self.entities_read().contains(entity) {
            return false;
        }
        let doomed = self.tree_read().subtree_post_order(entity);
        for victim in doomed {
            self.remove_entity(victim);
        }
        true
    }

    /// Make `child` a child of `parent`.
    ///
    /// Neither may be a root or in the shared subtree, both must be alive, and
    /// `parent` may not be inside `child`'s subtree: moving a scope into its
    /// own descendant would cut the tree in two.
    pub(crate) fn link_in(&mut self, child: Entity, parent: Entity) -> bool {
        if child == parent || self.is_root(child) || self.is_root(parent) {
            return false;
        }
        if self.is_shared(child) || self.is_shared(parent) {
            return false;
        }
        if !self.entities_read().contains(child) || !self.entities_read().contains(parent) {
            return false;
        }
        if self.is_descendant(child, parent) {
            return false;
        }
        self.write_scope(child, parent);
        true
    }

    /// Detach `child` from its parent and put it in `scope`.
    pub(crate) fn unlink_in(&mut self, child: Entity, scope: Entity) -> bool {
        if self.is_root(child) || self.is_shared(child) {
            return false;
        }
        if !self.entities_read().contains(child) || !self.entities_read().contains(scope) {
            return false;
        }
        if self.parent_scope(child) == Some(scope) {
            return false;
        }
        self.write_scope(child, scope);
        true
    }

    /// Rewrite the [`ScopeOf`] component of a live entity and reindex it.
    fn write_scope(&mut self, entity: Entity, scope: Entity) {
        let Some(location) = self.location(entity) else {
            return;
        };
        let archetype = self.archetypes.get_mut(location.archetype());
        let Some(cell) = archetype.cell::<ScopeOf>(location.row()) else {
            return;
        };
        *cell.borrow_mut() = ScopeOf(scope);
        archetype.recompute_scope_summary();
        self.tree_write().attach(entity, scope);
    }

    // -- the row itself ----------------------------------------------------

    /// Insert a row for `entity` in `scope`, with the [`ScopeOf`] component the
    /// world writes and the ones `bundle` carries.
    fn insert_row<B: Bundle>(&mut self, entity: Entity, scope: Entity, bundle: B) {
        let (types, mut values, ctors) = bundle.into_builder().finish();
        for (type_id, name, ctor) in ctors {
            self.ctors.entry(type_id).or_insert(ctor);
            self.names.entry(type_id).or_insert(name);
        }
        // The scope is the world's to write, so a bundle that names it is a
        // programming error rather than a second source for the same fact.
        assert!(
            !types.contains(&TypeId::of::<ScopeOf>()),
            "a bundle cannot carry ScopeOf; spawn in a scope instead",
        );
        self.ctors
            .entry(TypeId::of::<ScopeOf>())
            .or_insert_with(crate::column::Column::<ScopeOf>::eraser);
        self.names
            .entry(TypeId::of::<ScopeOf>())
            .or_insert(core::any::type_name::<ScopeOf>());
        values.push((TypeId::of::<ScopeOf>(), Box::new(ScopeOf(scope))));
        let mut types: Vec<TypeId> = types.into_vec();
        types.push(TypeId::of::<ScopeOf>());
        types.sort_unstable();
        values.sort_unstable_by_key(|(type_id, _)| *type_id);

        let archetype = self.archetype_for_types(&types);
        let target = self.archetypes.get_mut(archetype);
        for (type_id, value) in values {
            target.push_erased(type_id, value);
        }
        target.push_entity(entity, scope);
        let row = (target.len() - 1) as u32;
        self.entities_write()
            .set_location(entity, Location { archetype, row });
        self.tree_write().attach(entity, scope);
    }

    /// Run the despawn callbacks of `entity` and drop its row and handle.
    fn remove_entity(&mut self, entity: Entity) {
        let Some(location) = self.location(entity) else {
            return;
        };
        self.run_lifecycle::<crate::OnDespawn>(entity);
        self.tree_write().detach(entity);
        self.remove_row(location);
        self.free_entity(entity);
    }

    /// Run the spawn callbacks of an entity whose row is already in place.
    pub(crate) fn finish_spawn(&mut self, entity: Entity) {
        self.run_lifecycle::<crate::OnSpawn>(entity);
    }

    /// Run every callback of the lifecycle component `C` the entity carries.
    ///
    /// The callbacks run in the order they were registered. The scope is the
    /// one of the entity's own scope, which is its parent: a callback that
    /// wants the entity's own subtree descends into the entity itself.
    fn run_lifecycle<C: 'static + Lifecycle>(&mut self, entity: Entity) {
        let Some(scope) = self.parent_scope(entity) else {
            return;
        };
        let Some(location) = self.location(entity) else {
            return;
        };
        if self
            .archetypes
            .get(location.archetype())
            .column::<C>()
            .is_none()
        {
            return;
        }
        let scope = Scope::new(self, scope);
        let mut hooks = self
            .get_mut::<C>(entity)
            .expect("the column was just found");
        for callback in hooks.callbacks() {
            callback(&scope, entity);
        }
    }

    // -- queries and diagnostics -------------------------------------------

    /// The number of archetypes, including the empty one.
    #[must_use]
    pub fn archetype_count(&self) -> usize {
        self.archetypes.len()
    }

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
    /// already in the queue.
    pub fn apply(&mut self) {
        loop {
            let Some(command) = self.commands_write().pop_front() else {
                return;
            };
            command.apply(self);
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

    pub(crate) fn tree_read(&self) -> Ref<'_, ScopeTree> {
        self.tree
            .try_borrow()
            .unwrap_or_else(|_| panic!("the scope tree is already exclusively borrowed"))
    }

    fn tree_write(&self) -> RefMut<'_, ScopeTree> {
        self.tree
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the scope tree is already borrowed"))
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

    /// The archetype with exactly `types` (sorted), creating it when it is
    /// new. Every type must have a registered column constructor.
    fn archetype_for_types(&mut self, types: &[TypeId]) -> u32 {
        if let Some(id) = self.archetypes.find(types) {
            return id;
        }
        let columns: Vec<Box<dyn AnyColumn>> = types
            .iter()
            .map(|type_id| {
                let ctor = self
                    .ctors
                    .get(type_id)
                    .expect("every component type is registered before its archetype exists");
                ctor()
            })
            .collect();
        self.archetypes
            .register(Archetype::new(types.into(), columns.into_boxed_slice()))
    }

    /// The archetype with this id, or `None` when there is no such archetype.
    pub(crate) fn archetype_at(&self, id: u32) -> Option<&Archetype> {
        self.archetypes.as_slice().get(id as usize)
    }

    /// Where a live entity's components live, or `None` when it is not alive.
    #[inline]
    pub(crate) fn location(&self, entity: Entity) -> Option<Location> {
        self.entities_read().location(entity)
    }

    /// The archetypes, including the empty one, as a slice.
    pub(crate) fn archetypes_slice(&self) -> &[Archetype] {
        self.archetypes.as_slice()
    }

    /// The component of `entity`, or `None` when it is missing or the entity is
    /// not alive.
    ///
    /// Panics when the component is already exclusively borrowed.
    #[must_use]
    #[inline]
    pub(crate) fn get<C: 'static>(&self, entity: Entity) -> Option<Ref<'_, C>> {
        let location = self.entities_read().location(entity)?;
        let cell = self
            .archetypes
            .get(location.archetype())
            .cell::<C>(location.row())?;
        Some(
            cell.try_borrow()
                .unwrap_or_else(|_| borrow_conflict::<C>("read")),
        )
    }

    /// The component of `entity`, exclusively.
    ///
    /// Panics when the component is already borrowed, shared or exclusive.
    #[must_use]
    #[inline]
    pub(crate) fn get_mut<C: 'static>(&self, entity: Entity) -> Option<RefMut<'_, C>> {
        let location = self.entities_read().location(entity)?;
        let cell = self
            .archetypes
            .get(location.archetype())
            .cell::<C>(location.row())?;
        Some(
            cell.try_borrow_mut()
                .unwrap_or_else(|_| borrow_conflict::<C>("write")),
        )
    }

    /// Drop the row at `location`, keeping every other location correct.
    pub(crate) fn remove_row(&mut self, location: Location) {
        let archetype = self.archetypes.get_mut(location.archetype());
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

/// The borrow-conflict panic.
fn borrow_conflict<C: 'static>(kind: &str) -> ! {
    panic!(
        "component `{}` is already borrowed while it is being {}",
        core::any::type_name::<C>(),
        kind,
    )
}
