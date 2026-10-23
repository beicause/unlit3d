//! The errors an access that named an entity or an archetype answers with.
//!
//! Every lookup that names an entity answers [`Result`] rather than `Option`.
//! A handle travels over wires, save files and scope boundaries, so a lookup can
//! be turned down for reasons a caller has to tell apart: the entity is gone, it
//! is alive but out of the scope's reach, or its archetype does not carry what
//! was asked for. `Option` collapses all of them into one silence.
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

    /// The entity carries no component of type `C`.
    #[error("{entity:?} carries no `{component}`")]
    MissingComponent {
        /// The handle that was looked up.
        entity: Entity,
        /// The component type that was asked for, as
        /// [`core::any::type_name`] reports it.
        component: &'static str,
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
