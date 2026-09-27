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
    let entity = world.spawn(bundle!(
        Position { x: 1.0, y: 2.0 },
        Velocity { dx: 3.0, dy: 4.0 }
    ));

    assert_eq!(world.get::<Position>(entity).unwrap().x, 1.0);
    assert_eq!(world.get::<Velocity>(entity).unwrap().dy, 4.0);
}

#[test]
fn nesting_is_flattened_at_any_depth() {
    let mut world = World::new();
    let entity = world.spawn(bundle!(
        Position { x: 1.0, y: 2.0 },
        (Velocity { dx: 3.0, dy: 4.0 }, (7u32, (true, ()))),
    ));

    assert_eq!(*world.get::<u32>(entity).unwrap(), 7, "deep leaf");
    assert!(world.has::<bool>(entity));
    assert!(
        !world.has::<(u32, bool)>(entity),
        "the nested tuple is not stored as a component"
    );
    assert_eq!(world.get::<Position>(entity).unwrap().y, 2.0);
}

#[test]
fn a_nested_tuple_spawned_plainly_is_one_component() {
    let mut world = World::new();
    let entity = world.spawn(((1u32, 2.0f32), true));

    assert!(
        world.has::<(u32, f32)>(entity),
        "the tuple itself is a component"
    );
    assert!(!world.has::<u32>(entity), "not flattened without the macro");
    assert!(world.has::<bool>(entity));
}

#[test]
fn empty_groups_contribute_nothing() {
    let mut world = World::new();
    let entity = world.spawn(bundle!((), (), (1u32, ())));

    assert_eq!(world.len(), 1);
    assert_eq!(*world.get::<u32>(entity).unwrap(), 1);
    assert!(
        !world.has::<()>(entity),
        "the empty tuple is not a component here"
    );
}

#[test]
fn the_macro_returns_a_bundle_that_spawns_nothing() {
    let mut world = World::new();
    let entity = world.spawn(bundle!());

    assert!(world.contains(entity));
    assert!(!world.has::<u32>(entity));
}

#[test]
fn a_group_holding_one_expression_is_one_leaf() {
    let mut world = World::new();
    let value = 4u32;
    // Each group holds a single expression, so it is pushed as itself rather
    // than recursed into; the two are different types, so neither duplicates.
    let entity = world.spawn(bundle!((value,), (value as u64,)));

    assert_eq!(*world.get::<u32>(entity).unwrap(), 4);
    assert_eq!(*world.get::<u64>(entity).unwrap(), 4);
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
    let entity = world.spawn(bundle!(
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

    let location = world.location(entity).unwrap();
    let archetype = world.archetype(location.archetype()).unwrap();
    assert_eq!(archetype.types().len(), 23);
    assert!(
        world.has::<M08>(entity),
        "a leaf past the tuple limit landed"
    );
    assert!(world.has::<Position>(entity));
}

#[test]
fn a_flattened_bundle_matches_the_equivalent_tuple() {
    let mut flat = World::new();
    let mut nested = World::new();

    let flattened = flat.spawn(bundle!((1u32, 2.0f32), (true,)));
    let tupled = nested.spawn((1u32, 2.0f32, true));

    let flat_location = flat.location(flattened).unwrap();
    let flat_types = flat
        .archetype(flat_location.archetype())
        .unwrap()
        .types()
        .to_vec();
    let nested_location = nested.location(tupled).unwrap();
    let nested_types = nested
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
    world.spawn(bundle!(1u32, (1u32, true)));
}

#[test]
fn a_queued_flattened_spawn_applies() {
    let mut world = World::new();
    let entity = {
        let commands: Commands<'_> = world.queue();
        commands.spawn(bundle!((1u32, (2.0f32,)), true))
    };

    assert!(!world.contains(entity), "queued, not spawned");
    world.apply();

    assert!(world.has::<u32>(entity));
    assert!(world.has::<f32>(entity));
    assert!(world.has::<bool>(entity));
}

#[test]
fn a_built_bundle_is_a_bundle() {
    // The macro's output implements `Bundle`, so a caller can build one, pass it
    // around, and spawn it later.
    let mut builder = ArchetypeBuilder::new();
    builder.push(1u32);
    builder.push(2.0f32);

    let mut world = World::new();
    let entity = world.spawn(builder);

    assert!(world.has::<u32>(entity));
    assert!(world.has::<f32>(entity));
}

#[test]
fn into_builder_returns_exactly_the_bundles_components() {
    // `Bundle::into_builder` supplies the whole builder, so the components are
    // exactly the bundle's and nothing has to be kept or overwritten.
    let builder = bundle!((1u32, (2.0f32,)), true).into_builder();

    let mut world = World::new();
    let entity = world.spawn(builder);

    let location = world.location(entity).unwrap();
    let archetype = world.archetype(location.archetype()).unwrap();
    assert_eq!(archetype.types().len(), 3);
    assert_eq!(*world.get::<u32>(entity).unwrap(), 1);
    assert!(world.has::<f32>(entity));
    assert!(world.has::<bool>(entity));
}

#[test]
fn a_tuple_bundle_becomes_its_components() {
    let mut world = World::new();
    let entity = world.spawn((1u32, 2.0f32).into_builder());

    assert_eq!(*world.get::<u32>(entity).unwrap(), 1);
    assert_eq!(*world.get::<f32>(entity).unwrap(), 2.0);
}
