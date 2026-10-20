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
pub mod query;
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
#[doc(hidden)]
pub use query::QueryItem;
pub use query::{ArchetypeFilter, Has, Query, QueryIter};
pub use scope::ScopeOf;
pub use view::{Scope, ScopeMut};
pub use world::World;

/// The types most callers need.
pub mod prelude {
    pub use crate::{
        ArchetypeFilter, Entity, Has, Query, QueryIter, Scope, ScopeOf, World, bundle,
    };
}
