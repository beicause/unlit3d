//! What tells one world from another, and the shape an entity handle takes
//! when it crosses a world boundary.
//!
//! Every world numbers its entities from zero, so a single [`Entity`] value
//! exists in all of them and means something different in each. A handle that
//! leaves the world that made it therefore travels as a [`RemoteEntity`] - the
//! handle and the [`WorldId`] of the world it was made in - and [`World::resolve`]
//! turns one down when it names a world other than the resolver's, rather than
//! handing out whatever local entity happens to share its bits.
//!
//! An id is assigned when the world is made and stays with it until it is
//! dropped; a world made later gets a fresh one, so a handle to a world that no
//! longer exists is turned down rather than answered by its replacement.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::Entity;

/// What tells one world from another.
///
/// The number behind it is assigned in creation order and is not stable across
/// runs; [`Debug`] prints it, so an id reads as `WorldId(3)` in a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorldId(u64);

impl WorldId {
    /// The id of the next world made in this process.
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// An [`Entity`] together with the world that made it.
///
/// This is the handle to carry across a thread, a job or a channel. The plain
/// [`Entity`] is only an index and a generation into one world's storage, so the
/// pair is what stays meaningful once the handle is read anywhere else;
/// [`World::remote`] makes one and [`World::resolve`] reads one back.
///
/// The fields are public so that a message can be built and taken apart without
/// a world at hand: the world that receives it is the one that decides whether
/// it may be resolved.
///
/// [`Debug`] prints both halves, and the entity half prints its name when the
/// handle still carries one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteEntity {
    /// The world the handle was made in.
    pub world: WorldId,
    /// The handle that world made.
    pub entity: Entity,
}
