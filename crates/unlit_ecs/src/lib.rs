#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

mod archetype;
mod bundle;
mod column;
mod command;
mod component;
mod entity;
mod hash;
mod info;
mod lifecycle;
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
pub mod mcp;
mod query;
#[cfg(feature = "reflect")]
pub mod reflect;
mod scope;
mod view;
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
pub use lifecycle::{Lifecycle, LifecycleCallback, OnDespawn, OnSpawn};
pub use query::{Access, Filter, Or, Query, QueryIter, Row, Slot, With, Without};
#[doc(hidden)]
pub use query::{IntoFilter, IntoFilterTuple, IntoSlot};
pub use scope::ScopeOf;
pub use view::{Scope, ScopeMut};
pub use world::World;

/// The types most callers need.
pub mod prelude {
    pub use crate::{
        Access, Entity, Filter, Or, Query, QueryIter, Row, Scope, ScopeOf, Slot, With, Without,
        World, bundle, filter, query,
    };
}
