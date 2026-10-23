//! The bundle! macro: every item is one component, and ..value spreads a bundle.

use unlit_ecs::{ArchetypeBuilder, Bundle, Commands, Entity, Has, World, bundle};

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

/// The component set of the archetype an entity lives in.
fn types_of(world: &World, entity: Entity) -> Vec<core::any::TypeId> {
    let location = world.root_scope().location(entity).unwrap();
    world
        .root_scope()
        .archetype(location.archetype())
        .unwrap()
        .types()
        .to_vec()
}

#[test]
fn a_flat_list_spawns_every_component() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![
        Position { x: 1.0, y: 2.0 },
        Velocity { dx: 3.0, dy: 4.0 },
    ]);

    assert_eq!(
        world.root_scope().query_one::<&Position>(entity).unwrap().x,
        1.0
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Velocity>(entity)
            .unwrap()
            .dy,
        4.0
    );
}

#[test]
fn a_parenthesised_group_is_one_component() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![
        Position { x: 1.0, y: 2.0 },
        (Velocity { dx: 3.0, dy: 4.0 }, 7u32),
    ]);

    let nested = world
        .root_scope()
        .query_one::<&(Velocity, u32)>(entity)
        .unwrap();
    assert_eq!(nested.0.dy, 4.0);
    assert_eq!(nested.1, 7);
    assert!(
        !world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false),
        "the group is one component, so its leaves are not components"
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<Velocity>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn a_tuple_pushed_whole_is_one_component() {
    let mut world = World::new();
    let mut builder = ArchetypeBuilder::new();
    builder.push_component((1u32, 2.0f32));
    builder.push_component(true);
    let entity = world.root_scope_mut().spawn(builder);

    assert!(
        world
            .root_scope()
            .query_one::<Has<(u32, f32)>>(entity)
            .unwrap_or(false),
        "the tuple itself is a component"
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false),
        "a builder never takes a component apart"
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<bool>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn an_empty_tuple_is_a_component() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![(), 1u32]);

    assert!(
        world
            .root_scope()
            .query_one::<Has<()>>(entity)
            .unwrap_or(false),
        "the empty tuple is written, so it is a component"
    );
    assert_eq!(*world.root_scope().query_one::<&u32>(entity).unwrap(), 1);
}

#[test]
fn the_macro_returns_a_bundle_that_spawns_nothing() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![]);

    assert!(world.root_scope().contains(entity));
    assert!(
        !world
            .root_scope()
            .query_one::<Has<()>>(entity)
            .unwrap_or(false),
        "an empty list contributes no component at all"
    );
}

#[test]
fn a_group_holding_one_expression_is_that_expression_as_a_component() {
    let mut world = World::new();
    let value = 4u32;
    let entity = world
        .root_scope_mut()
        .spawn(bundle![(value,), (value as u64,)]);

    assert!(
        world
            .root_scope()
            .query_one::<Has<(u32,)>>(entity)
            .unwrap_or(false),
        "a one-element tuple stays a tuple"
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<(u64,)>>(entity)
            .unwrap_or(false)
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false)
    );
}

