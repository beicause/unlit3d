//! The world: entities, their components, and the scopes they live in.
//!
//! Components live in `UnsafeCell`s so that a shared `&World` can reach them,
//! and a borrow ledger written through that same shared reference catches two
//! access paths reaching one value when at least one of them writes. The world
//! is therefore `!Send`: it stays on the thread that created it, which is where
//! a renderer or any other `!Send` state lives.
//!
//! Reading and writing components needs only `&World`. Structural changes —
//! spawning and despawning — need `&mut World`. A caller with only a read-only
//! scope can queue those changes instead with [`crate::Commands`] and apply them
//! later with [`World::apply`].
//!
//! Every entity lives in a scope, and a scope is another entity: the one the
//! world's tree says it lives in. The world therefore holds a tree, and the
//! entry points that read or write it are scopes of a scope rather than
//! methods on the world — [`Scope`](crate::Scope) and
//! [`ScopeMut`](crate::ScopeMut). A scope sees its own subtree and nothing
//! beside it; only `&World` reaches the root.
//!
//! An entity's components are fixed when it is spawned; there is no way to add
//! or remove one later. To change the set of components, despawn the entity
//! and spawn a new one.
//!
//! A borrow conflict is a panic, not a compile error: two claims the ledger
//! cannot share — a shared claim still held while a write asks for the same
//! component, or the same component fetched as `&mut` twice in one query —
//! report the component's name and both claim sites, then stop. This is the
//! trade for not doing access analysis. An optional component is optional only
//! about the component being absent: a conflict over one the row does carry
//! panics as well.
//!
//! This file holds the fields and the constructor. The methods are split by
//! the state they touch, one file per responsibility:
//!
//! - [`tree`]: who lives where, and who may see or move it
//! - [`scopes`]: opening a scope by handle
//! - [`spawn`]: putting a bundle on a row
//! - [`despawn`]: taking rows out, cascading through the subtree
//! - [`link`]: moving a live entity to another scope
//! - [`lifecycle`]: running the callbacks a row stores
//! - [`commands`]: the queue applied by [`World::apply`]
//! - [`claims`]: the borrow ledger
//! - [`storage`]: plain reads of the entity table and archetype pool
//! - [`identity`]: telling one world's handles from another's

use core::any::TypeId;
use core::cell::RefCell;
use std::collections::VecDeque;

use crate::archetype::Archetypes;
use crate::borrow::BorrowLedger;
use crate::column::AnyColumn;
use crate::command::Command;
use crate::entity::{Entities, Entity};
use crate::hash::{EntityHashMap, TypeIdHashMap};
use crate::identity::WorldId;
use crate::scope::ScopeTree;

mod claims;
mod commands;
mod despawn;
mod identity;
mod lifecycle;
mod link;
mod scopes;
mod spawn;
mod storage;
#[cfg(test)]
mod tests;
mod tree;

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
    despawn: despawn::CascadeScratch,
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
    /// The live entity holding each singleton key.
    ///
    /// World-wide: a scope's visibility does not narrow a key. A key is claimed
    /// once its entity's row is in and released once the row is removed, after
    /// the entity's `OnDespawn` callback has run.
    singleton_holders: TypeIdHashMap<Entity>,
    /// The key each singleton holder claimed, so removing a holder finds its key
    /// without searching `singleton_holders`.
    singleton_keys: EntityHashMap<TypeId>,
    /// What tells this world's handles from another world's.
    id: WorldId,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// A world holding its root and shared scope, and nothing else.
    ///
    /// Both are ordinary entities: they are addressable, carry components and
    /// can be opened as scopes, and they are what a shared resource hangs from.
    /// Neither is counted, queried, despawned or linked: both are marked
    /// internal, which is what puts them in an archetype of their own. See
    /// [`crate::Archetype::is_internal`] and [`crate::Scope::len`], which
    /// reports the entities a scope sees with the pair left out.
    pub fn new() -> Self {
        let mut world = Self {
            entities: RefCell::new(Entities::default()),
            archetypes: Archetypes::new(),
            ctors: TypeIdHashMap::default(),
            names: TypeIdHashMap::default(),
            commands: RefCell::new(VecDeque::new()),
            tree: ScopeTree::default(),
            despawn: despawn::CascadeScratch::default(),
            borrow: RefCell::new(BorrowLedger::default()),
            // Both are overwritten below, before the world leaves this
            // function and anything can read them.
            root: Entity::from_raw(0, 0),
            shared: Entity::from_raw(0, 0),
            singleton_holders: TypeIdHashMap::default(),
            singleton_keys: EntityHashMap::default(),
            id: WorldId::next(),
        };
        // The root's scope is itself, which is where walking up the tree stops.
        // It is recorded before the shared scope is inserted, because that one
        // hangs from it. Neither goes through the spawn path, which reads the
        // root that is being written here.
        let root = world.entities_write().alloc("root");
        world.root = root;
        world.insert_row(root, root, crate::DynamicBundle::new().internal().finish());
        let shared = world.entities_write().alloc("shared");
        world.shared = shared;
        world.insert_row(
            shared,
            root,
            crate::DynamicBundle::new().internal().finish(),
        );
        world
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
            .field("archetypes", &self.archetypes)
            .finish_non_exhaustive()
    }
}
