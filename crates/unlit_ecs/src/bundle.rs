//! Bundles: the set of components an entity is spawned with.
//!
//! A bundle is a tuple of components. Tuples of one to sixteen components
//! implement [`Bundle`], and so does the empty tuple `()`, which spawns an
//! entity with no components.
//!
//! Tuples do not flatten: a nested tuple is itself a component, so
//! `world.spawn(((a, b), c))` stores `(a, b)` as one component rather than as
//! `a` and `b`. Every `'static` type is a component, tuples included, which is
//! what keeps a blanket `Bundle` impl for single components and a recursive one
//! for tuples from coexisting. To flatten nesting, build the bundle with the
//! [`crate::bundle!`] macro.
//!
//! The same component cannot appear twice in a bundle; spawning with duplicates
//! panics.

use core::any::{Any, TypeId};

use crate::column::{AnyColumn, Column};

/// One component of a bundle, erased into the value box the archetype takes.
pub(crate) type ErasedValue = (TypeId, Box<dyn Any>);

/// A column constructor, keyed by the component type it builds for.
pub(crate) type ColumnCtor = (TypeId, fn() -> Box<dyn AnyColumn>);

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
    pub fn push<C: 'static>(&mut self, value: C) {
        self.values.push((TypeId::of::<C>(), Box::new(value)));
        self.ctors.push((TypeId::of::<C>(), Column::<C>::eraser()));
    }

    /// Sorted component types, the component values in the same order, and one
    /// column constructor per type.
    pub(crate) fn finish(self) -> FinishedColumns {
        let Self {
            mut values,
            mut ctors,
        } = self;
        values.sort_unstable_by_key(|(type_id, _)| *type_id);
        ctors.sort_unstable_by_key(|(type_id, _)| *type_id);
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
/// Implemented for the empty tuple, for tuples of one to sixteen components,
/// and for an [`ArchetypeBuilder`] the [`crate::bundle!`] macro produced.
pub trait Bundle {
    /// The builder holding exactly this bundle's components.
    ///
    /// The bundle supplies the whole builder, so there is no target to append
    /// to or overwrite; a caller that wants to gather several bundles calls
    /// [`ArchetypeBuilder::push`] for each component instead.
    fn into_builder(self) -> ArchetypeBuilder;
}

impl Bundle for () {
    fn into_builder(self) -> ArchetypeBuilder {
        ArchetypeBuilder::new()
    }
}

impl Bundle for ArchetypeBuilder {
    fn into_builder(self) -> ArchetypeBuilder {
        self
    }
}

macro_rules! impl_bundle {
    ($($name:ident),*) => {
        impl<$($name: 'static),*> Bundle for ($($name,)*) {
            #[expect(non_snake_case, reason = "the macro names bindings after the type parameters")]
            fn into_builder(self) -> ArchetypeBuilder {
                let ($($name,)*) = self;
                let mut builder = ArchetypeBuilder::new();
                $(builder.push::<$name>($name);)*
                builder
            }
        }
    };
}

// The type-parameter names run A..Q but skip O: a bare `O` reads like a zero
// in type lists and invites miscounting. The impls cover tuples up to sixteen
// components.
impl_bundle!(A);
impl_bundle!(A, B);
impl_bundle!(A, B, C);
impl_bundle!(A, B, C, D);
impl_bundle!(A, B, C, D, E);
impl_bundle!(A, B, C, D, E, F);
impl_bundle!(A, B, C, D, E, F, G);
impl_bundle!(A, B, C, D, E, F, G, H);
impl_bundle!(A, B, C, D, E, F, G, H, I);
impl_bundle!(A, B, C, D, E, F, G, H, I, J);
impl_bundle!(A, B, C, D, E, F, G, H, I, J, K);
impl_bundle!(A, B, C, D, E, F, G, H, I, J, K, L);
impl_bundle!(A, B, C, D, E, F, G, H, I, J, K, L, N);
impl_bundle!(A, B, C, D, E, F, G, H, I, J, K, L, N, O);
impl_bundle!(A, B, C, D, E, F, G, H, I, J, K, L, N, O, P);
impl_bundle!(A, B, C, D, E, F, G, H, I, J, K, L, N, O, P, Q);

/// Builds a bundle from components written as a tuple, flattening nesting.
///
/// A plain tuple spawn stores a nested tuple as a single component, because
/// every `'static` type is a component. This macro instead walks the syntax it
/// is given: every parenthesized group is unwrapped and its elements are added
/// on their own, at any depth, so the result is the flat set of leaves. Because
/// the walk happens while expanding, the components do not have to be a tuple
/// type at all, and a bundle can hold more than sixteen of them.
///
/// `````
/// # use unlit_ecs::{bundle, World};
/// let mut world = World::new();
/// let entity = world.spawn(bundle!((1u32, 2.0f32), (true, ())));
/// assert!(world.has::<u32>(entity));
/// assert!(world.has::<f32>(entity));
/// assert!(world.has::<bool>(entity));
/// `````
///
/// The width is bounded by the compiler's recursion limit rather than by a
/// fixed count: each item costs a level of macro recursion, so roughly a
/// hundred components fit at the default limit of 128. A wider bundle is built
/// by raising `#![recursion_limit]` in the calling crate.
///
/// Only the macro flattens: `world.spawn(((a, b), c))` still stores `(a, b)` as
/// one component. A group holding a single expression is likewise one
/// component, so `bundle!((value,))` and `bundle!(value)` are the same. A type
/// that appears twice anywhere in the macro panics when the bundle is spawned.
#[macro_export]
macro_rules! bundle {
    () => { $crate::ArchetypeBuilder::new() };
    // `@push` unwraps one group: it exists so that a group is flattened even
    // when nothing follows it. Without it a trailing group would fall through
    // to the `$leaf:expr` arms and be pushed as a single tuple-typed component.
    (@push $builder:ident, ()) => {};
    (@push $builder:ident, ($($inner:tt)*)) => {
        $crate::bundle!(@list $builder, $($inner)*);
    };
    // A group with items after it. A trailing comma is covered, so that
    // `bundle!((a, b),)` unwraps like `bundle!((a, b))`.
    (@list $builder:ident, ($($inner:tt)*), $($rest:tt)*) => {
        $crate::bundle!(@push $builder, ($($inner)*));
        $crate::bundle!(@list $builder, $($rest)*);
    };
    // A group at the end of the list.
    (@list $builder:ident, ($($inner:tt)*)) => {
        $crate::bundle!(@push $builder, ($($inner)*));
    };
    (@list $builder:ident, $leaf:expr, $($rest:tt)*) => {
        $builder.push($leaf);
        $crate::bundle!(@list $builder, $($rest)*);
    };
    (@list $builder:ident,) => {};
    (@list $builder:ident, $leaf:expr) => { $builder.push($leaf) };
    ($($item:tt)+) => {{
        let mut builder = $crate::ArchetypeBuilder::new();
        $crate::bundle!(@list builder, $($item)+);
        builder
    }};
}