/// Distinct components for the width test: a plain tuple runs out at sixteen.
macro_rules! width_markers {
    ($($name:ident),*) => { $(#[derive(Clone, Copy, Debug)] struct $name;)* };
}
width_markers!(M00, M01, M02, M03, M04, M05, M06, M07, M08);

#[test]
fn more_than_sixteen_components() {
    let mut world = World::new();
    // Twenty-three distinct components, wider than any tuple the query items
    // cover, which the macro is not bound by.
    let entity = world.root_scope_mut().spawn(bundle![
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
    ]);

    let location = world.root_scope().location(entity).unwrap();
    let archetype = world.root_scope().archetype(location.archetype()).unwrap();
    assert_eq!(archetype.types().len(), 24, "23 components plus ScopeOf");
    assert!(
        world
            .root_scope()
            .query_one::<Has<M08>>(entity)
            .unwrap_or(false),
        "a leaf past the tuple limit landed"
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<Position>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn a_parenthesised_bundle_matches_the_equivalent_composite() {
    let mut written_world = World::new();
    let written = written_world
        .root_scope_mut()
        .spawn(bundle![(1u32, 2.0f32), (true,)]);

    let mut pushed_world = World::new();
    let mut builder = ArchetypeBuilder::new();
    builder.push_component((1u32, 2.0f32));
    builder.push_component((true,));
    let pushed = pushed_world.root_scope_mut().spawn(builder);

    assert_eq!(
        types_of(&written_world, written),
        types_of(&pushed_world, pushed),
        "same set, same archetype layout"
    );
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_duplicate_panics() {
    let mut world = World::new();
    world.root_scope_mut().spawn(bundle![1u32, 1u32]);
}

#[test]
fn a_queued_spawn_applies() {
    let mut world = World::new();
    let entity = {
        let commands: Commands<'_> = world.root_scope().queue();
        commands.spawn(bundle![(1u32, 2.0f32), true])
    };

    assert!(!world.root_scope().contains(entity), "queued, not spawned");
    world.apply().unwrap();

    assert!(
        world
            .root_scope()
            .query_one::<Has<(u32, f32)>>(entity)
            .unwrap_or(false)
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<bool>>(entity)
            .unwrap_or(false)
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn a_built_bundle_is_a_bundle() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);
    builder.push_component(2.0f32);

    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(builder);

    assert!(
        world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false)
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<f32>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn into_builder_returns_exactly_the_bundles_components() {
    let builder = bundle![(1u32, 2.0f32), true].into_builder();

    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(builder);

    assert_eq!(
        types_of(&world, entity).len(),
        3,
        "2 components plus ScopeOf"
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<(u32, f32)>>(entity)
            .unwrap_or(false)
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<bool>>(entity)
            .unwrap_or(false)
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn a_builder_is_spread_with_a_dot_dot() {
    let mut gathered = ArchetypeBuilder::new();
    gathered.push_component(Position { x: 1.0, y: 2.0 });
    gathered.push_component(7u32);

    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![..gathered, Velocity { dx: 3.0, dy: 4.0 }]);

    assert_eq!(
        world.root_scope().query_one::<&Position>(entity).unwrap().y,
        2.0
    );
    assert_eq!(*world.root_scope().query_one::<&u32>(entity).unwrap(), 7);
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Velocity>(entity)
            .unwrap()
            .dy,
        4.0
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<ArchetypeBuilder>>(entity)
            .unwrap_or(false),
        "the builder is not stored as a component"
    );
}

#[test]
fn a_custom_bundle_is_spread_with_a_dot_dot() {
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
        .spawn(bundle![..Pose(Position { x: 5.0, y: 6.0 }), true]);

    assert_eq!(
        world.root_scope().query_one::<&Position>(entity).unwrap().x,
        5.0
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<bool>>(entity)
            .unwrap_or(false)
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<Pose>>(entity)
            .unwrap_or(false),
        "the bundle is not stored as a component"
    );
}

#[test]
fn a_bundle_spread_keeps_a_tuple_component_whole() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);
    builder.push_component((2.0f32, true));

    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![..builder]);

    assert!(
        world
            .root_scope()
            .query_one::<Has<(f32, bool)>>(entity)
            .unwrap_or(false)
    );
    assert_eq!(*world.root_scope().query_one::<&u32>(entity).unwrap(), 1);
    assert!(
        !world
            .root_scope()
            .query_one::<Has<f32>>(entity)
            .unwrap_or(false)
    );
}

#[test]
fn a_queued_spread_bundle_applies() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);

    let mut world = World::new();
    let entity = {
        let commands: Commands<'_> = world.root_scope().queue();
        commands.spawn(bundle![..builder, 2.0f32])
    };

    assert!(!world.root_scope().contains(entity), "queued, not spawned");
    world.apply().unwrap();

    assert!(
        world
            .root_scope()
            .query_one::<Has<u32>>(entity)
            .unwrap_or(false)
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<f32>>(entity)
            .unwrap_or(false)
    );
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_duplicate_across_a_spread_panics() {
    let mut builder = ArchetypeBuilder::new();
    builder.push_component(1u32);

    let mut world = World::new();
    world.root_scope_mut().spawn(bundle![1u32, ..builder]);
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
