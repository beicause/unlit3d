//! The `bundle!` macro: flattening nested tuples into one component set.

use unlit_ecs::{ArchetypeBuilder, Bundle, Commands, World, bundle};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Position {
    x: f32,
    y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Velocity {
    dx: f32,
    dy: f32,
}

#[test]
fn a_flat_list_spawns_every_component() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle!(
        Position { x: 1.0, y: 2.0 },
        Velocity { dx: 3.0, dy: 4.0 }
    ));

    assert_eq!(world.root_scope().get::<Position>(entity).unwrap().x, 1.0);
    assert_eq!(world.root_scope().get::<Velocity>(entity).unwrap().dy, 4.0);
}

#[test]
fn nesting_is_flattened_at_any_depth() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle!(
        Position { x: 1.0, y: 2.0 },
        (Velocity { dx: 3.0, dy: 4.0 }, (7u32, (true, ()))),
    ));

    assert_eq!(
        *world.root_scope().get::<u32>(entity).unwrap(),
        7,
        "deep leaf"
    );
    assert!(world.root_scope().has::<bool>(entity));
    assert!(
        !world.root_scope().has::<(u32, bool)>(entity),
        "the nested tuple is not stored as a component"
    );
    assert_eq!(world.root_scope().get::<Position>(entity).unwrap().y, 2.0);
}

#[test]
fn a_tuple_pushed_whole_is_one_component() {
    let mut world = World::new();
    let mut builder = ArchetypeBuilder::new();
    builder.push_component((1u32, 2.0f32));
    builder.push_component(true);
    let entity = world.root_scope_mut().spawn(builder);

    assert!(
        world.root_scope().has::<(u32, f32)>(entity),
        "the tuple itself is a component"
    );
    assert!(
        !world.root_scope().has::<u32>(entity),
        "a builder never takes a component apart"
    );
    assert!(world.root_scope().has::<bool>(entity));
}

#[test]
fn empty_groups_contribute_nothing() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle!((), (), (1u32, ())));

    assert_eq!(world.root_scope().len(), 1);
    assert_eq!(*world.root_scope().get::<u32>(entity).unwrap(), 1);
    assert!(
        !world.root_scope().has::<()>(entity),
        "the empty tuple is not a component here"
    );
}

#[test]
fn the_macro_returns_a_bundle_that_spawns_nothing() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle!());

    assert!(world.root_scope().contains(entity));
    assert!(!world.root_scope().has::<u32>(entity));
}

#[test]
fn a_group_holding_one_expression_flattens_to_it() {
    let mut world = World::new();
    let value = 4u32;
    // Each group holds a single expression, so it contributes that expression;
    // the two are different types, so neither duplicates.
    let entity = world
        .root_scope_mut()
        .spawn(bundle!((value,), (value as u64,)));

    assert_eq!(*world.root_scope().get::<u32>(entity).unwrap(), 4);
    assert_eq!(*world.root_scope().get::<u64>(entity).unwrap(), 4);
}

