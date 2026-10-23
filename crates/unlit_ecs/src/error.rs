//! The errors an access that named an entity or an archetype answers with.
//!
//! Every lookup that names an entity answers [`Result`] rather than `Option`.
//! A handle travels over wires, save files and scope boundaries, so a lookup can
//! be turned down for reasons a caller has to tell apart: the entity is gone, it
//! is alive but out of the scope's reach, or its archetype does not carry what
//! was asked for. `Option` collapses all of them into one silence.
//!
//! A structural change is answered the same way. Despawning and relinking are
//! turned down for reasons of their own - the entity is out of the subtree, it is
//! shared, it is scaffolding - and a caller that was refused has to know which it
//! was: the answer to "not yours" is to ask the scope that owns it, and the answer
//! to "scaffolding" is to stop asking.
//!
//! A borrow conflict is deliberately not one of these. It says nothing about the
//! handle, only that the cell is busy at the moment it was asked for, and it
//! stays a panic; see the module documentation of [`crate`].

use crate::Entity;

/// Why a lookup of, or through, an entity handle was turned down.
///
/// The handle is carried by every variant, so a caller that reports the error
/// does not have to thread it alongside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EntityAccessError {
    /// No live entity has this handle: it was despawned, or it never named one.
    #[error("{entity:?} names no live entity")]
    NotAlive {
        /// The handle that was looked up.
        entity: Entity,
    },

    /// The entity is alive, but out of this scope's reach.
    #[error("{entity:?} is not visible from {scope:?}")]
    NotVisible {
        /// The handle that was looked up.
        entity: Entity,
        /// The scope it was looked up from.
        scope: Entity,
    },

    /// The entity is visible here, but its archetype does not satisfy the
    /// query it was looked up with.
    #[error("{entity:?} does not match the query")]
    NotMatched {
        /// The handle that was looked up.
        entity: Entity,
    },

    /// The handle was not reserved, so there is no row to spawn on.
    ///
    /// A handle from [`crate::World::reserve_entity`] can be spawned on once.
    /// Spawning on it again, on a handle that was released, or on one that was
    /// never reserved would leave a row nobody can reach or free.
    #[error("{entity:?} was not reserved")]
    NotReserved {
        /// The handle that was spawned on.
        entity: Entity,
    },

    /// The entity is alive, but this scope's subtree does not reach it, so it
    /// is not the scope's to remove or move.
    ///
    /// Despawning and relinking walk the subtree, so what a scope cannot reach
    /// from inside it is also not its to change. The scope that owns the entity
    /// - the root, for anything beside this one - is the one to ask.
    #[error("{entity:?} is outside {scope:?}, so it is not its to move or remove")]
    NotOwned {
        /// The handle that was moved or removed.
        entity: Entity,
        /// The scope that tried.
        scope: Entity,
    },

    /// The entity is visible here, but only the root may move or remove it.
    ///
    /// The shared subtree is readable from every scope and rearrangeable by none
    /// but the root: a scope that reads a resource there is a borrower, not an
    /// owner.
    #[error("{entity:?} is shared, so only the root may move or remove it")]
    NotMovable {
        /// The handle that was moved or removed.
        entity: Entity,
        /// The scope that tried.
        scope: Entity,
    },

    /// The entity is one of the tree's two scaffolding entities.
    ///
    /// The root and the shared scope are what the tree hangs from: they are
    /// neither counted nor queried, and they do not move or go.
    #[error("{entity:?} is scaffolding: it neither moves nor goes")]
    Scaffolding {
        /// The handle that was moved or removed.
        entity: Entity,
    },

    /// Moving `child` under `parent` would put a scope inside its own subtree,
    /// which would cut the tree in two.
    #[error("{child:?} cannot be moved under {parent:?}, which it holds")]
    WouldCycle {
        /// The handle that would have moved.
        child: Entity,
        /// The parent it would have moved under.
        parent: Entity,
    },

    /// The entity's archetype could not be reached.
    #[error(transparent)]
    Archetype(#[from] ArchetypeAccessError),
}

/// Why a lookup by archetype id was turned down.
///
/// An archetype id is the row it was registered at, so a stale id keeps naming
/// a different archetype instead of naming nothing. These variants are what a
/// caller checks against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ArchetypeAccessError {
    /// No archetype was ever registered under this id.
    #[error("no archetype has id {id}")]
    NoSuchArchetype {
        /// The id that was looked up.
        id: u32,
    },

    /// The archetype exists, but it holds no row to look at.
    #[error("archetype {id} holds no row")]
    Empty {
        /// The id that was looked up.
        id: u32,
    },

    /// The archetype holds rows this scope cannot reach.
    #[error("archetype {id} holds rows that {scope:?} cannot see")]
    NotVisible {
        /// The id that was looked up.
        id: u32,
        /// The scope the lookup was made from.
        scope: Entity,
    },
}
