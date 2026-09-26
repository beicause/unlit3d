#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod archetype;
mod bundle;
mod command;
mod component;
mod entity;
mod hash;
mod mode;
mod query;
#[cfg(test)]
mod tests_common;
mod world;

pub use archetype::{Archetype, Archetypes};
pub use bundle::{ArchetypeBuilder, Bundle};
pub use command::{Command, CommandErase, Commands};
#[doc(hidden)]
pub use component::Component;
pub use entity::Entity;
pub use hash::{EntityHashMap, EntityHashSet, TypeIdHashMap, TypeIdHashSet};
pub use mode::{LocalMode, Mode, SendMode};
pub use query::{Or, Query, QueryFilter, QueryIter, With, Without};
pub use world::World;

/// The world whose components live in `RefCell`s.
///
/// It is `!Send`, so it stays on the thread that created it; this is where a
/// renderer and other thread-bound state belong.
pub type LocalWorld = World<LocalMode>;

/// The world whose components live in `RwLock`s.
///
/// It is `Send + Sync`, so worker threads may read and write it.
pub type SendWorld = World<SendMode>;

/// The types most callers need.
pub mod prelude {
    pub use crate::{Entity, LocalWorld, Query, SendWorld, Without, World};
}
