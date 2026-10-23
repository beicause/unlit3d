//! Bundles: the set of components an entity is spawned with.
//!
//! A bundle is built by the [`crate::bundle!`] macro or by an [`ArchetypeBuilder`]. It is
//! not a tuple: every `'static` type is a component, tuples included, so a bare
//! `(a, b)` handed to `spawn` names one component rather than two. Writing
//! `bundle![a, b]` is what names two.
//!
//! The macro takes its items one at a time and each item is one component, so a
//! parenthesised item is a tuple component rather than a list to unpack: a
//! bundle only spreads when it is written as `..value`.
//!
//! The same component cannot appear twice in a bundle; spawning with duplicates
//! panics.

use core::any::{Any, TypeId};

use crate::column::{AnyColumn, Column};

/// One component of a bundle: its type, the name and column constructor the
/// world registers it under, and the value box the archetype takes.
///
/// The type-keyed facts sit in one entry rather than in parallel arrays, so a
/// component's type is written once and the arrays cannot fall out of step
/// when they are sorted.
pub(crate) struct BundleComponent {
    pub(crate) type_id: TypeId,
    pub(crate) name: &'static str,
    pub(crate) column: fn() -> Box<dyn AnyColumn>,
    pub(crate) value: Box<dyn Any>,
}

/// The components of one bundle, sorted by type.
pub(crate) type Components = Vec<BundleComponent>;

/// Builds the components of one spawn; only [`Bundle`] implementations use it.
///
/// The builder also carries the name of the entity it will spawn, which by
/// default is the source position of the [`crate::bundle!`] that built it.
pub struct ArchetypeBuilder {
    components: Components,
    name: &'static str,
}

impl Default for ArchetypeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchetypeBuilder {
    /// An empty builder with no name, for one built by hand.
    ///
    /// [`crate::bundle!`] is what supplies a name, so a builder filled in by
    /// hand names its entity after nothing: give it one with
    /// [`ArchetypeBuilder::named`], or spawn it with
    /// [`ScopeMut::spawn_named`](crate::ScopeMut::spawn_named).
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
            name: "",
        }
    }

    /// Name the bundle, replacing the one [`crate::bundle!`] gave it.
    ///
    /// [`crate::bundle!`] names every bundle it builds after the source
    /// position that wrote it, and a spawn gives that name to the entity. A
    /// builder assembled at runtime has no such position to speak of, so it
    /// names itself here instead.
    #[must_use]
    pub fn named(self, name: &'static str) -> Self {
        Self {
            components: self.components,
            name,
        }
    }

    /// The name the spawned entity takes, empty when nobody named the bundle.
    pub(crate) const fn name(&self) -> &'static str {
        self.name
    }

    /// Add one component.
    ///
    /// The value is stored under its own type, so a tuple pushed here stays one
    /// component.
    pub fn push_component<C: 'static>(&mut self, value: C) {
        self.components.push(BundleComponent {
            type_id: TypeId::of::<C>(),
            name: core::any::type_name::<C>(),
            column: Column::<C>::eraser(),
            value: Box::new(value),
        });
    }

    /// Add every component of `bundle` to this builder.
    ///
    /// This is how a caller gathers several bundles into one spawn: the
    /// components keep their own types, so a bundle that holds a tuple
    /// contributes that tuple as a single component, exactly as spawning it
    /// alone would.
    pub fn push_bundle<B: Bundle>(&mut self, bundle: B) {
        self.components.extend(bundle.into_builder().components);
    }

    /// The components in type order.
    ///
    /// # Panics
    ///
    /// When the same component appears twice. The duplicate is caught here
    /// rather than at spawn so that every path into an archetype, queued or
    /// immediate, reports it from the call that wrote the bundle.
    #[track_caller]
    pub(crate) fn finish(self) -> Components {
        let mut components = self.components;
        sort_components(&mut components);
        components
    }
}

/// Put a bundle's components in type order, which is the order an archetype
/// stores its columns in.
///
/// # Panics
///
/// When the same component appears twice.
#[track_caller]
fn sort_components(components: &mut [BundleComponent]) {
    components.sort_unstable_by_key(|component| component.type_id);
    for pair in components.windows(2) {
        assert_ne!(
            pair[0].type_id, pair[1].type_id,
            "a bundle cannot contain the same component twice",
        );
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
    note = "`(a, b)` is a single component here: every `'static` type is one, tuples included. Write `bundle![a, b]` for two components."
)]
pub trait Bundle {
    /// The builder holding exactly this bundle's components.
    ///
    /// The bundle supplies the whole builder, so there is no target to append
    /// to or overwrite. A caller gathering several bundles into one spawn
    /// pushes each onto an [`ArchetypeBuilder`] with
    /// [`ArchetypeBuilder::push_bundle`].
    fn into_builder(self) -> ArchetypeBuilder;
}

impl Bundle for ArchetypeBuilder {
    fn into_builder(self) -> ArchetypeBuilder {
        self
    }
}

