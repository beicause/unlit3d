#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod archetype;
mod bundle;
mod column;
mod command;
mod component;
mod entity;
mod hash;
mod info;
mod query;
#[cfg(test)]
mod tests_common;
mod world;

pub use archetype::{Archetype, Archetypes};
pub use bundle::{ArchetypeBuilder, Bundle};
pub use column::{CellRef, CellRefMut};
pub use command::{Command, CommandErase, Commands};
#[doc(hidden)]
pub use component::Component;
pub use entity::{Entity, Location};
#[cfg(feature = "reflect")]
pub use entity::{EntityProxy, EntityVecProxy};
pub use hash::{EntityHashMap, EntityHashSet, TypeIdHashMap, TypeIdHashSet};
pub use info::{ArchetypeInfo, EntityInfo, WorldInfo};
pub use query::{Or, Query, QueryFilter, QueryIter, With, Without};
pub use world::World;

/// The types most callers need.
pub mod prelude {
    pub use crate::{Entity, Query, Without, World};
}
