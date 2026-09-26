//! Components.
//!
//! A component is any `'static` type. Components live in archetypes, and an
//! entity's set of components is fixed when it is spawned: components are only
//! read and written, never added or removed. To change the set of components,
//! despawn the entity and spawn a new one.

/// A value stored on an entity.
///
/// This is implemented for every `'static` type, so it only serves as
/// documentation and as a bound: it is what keeps a non-`'static` type from
/// being used as a component.
pub trait Component: 'static {}

impl<T: 'static> Component for T {}