/// Builds a bundle from the components written between the brackets.
///
/// The items are components, separated by commas: `bundle![a, b]` is two
/// components. A list of one needs no comma, so `bundle![a]` is the single
/// component `a`. An item is one component however it is written, because every
/// `'static` type is one: `bundle![(a, b)]` names a single tuple component, and
/// `bundle![()]` a single empty-tuple one.
///
/// The macro is written with brackets, `bundle![a, b]`, throughout this crate
/// and its dependents. Rust accepts any of the three delimiters after the `!`,
/// so the brackets are a convention that keeps a bundle's items, which are not
/// call arguments, visibly distinct.
///
/// ````rust
/// # use unlit_ecs::{Has, World, bundle};
/// let mut world = World::new();
/// let entity = world.root_scope_mut().spawn(bundle![1u32, 2.0f32, true]);
/// assert!(world.root_scope().query_one::<Has<u32>>(entity).unwrap_or(false));
/// assert!(world.root_scope().query_one::<Has<f32>>(entity).unwrap_or(false));
/// assert!(world.root_scope().query_one::<Has<bool>>(entity).unwrap_or(false));
///
/// // A tuple is one component.
/// let entity = world.root_scope_mut().spawn(bundle![(1u8, 2u8)]);
/// assert!(world.root_scope().query_one::<Has<(u8, u8)>>(entity).unwrap_or(false));
/// assert!(!world.root_scope().query_one::<Has<u8>>(entity).unwrap_or(false));
/// ````
///
/// A bundle written as `..value` is unwrapped into its own components, the way
/// spawning it alone would. The syntax is needed because the macro walks the
/// source and cannot tell a bundle from a component; written plainly, a bundle
/// is one component.
///
/// ````rust
/// # use unlit_ecs::{ArchetypeBuilder, Has, World, bundle};
/// let mut gathered = ArchetypeBuilder::new();
/// gathered.push_component(1u32);
/// let mut world = World::new();
/// let entity = world.root_scope_mut().spawn(bundle![..gathered, true]);
/// assert!(world.root_scope().query_one::<Has<u32>>(entity).unwrap_or(false));
/// assert!(world.root_scope().query_one::<Has<bool>>(entity).unwrap_or(false));
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
    [] => { $crate::ArchetypeBuilder::new().named(concat!(file!(), ":", line!())) };
    // A bundle written as `..value`, unwrapped into its own components. It is
    // matched before the `$leaf:expr` arms because `..value` is not an
    // expression.
    [@list $builder:ident, ..$bundle:expr, $($rest:tt)*] => {
        $builder.push_bundle($bundle);
        $crate::bundle![@list $builder, $($rest)*];
    };
    [@list $builder:ident, ..$bundle:expr] => { $builder.push_bundle($bundle) };
    // One component. It is matched opaquely, so a tuple is one component and a
    // group is never taken apart.
    [@list $builder:ident, $leaf:expr, $($rest:tt)*] => {
        $builder.push_component($leaf);
        $crate::bundle![@list $builder, $($rest)*];
    };
    [@list $builder:ident,] => {};
    [@list $builder:ident, $leaf:expr] => { $builder.push_component($leaf) };
    [$($item:tt)+] => {{
        let mut builder = $crate::ArchetypeBuilder::new().named(concat!(file!(), ":", line!()));
        $crate::bundle![@list builder, $($item)+];
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
        builder.push_bundle(crate::bundle![2.0f32, true]);
        builder.push_component((7u8, 8u8));
        let entity = world.root_scope_mut().spawn(builder);
        assert_eq!(
            world
                .root_scope()
                .query_one::<&u32>(entity)
                .map(|value| *value),
            Ok(1)
        );
        assert_eq!(
            world
                .root_scope()
                .query_one::<&f32>(entity)
                .map(|value| *value),
            Ok(2.0)
        );
        assert_eq!(
            world
                .root_scope()
                .query_one::<&bool>(entity)
                .map(|value| *value),
            Ok(true)
        );
        // The pair went in as one component, the way spawning it alone would
        // have.
        assert_eq!(
            world
                .root_scope()
                .query_one::<&(u8, u8)>(entity)
                .map(|value| *value),
            Ok((7, 8))
        );
    }

    /// A duplicate is reported by the call that finishes the bundle, whether it
    /// was written twice in one list or gathered from a spread.
    #[test]
    #[should_panic(expected = "same component twice")]
    fn a_duplicate_panics_when_the_bundle_is_finished() {
        let mut builder = ArchetypeBuilder::new();
        builder.push_component(1u32);
        builder.push_bundle(crate::bundle![2u32]);
        builder.finish();
    }

    /// A wide bundle still finishes in type order, each component whole.
    #[test]
    fn a_wide_bundle_finishes_in_type_order() {
        let mut builder = ArchetypeBuilder::new();
        builder.push_component(1u8);
        builder.push_component(2u16);
        builder.push_component(3u32);
        builder.push_component(4u64);
        builder.push_component(5i8);
        builder.push_component(6i16);
        builder.push_component(7i32);
        builder.push_component(8i64);
        builder.push_component(9.0f32);
        builder.push_component(10.0f64);
        let finished = builder.finish();
        assert_eq!(finished.len(), 10);
        assert!(
            finished
                .windows(2)
                .all(|pair| pair[0].type_id < pair[1].type_id),
            "the components come out in type order",
        );
    }
}