/// Distinct components for the width test: a plain tuple runs out at sixteen.
macro_rules! width_markers {
    ($($name:ident),*) => { $(#[derive(Clone, Copy, Debug)] struct $name;)* };
}
width_markers!(M00, M01, M02, M03, M04, M05, M06, M07, M08);

#[test]
fn more_than_sixteen_components_flatten() {
    let mut world = World::new();
    // Twenty-two distinct components, wider than any tuple `Bundle` impl, which
    // the macro is not bound by.
    let entity = world.root_scope_mut().spawn(bundle!(
        1u8,
        2u16,
        3u32,
        4u64,
        5i8,
        6i16,
        7i32,
        8i64,
        true,
        'x',
        1.5f32,
        2.5f64,
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 0.0, dy: 0.0 },
        M00,
        M01,
        M02,
        M03,
        M04,
        M05,
        M06,
        M07,
        M08,
    ));

    let location = world.root_scope().location(entity).unwrap();
    let archetype = world.root_scope().archetype(location.archetype()).unwrap();
    assert_eq!(archetype.types().len(), 24, "23 components plus ScopeOf");
    assert!(
        world.root_scope().has::<M08>(entity),
        "a leaf past the tuple limit landed"
    );
    assert!(world.root_scope().has::<Position>(entity));
}

#[test]
fn a_flattened_bundle_matches_the_equivalent_tuple() {
    let mut flat = World::new();
    let mut nested = World::new();

    let flattened = flat
        .root_scope_mut()
        .spawn(bundle!((1u32, 2.0f32), (true,)));
    let tupled = nested.root_scope_mut().spawn(bundle!(1u32, 2.0f32, true));

    let flat_location = flat.root_scope().location(flattened).unwrap();
    let flat_types = flat
        .root_scope()
        .archetype(flat_location.archetype())
        .unwrap()
        .types()
        .to_vec();
    let nested_location = nested.root_scope().location(tupled).unwrap();
    let nested_types = nested
        .root_scope()
        .archetype(nested_location.archetype())
        .unwrap()
        .types()
        .to_vec();
    assert_eq!(flat_types, nested_types, "same set, same archetype layout");
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_duplicate_across_levels_panics() {
    let mut world = World::new();
    // The top-level tuple impl only compares siblings; flattening first is what
    // catches a duplicate buried in a nested group.
    world.root_scope_mut().spawn(bundle!(1u32, (1u32, true)));
}

#[test]
fn a_queued_flattened_spawn_applies() {
    let mut world = World::new();
    let entity = {
        let commands: Commands<'_> = world.root_scope().queue();
        commands.spawn(bundle!((1u32, (2.0f32,)), true))
    };

    assert!(!world.root_scope().contains(entity), "queued, not spawned");
    world.apply();

    assert!(world.root_scope().has::<u32>(entity));
    assert!(world.root_scope().has::<f32>(entity));
    assert!(world.root_scope().has::<bool>(entity));
}

#[test]
fn a_built_bundle_is_a_bundle() {
    // The macro's output implements `Bundle`, so a caller can build one, pass it
    // around, and spawn it later.
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);
    builder.push_component(2.0f32);

    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(builder);

    assert!(world.root_scope().has::<u32>(entity));
    assert!(world.root_scope().has::<f32>(entity));
}

#[test]
fn into_builder_returns_exactly_the_bundles_components() {
    // `Bundle::into_builder` supplies the whole builder, so the components are
    // exactly the bundle's and nothing has to be kept or overwritten.
    let builder = bundle!((1u32, (2.0f32,)), true).into_builder();

    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(builder);

    let location = world.root_scope().location(entity).unwrap();
    let archetype = world.root_scope().archetype(location.archetype()).unwrap();
    assert_eq!(archetype.types().len(), 4, "3 components plus ScopeOf");
    assert_eq!(*world.root_scope().get::<u32>(entity).unwrap(), 1);
    assert!(world.root_scope().has::<f32>(entity));
    assert!(world.root_scope().has::<bool>(entity));
}

#[test]
fn a_builder_is_flattened_with_a_dot_dot() {
    // A bundle written with `..` contributes its own components, so a builder
    // the macro produced lands as its flat leaves rather than as one component.
    let mut gathered = ArchetypeBuilder::new();
    gathered.push_component(Position { x: 1.0, y: 2.0 });
    gathered.push_component(7u32);

    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(..gathered, Velocity { dx: 3.0, dy: 4.0 }));

    assert_eq!(world.root_scope().get::<Position>(entity).unwrap().y, 2.0);
    assert_eq!(*world.root_scope().get::<u32>(entity).unwrap(), 7);
    assert_eq!(world.root_scope().get::<Velocity>(entity).unwrap().dy, 4.0);
    assert!(
        !world.root_scope().has::<ArchetypeBuilder>(entity),
        "the builder is not stored as a component"
    );
}

#[test]
fn a_custom_bundle_is_flattened_with_a_dot_dot() {
    struct Pose(Position);

    impl Bundle for Pose {
        fn into_builder(self) -> ArchetypeBuilder {
            let mut builder = ArchetypeBuilder::new();
            builder.push_component(self.0);
            builder.push_component(Velocity { dx: 0.0, dy: 1.0 });
            builder
        }
    }

    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(..Pose(Position { x: 5.0, y: 6.0 }), true));

    assert_eq!(world.root_scope().get::<Position>(entity).unwrap().x, 5.0);
    assert_eq!(world.root_scope().get::<Velocity>(entity).unwrap().dy, 1.0);
    assert!(world.root_scope().has::<bool>(entity));
    assert!(
        !world.root_scope().has::<Pose>(entity),
        "the bundle is not stored as a component"
    );
}

#[test]
fn a_bundle_spread_keeps_a_tuple_component_whole() {
    // The spread spawns the bundle exactly as spawning it alone would, so a
    // tuple a bundle holds stays one component instead of being flattened.
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);
    builder.push_component((2.0f32, true));

    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle!(..builder));

    assert!(world.root_scope().has::<(f32, bool)>(entity));
    assert_eq!(*world.root_scope().get::<u32>(entity).unwrap(), 1);
    assert!(!world.root_scope().has::<f32>(entity));
}

#[test]
fn a_spread_bundle_nests_inside_groups() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);

    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!((..builder, 2.0f32), (true,)));

    assert_eq!(*world.root_scope().get::<u32>(entity).unwrap(), 1);
    assert_eq!(*world.root_scope().get::<f32>(entity).unwrap(), 2.0);
    assert!(world.root_scope().has::<bool>(entity));
}

#[test]
fn a_queued_spread_bundle_applies() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);

    let mut world = World::new();
    let entity = {
        let commands: Commands<'_> = world.root_scope().queue();
        commands.spawn(bundle!(..builder, 2.0f32))
    };

    assert!(!world.root_scope().contains(entity), "queued, not spawned");
    world.apply();

    assert!(world.root_scope().has::<u32>(entity));
    assert!(world.root_scope().has::<f32>(entity));
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_duplicate_across_a_spread_panics() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);

    let mut world = World::new();
    world.root_scope_mut().spawn(bundle!(1u32, ..builder));
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_component_pushed_twice_panics() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);
    builder.push_component(2u32);

    let mut world = World::new();
    world.root_scope_mut().spawn(builder);
}
