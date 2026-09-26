//! Bundles: the set of components an entity is spawned with.
//!
//! A bundle is a tuple of components. Tuples of one to sixteen components
//! implement [`Bundle`], and so does the empty tuple `()`, which spawns an
//! entity with no components.
//!
//! The same component cannot appear twice in a bundle; spawning with duplicates
//! panics.

use core::any::{Any, TypeId};

use crate::mode::{Column, ColumnErase, Mode};

/// One component of a bundle, erased into the value box the archetype takes.
pub(crate) type ErasedValue = (TypeId, Box<dyn Any>);

/// A column constructor, keyed by the component type it builds for.
pub(crate) type ColumnCtor<M> = (TypeId, fn() -> Box<<M as Mode>::ErasedColumn>);

/// The component types, values and column constructors of one completed bundle.
pub(crate) type FinishedColumns<M> = (Box<[TypeId]>, Vec<ErasedValue>, Vec<ColumnCtor<M>>);

/// Builds the components of one spawn; only [`Bundle`] implementations use it.
pub struct ArchetypeBuilder<M: Mode> {
    values: Vec<ErasedValue>,
    ctors: Vec<ColumnCtor<M>>,
}

impl<M: Mode> Default for ArchetypeBuilder<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Mode> ArchetypeBuilder<M> {
    /// An empty builder.
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            ctors: Vec::new(),
        }
    }

    /// Add one component.
    pub fn push<C: 'static>(&mut self, value: C)
    where
        Column<M, C>: ColumnErase<M>,
    {
        self.values.push((TypeId::of::<C>(), Box::new(value)));
        self.ctors
            .push((TypeId::of::<C>(), Column::<M, C>::eraser()));
    }

    /// Sorted component types, the component values in the same order, and one
    /// column constructor per type.
    pub(crate) fn finish(self) -> FinishedColumns<M> {
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
        let types: Vec<TypeId> = values.iter().map(|(type_id, _)| *type_id).collect();
        (types.into_boxed_slice(), values, ctors)
    }
}

/// A set of components to spawn an entity with.
///
/// Implemented for the empty tuple and for tuples of one to sixteen components.
pub trait Bundle<M: Mode> {
    /// Insert the bundle's components into the builder.
    fn put_into(self, builder: &mut ArchetypeBuilder<M>);
}

impl<M: Mode> Bundle<M> for () {
    fn put_into(self, _builder: &mut ArchetypeBuilder<M>) {}
}

macro_rules! impl_bundle {
    ($($name:ident),*) => {
        impl<M: Mode, $($name: 'static),*> Bundle<M> for ($($name,)*)
        where
            $(Column<M, $name>: ColumnErase<M>,)*
        {
            #[expect(non_snake_case, reason = "the macro names bindings after the type parameters")]
            fn put_into(self, builder: &mut ArchetypeBuilder<M>) {
                let ($($name,)*) = self;
                $(builder.push::<$name>($name);)*
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
