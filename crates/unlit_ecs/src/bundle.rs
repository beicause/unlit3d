//! Bundles: the set of components an entity is spawned with.
//!
//! A bundle is built by the [`crate::bundle!`] macro or by an [`ArchetypeBuilder`]. It is
//! not a tuple: every `'static` type is a component, tuples included, so a bare
//! `(a, b)` handed to `spawn` names one component rather than two. Writing
//! `bundle!(a, b)` is what names two.
//!
//! The macro takes its items one at a time and each item is one component, so a
//! parenthesised item is a tuple component rather than a list to unpack: a
//! bundle only spreads when it is written as `..value`.
//!
//! The same component cannot appear twice in a bundle; spawning with duplicates
//! panics.

use core::any::{Any, TypeId};

use crate::column::{AnyColumn, Column};

/// One component of a bundle, erased into the value box the archetype takes.
pub(crate) type ErasedValue = (TypeId, Box<dyn Any>);

/// A column constructor, keyed by the component type it builds for, together
/// with that type's name.
pub(crate) type ColumnCtor = (TypeId, &'static str, fn() -> Box<dyn AnyColumn>);

/// The component types, values and column constructors of one completed bundle.
pub(crate) type FinishedColumns = (Box<[TypeId]>, Vec<ErasedValue>, Vec<ColumnCtor>);

/// Builds the components of one spawn; only [`Bundle`] implementations use it.
pub struct ArchetypeBuilder {
    values: Vec<ErasedValue>,
    ctors: Vec<ColumnCtor>,
}

impl Default for ArchetypeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchetypeBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            ctors: Vec::new(),
        }
    }

    /// Add one component.
    ///
    /// The value is stored under its own type, so a tuple pushed here stays one
    /// component.
    pub fn push_component<C: 'static>(&mut self, value: C) {
        self.values.push((TypeId::of::<C>(), Box::new(value)));
        self.ctors.push((
            TypeId::of::<C>(),
            core::any::type_name::<C>(),
            Column::<C>::eraser(),
        ));
    }

    /// Add every component of `bundle` to this builder.
    ///
    /// This is how a caller gathers several bundles into one spawn: the
    /// components keep their own types, so a bundle that holds a tuple
    /// contributes that tuple as a single component, exactly as spawning it
    /// alone would.
    pub fn push_bundle<B: Bundle>(&mut self, bundle: B) {
        let (_types, values, ctors) = bundle.into_builder().finish();
        self.values.extend(values);
        self.ctors.extend(ctors);
    }

    /// Sorted component types, the component values in the same order, and one
    /// column constructor per type.
    ///
    /// # Panics
    ///
    /// When the same component appears twice. The duplicate is caught here
    /// rather than at spawn so that every path into an archetype, queued or
    /// immediate, reports it from the call that wrote the bundle.
    #[track_caller]
    pub(crate) fn finish(self) -> FinishedColumns {
        let Self {
            mut values,
            mut ctors,
        } = self;
        values.sort_unstable_by_key(|(type_id, _)| *type_id);
        ctors.sort_unstable_by_key(|(type_id, _, _)| *type_id);
        for pair in values.windows(2) {
            assert_ne!(
                pair[0].0, pair[1].0,
                "a bundle cannot contain the same component twice",
            );
        }
        let types: Box<[TypeId]> = values.iter().map(|(type_id, _)| *type_id).collect();
        (types, values, ctors)
    }
}

/// A set of components to spawn an entity with.
///
/// Implemented for an [`ArchetypeBuilder`], which is what the [`crate::bundle!`] macro
/// produces, and for any type of a caller's own that can say what its
/// components are. It is deliberately not implemented for tuples: every
/// `'static` type is a component, so a tuple impl would make `(a, b)` mean two
/// components in one place and one component in another.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a bundle",
    label = "build it with `bundle!` or an `ArchetypeBuilder`",
    note = "`(a, b)` is a single component here: every `'static` type is one, tuples included. Write `bundle!(a, b)` for two components."
)]
pub trait Bundle {
    /// The builder holding exactly this bundle's components.
    ///
    /// The bundle supplies the whole builder, so there is no target to append
    /// to or overwrite; a caller that wants to gather several bundles calls
    /// [`ArchetypeBuilder::push_component`] for each component instead.
    fn into_builder(self) -> ArchetypeBuilder;
}

impl Bundle for ArchetypeBuilder {
    fn into_builder(self) -> ArchetypeBuilder {
        self
    }
}

