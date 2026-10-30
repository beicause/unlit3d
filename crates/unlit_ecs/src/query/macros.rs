//! The declarative front end for a query.
//!
//! Writing a query by hand takes three pieces: a struct of items, a type for the
//! plan, and an impl with two bodies. This macro derives all of it from one
//! declaration, so what the caller writes is the shape of one row:
//!
//! `~rust
//! # use unlit_ecs::{Entity, Has, World, bundle, query};
//! struct Health(pub i32);
//! struct Armor(pub i32);
//!
//! query! {
//!     /// One combatant, as the combat pass reads it.
//!     pub struct Combatant<'a> {
//!         pub entity: Entity,
//!         pub health: &'a mut Health,
//!         pub armor: &'a Armor,
//!     }
//! }
//!
//! let mut world = World::new();
//! let entity = world.root_scope_mut().spawn(bundle![Health(10), Armor(2)]);
//!
//! for row in world.root_scope().query::<Combatant>().iter() {
//!     row.health.0 -= row.armor.0;
//! }
//!
//! assert_eq!(world.root_scope().query_one::<&Health>(entity).unwrap().get().0, 8);
//! `~
//!
//! The lifetime is nominal: the items a query yields borrow from the holder,
//! never from the declaration, so `Combatant`, `Combatant<'static>` and
//! `Combatant<'_>` name the same query.
//!
//! # Fields
//!
//! | Field | Yields | Matches |
//! | --- | --- | --- |
//! | `&'a T` | `&'a T` | only archetypes carrying `T` |
//! | `&'a mut T` | `&'a mut T` | only archetypes carrying `T` |
//! | `Option<&'a T>` | `Option<&'a T>` | every archetype |
//! | `Option<&'a mut T>` | `Option<&'a mut T>` | every archetype |
//! | `Entity` | `Entity` | every archetype |
//! | `Has<T>` | `bool` | every archetype |
//!
//! A field whose type is none of these is reported at compile time, naming the
//! field. A component named twice with at least one of the two exclusive panics
//! when the query is iterated, the same as `(&T, &mut T)` does.
//!
//! # Visibility
//!
//! The struct takes the visibility it is declared with, and its fields may carry
//! attributes and their own visibility: `#[derive(Debug)]`, doc comments and
//! `pub(crate)` all travel as written.

/// Declare a query from the shape of one row.
///
/// See the [module documentation](self) for the fields it accepts.
#[macro_export]
macro_rules! query {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident<$lt:lifetime> {
            $($body:tt)*
        }
    ) => {
        $crate::__query_fields! {
            [builder row]
            [$(#[$meta])*]
            [$vis $name, $lt]
            []
            []
            []
            []
            []
            [$($body)*]
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __query_fields {
    // Every field has been rewritten: emit the item struct and the impl.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        []
    ) => {
        $($meta)*
        $vis struct $name<$lt> {
            $($field)*
        }

        impl<$lt> $crate::Query for $name<$lt> {
            type Item<'r> = $name<'r>;
            type Plan<'p> = ($($plan)*);

            fn prepare<'p>(
                $b: &mut $crate::PlanBuilder<'p>,
            ) -> ::core::option::Option<Self::Plan<'p>> {
                ::core::option::Option::Some(($($prep)*))
            }

            fn read<'r, 'p: 'r>(
                plan: &'r mut Self::Plan<'p>,
                $r: usize,
            ) -> Self::Item<'r> {
                #[expect(
                    unused_variables,
                    reason = "a query whose fields claim nothing still binds their plans"
                )]
                let ($($bind)*) = plan;
                $name { $($init)* }
            }
        }
    };

    // Option<&'a mut T>: an exclusive column that may be absent.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : Option<&$fl:lifetime mut $t:ty>, $($rest:tt)*]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)* ::core::option::Option<$crate::ColMut<'p, $t>>,]
            [$($prep)* $b.write::<$t>(),]
            [$($bind)* $f,]
            [$($field)* $(#[$fmeta])* $fv $f: ::core::option::Option<&$lt mut $t>,]
            [$($init)* $f: $f.as_mut().map(|column| column.at($r)),]
            [$($rest)*]
        }
    };

    // Option<&'a T>: a shared column that may be absent.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : Option<&$fl:lifetime $t:ty>, $($rest:tt)*]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)* ::core::option::Option<$crate::Col<'p, $t>>,]
            [$($prep)* $b.read::<$t>(),]
            [$($bind)* $f,]
            [$($field)* $(#[$fmeta])* $fv $f: ::core::option::Option<&$lt $t>,]
            [$($init)* $f: $f.as_ref().map(|column| column.at($r)),]
            [$($rest)*]
        }
    };

    // Has<T>: a yes/no question. Claims nothing, so it never narrows the query.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : Has<$t:ty>, $($rest:tt)*]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)* bool,]
            [$($prep)* $b.has::<$t>(),]
            [$($bind)* $f,]
            [$($field)* $(#[$fmeta])* $fv $f: bool,]
            [$($init)* $f: *$f,]
            [$($rest)*]
        }
    };

    // Entity: the row's own id.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : Entity, $($rest:tt)*]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)* &'p [$crate::Entity],]
            [$($prep)* $b.entities(),]
            [$($bind)* $f,]
            [$($field)* $(#[$fmeta])* $fv $f: $crate::Entity,]
            [$($init)* $f: $f[$r],]
            [$($rest)*]
        }
    };

    // &'a mut T: an exclusive column.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : &$fl:lifetime mut $t:ty, $($rest:tt)*]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)* $crate::ColMut<'p, $t>,]
            [$($prep)* $b.write::<$t>()?,]
            [$($bind)* $f,]
            [$($field)* $(#[$fmeta])* $fv $f: &$lt mut $t,]
            [$($init)* $f: $f.at($r),]
            [$($rest)*]
        }
    };

    // &'a T: a shared column.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : &$fl:lifetime $t:ty, $($rest:tt)*]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)* $crate::Col<'p, $t>,]
            [$($prep)* $b.read::<$t>()?,]
            [$($bind)* $f,]
            [$($field)* $(#[$fmeta])* $fv $f: &$lt $t,]
            [$($init)* $f: $f.at($r),]
            [$($rest)*]
        }
    };

    // A last field written without a trailing comma.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : $t:ty]
    ) => {
        $crate::__query_fields! {
            [$b $r]
            [$($meta)*]
            [$vis $name, $lt]
            [$($plan)*]
            [$($prep)*]
            [$($bind)*]
            [$($field)*]
            [$($init)*]
            [$(#[$fmeta])* $fv $f : $t ,]
        }
    };

    // A field whose type is none of the supported shapes.
    (
        [$b:ident $r:ident]
        [$($meta:tt)*]
        [$vis:vis $name:ident, $lt:lifetime]
        [$($plan:tt)*]
        [$($prep:tt)*]
        [$($bind:tt)*]
        [$($field:tt)*]
        [$($init:tt)*]
        [$(#[$fmeta:meta])* $fv:vis $f:ident : $t:ty $(, $($rest:tt)*)?]
    ) => {
        ::core::compile_error!(::core::concat!(
            "field `", ::core::stringify!($f), "` has an unsupported type; ",
            "a query field is `&'a T`, `&'a mut T`, `Option<&'a T>`, ",
            "`Option<&'a mut T>`, `Has<T>` or `Entity`",
        ));
    };
}
