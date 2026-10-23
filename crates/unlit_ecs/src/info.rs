//! Plain-data summaries of a world's shape.
//!
//! A world stores components behind type-erased columns, so a caller that only
//! has a scope can count entities and archetypes but cannot name a component:
//! the names live in a side table keyed by [`TypeId`]. These structures put a
//! name back on every column and borrow nothing from the world, which is what a
//! bridge that reports a world over JSON needs.
//!
//! Every count here is what one scope sees, not what the world holds: a scope of a
//! scene reports that scene. The two root entities are reported by handle so
//! that a caller can tell why the count is two higher than the content.
//!
//! They are deliberately shallow. Nothing here reaches into a component's own
//! fields -- that is the caller's business, and a caller that wants it derives
//! its own reflection.
//!
//! [`TypeId`]: core::any::TypeId

use crate::entity::Entity;

/// A live entity's location and component names.
///
/// Only a live, visible entity has one: [`Scope::entity_info`] answers
/// [`EntityAccessError::NotAlive`] or [`EntityAccessError::NotVisible`] for a
/// handle that is neither, so there is no `alive` field to report.
///
/// [`EntityAccessError::NotAlive`]: crate::EntityAccessError::NotAlive
/// [`EntityAccessError::NotVisible`]: crate::EntityAccessError::NotVisible
///
/// [`Scope::entity_info`]: crate::Scope::entity_info
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct EntityInfo {
    /// The entity's handle.
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = crate::EntityProxy))]
    pub entity: Entity,
    /// The entity's name, which its handle carries but its bits do not, so a
    /// reader of this listing sees the label the entity was spawned with even
    /// though the handle travels as a bare integer.
    pub name: &'static str,
    /// The archetype the entity lives in, as [`Location::archetype`].
    ///
    /// [`Location::archetype`]: crate::Location::archetype
    pub archetype: u32,
    /// The entity's row in that archetype, as [`Location::row`].
    ///
    /// [`Location::row`]: crate::Location::row
    pub row: usize,
    /// The component names of the entity's archetype, in column order.
    pub components: Vec<&'static str>,
}

/// One archetype: its id, how many of its rows a scope sees, and its component
/// names.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ArchetypeInfo {
    /// The archetype id, the value [`EntityInfo::archetype`] carries.
    pub index: u32,
    /// The number of rows visible from the scope, not the archetype's total.
    ///
    /// This is the only definition under which the lengths sum to
    /// [`WorldInfo::entities`].
    pub len: usize,
    /// Its component names, in column order.
    pub components: Vec<&'static str>,
}

/// A scope's high-level shape.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct WorldInfo {
    /// The number of live entities visible from the scope, the roots included.
    pub entities: usize,
    /// The number of archetypes with at least one visible row.
    pub archetypes: usize,
    /// The names of the component types this scope can see, sorted.
    ///
    /// A type is named here when an archetype holding at least one visible row
    /// carries it, which is the same rule as [`WorldInfo::archetypes`]: the two
    /// describe the rows the scope reaches, not the types that ever entered the
    /// world.
    pub components: Vec<&'static str>,
    /// The root entity, which owns everything.
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = crate::EntityProxy))]
    pub root: Entity,
    /// The shared entity, whose subtree every scope sees.
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = crate::EntityProxy))]
    pub shared: Entity,
}