/// Builds a bundle from the components written between the parentheses.
///
/// The items are components, separated by commas: `bundle!(a, b)` is two
/// components. An item is one component however it is written, because every
/// `'static` type is one: `bundle!((a, b))` names a single tuple component, and
/// `bundle!(())` a single empty-tuple one.
///
/// ````rust
/// # use unlit_ecs::{World, bundle};
/// let mut world = World::new();
/// let entity = world.root_scope_mut().spawn(bundle!(1u32, 2.0f32, true));
/// assert!(world.root_scope().has::<u32>(entity));
/// assert!(world.root_scope().has::<f32>(entity));
/// assert!(world.root_scope().has::<bool>(entity));
///
/// // A tuple is one component.
/// let entity = world.root_scope_mut().spawn(bundle!((1u8, 2u8)));
/// assert!(world.root_scope().has::<(u8, u8)>(entity));
/// assert!(!world.root_scope().has::<u8>(entity));
/// ````
///
/// A bundle written as `..value` is unwrapped into its own components, the way
/// spawning it alone would. The syntax is needed because the macro walks the
/// source and cannot tell a bundle from a component; written plainly, a bundle
/// is one component.
///
/// ````rust
/// # use unlit_ecs::{ArchetypeBuilder, World, bundle};
/// let mut gathered = ArchetypeBuilder::new();
/// gathered.push_component(1u32);
/// let mut world = World::new();
/// let entity = world.root_scope_mut().spawn(bundle!(..gathered, true));
/// assert!(world.root_scope().has::<u32>(entity));
/// assert!(world.root_scope().has::<bool>(entity));
/// ````
///
/// The width is bounded by the compiler's recursion limit rather than by a
/// fixed count: each item costs a level of macro recursion, so roughly a
/// hundred components fit at the default limit of 128. A wider bundle is built
/// by raising `#![recursion_limit]` in the calling crate, or by pushing onto
/// an [`ArchetypeBuilder`] directly.
///
/// A type that appears twice panics when the bundle is spawned.
#[macro_export]
macro_rules! bundle {
    () => { $crate::ArchetypeBuilder::new() };
    // A bundle written as `..value`, unwrapped into its own components. It is
    // matched before the `$leaf:expr` arms because `..value` is not an
    // expression.
    (@list $builder:ident, ..$bundle:expr, $($rest:tt)*) => {
        $builder.push_bundle($bundle);
        $crate::bundle!(@list $builder, $($rest)*);
    };
    (@list $builder:ident, ..$bundle:expr) => { $builder.push_bundle($bundle) };
    // One component. It is matched opaquely, so a tuple is one component and a
    // group is never taken apart.
    (@list $builder:ident, $leaf:expr, $($rest:tt)*) => {
        $builder.push_component($leaf);
        $crate::bundle!(@list $builder, $($rest)*);
    };
    (@list $builder:ident,) => {};
    (@list $builder:ident, $leaf:expr) => { $builder.push_component($leaf) };
    ($($item:tt)+) => {{
        let mut builder = $crate::ArchetypeBuilder::new();
        $crate::bundle!(@list builder, $($item)+);
        builder
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::World;

    /// Gathering bundles keeps each one's own components: a bundle's leaves are
    /// its own, and a component pushed whole that is itself a tuple stays one
    /// component.
    #[test]
    fn push_bundle_gathers_bundles() {
        let mut world = World::new();
        let mut builder = ArchetypeBuilder::new();
        builder.push_component(1u32);
        builder.push_bundle(crate::bundle!(2.0f32, true));
        builder.push_component((7u8, 8u8));
        let entity = world.root_scope_mut().spawn(builder);
        assert_eq!(
            world.root_scope().get::<u32>(entity).map(|value| *value),
            Some(1)
        );
        assert_eq!(
            world.root_scope().get::<f32>(entity).map(|value| *value),
            Some(2.0)
        );
        assert_eq!(
            world.root_scope().get::<bool>(entity).map(|value| *value),
            Some(true)
        );
        // The pair went in as one component, the way spawning it alone would
        // have.
        assert_eq!(
            world
                .root_scope()
                .get::<(u8, u8)>(entity)
                .map(|value| *value),
            Some((7, 8))
        );
    }

    /// A duplicate is reported by the call that finishes the bundle, whether it
    /// was written twice in one list or gathered from a spread.
    #[test]
    #[should_panic(expected = "same component twice")]
    fn a_duplicate_panics_when_the_bundle_is_finished() {
        let mut builder = ArchetypeBuilder::new();
        builder.push_component(1u32);
        builder.push_bundle(crate::bundle!(2u32));
        builder.finish();
    }
}
