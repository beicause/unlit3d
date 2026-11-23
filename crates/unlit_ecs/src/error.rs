//! The errors an access that named an entity or an archetype answers with.
//!
//! A lookup that names an entity is never `Option`. A handle travels over wires,
//! save files and scope boundaries, so a lookup can be turned down for reasons a
//! caller has to tell apart: the entity is gone, it is alive but out of the
//! scope's reach, or its archetype does not carry what was asked for. `Option`
//! collapses all of them into one silence, so the reasons are variants here
//! instead.
//!
//! The accessors that are most often called on a handle the caller just took -
//! opening a scope, opening a nested one, reading one row by handle - hand their
//! item out directly and panic with the variant's own message. They are the
//! shortcuts; the `try_` name next to each answers this type, which is what a
//! caller holding a handle from elsewhere uses. See the module documentation of
//! [`crate::view`].
//!
//! A structural change keeps the `Result` form throughout. Despawning and
//! relinking are turned down for reasons of their own - the entity is out of the
//! subtree, it is shared, the world holds it for itself - and a caller that was
//! refused has to know which it was: the answer to "not yours" is to ask the
//! scope that owns it, and the answer to "internal" is to stop asking.
//!
//! A borrow conflict is deliberately not one of these. It says nothing about the
//! handle, only that another holder has already claimed the component, and it
//! stays a panic; see the module documentation of [`crate`].

use crate::Entity;
use crate::identity::WorldId;

/// Why an entity lookup or structural change was turned down.
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

    /// The entity is named more than once in a list it was looked up with.
    ///
    /// Two items of the list would reach the same row, so the list is refused
    /// whole rather than yielding the row twice.
    #[error("{entity:?} is named more than once")]
    Repeated {
        /// The handle that was named twice.
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

    /// The bundle declares a singleton key that a live entity already holds.
    ///
    /// A key is held world-wide from the moment its entity's row goes in until
    /// that row is removed, so a scope that cannot see the holder is refused
    /// too. The error names the key rather than the holder, which may be out of
    /// sight.
    #[error("the singleton {key} is already held by a live entity")]
    SingletonTaken {
        /// The name of the key type, as [`core::any::type_name`] reports it.
        key: &'static str,
    },

    /// A cascade despawn kept finding entities to remove past its pass bound.
    ///
    /// Callbacks that keep adding entities under the subtree being removed
    /// never let it empty. The entities already removed stay removed; the rest
    /// of the subtree, `entity` included, stays standing.
    #[error("the despawn of {entity:?} did not finish within {passes} passes")]
    CascadeOverflow {
        /// The entity whose despawn was requested.
        entity: Entity,
        /// The number of passes the cascade was allowed.
        passes: usize,
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

    /// The entity is one of the rows the world holds for itself.
    ///
    /// The root and the shared scope are what the tree hangs from: they are
    /// neither counted nor queried, and they do not move or go. See
    /// [`Archetype::is_internal`](crate::Archetype::is_internal).
    #[error("{entity:?} is internal: it neither moves nor goes")]
    Internal {
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

/// Why a remote handle was turned down by the world it was offered to.
///
/// A handle crosses a thread, a job or a channel together with the id of the
/// world that made it; resolving one against a world is what tells the two
/// apart. It is a single variant because there is a single way to be turned
/// down: the handle is fine, it simply belongs somewhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteEntityError {
    /// The handle was made in a world other than the one it was offered to.
    ///
    /// Both worlds are named, so a message can say which handle was meant for
    /// where rather than only that it was refused.
    #[error("{entity:?} belongs to world {found:?}, not to {expected:?}")]
    ForeignWorld {
        /// The handle that was offered.
        entity: Entity,
        /// The world that was asked to resolve it.
        expected: WorldId,
        /// The world that made it.
        found: WorldId,
    },
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

/// Every entry of a list that a query over the list turned down, in list order.
///
/// A list is refused whole, so this is the full account of why. Each failure
/// names its own handle, and there is one failure for each refused position in
/// the list.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{} of the listed entities were turned down", .failures.len())]
pub struct ListAccessError {
    /// One failure for each refused position of the list, in list order.
    pub failures: Vec<EntityAccessError>,
}
