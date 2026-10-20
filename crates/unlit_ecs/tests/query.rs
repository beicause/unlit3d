//! Querying entities through the public API.

use core::any::TypeId;

use unlit_ecs::{Archetype, Entity, Has, World, bundle};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Visible;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hidden;

fn scene() -> (World, Entity, Entity, Entity) {
    let mut world = World::new();
    // Moving and visible.
    let mover = world.root_scope_mut().spawn(bundle!(
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 1.0, dy: 0.0 },
        Visible,
    ));
    // Moving but hidden.
    let hidden = world.root_scope_mut().spawn(bundle!(
        Position { x: 10.0, y: 0.0 },
        Velocity { dx: 2.0, dy: 0.0 },
        Hidden,
    ));
    // Visible but still.
    let still = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 20.0, y: 0.0 }, Visible));
    (world, mover, hidden, still)
}

#[test]
fn a_single_component_query_visits_every_holder() {
    let (world, _, _, _) = scene();

    assert_eq!(world.root_scope().query::<&Position>().count(), 3);
    assert_eq!(world.root_scope().query::<&Velocity>().count(), 2);
    assert_eq!(world.root_scope().query::<&Visible>().count(), 2);
}

#[test]
fn the_scaffolding_the_archetypes_share_is_skipped_by_position() {
    let mut world = World::new();
    // Bare entities land in the same archetype as the two scaffolding roots, so
    // a query that visits it has to step over them without a per-row test.
    let first = world.root_scope_mut().spawn_empty();
    let second = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 1.0, y: 2.0 }));
    let third = world.root_scope_mut().spawn_empty();

    let mut found: Vec<Entity> = world.root_scope().query::<Entity>().collect();
    found.sort();
    let mut expected = vec![first, second, third];
    expected.sort();
    assert_eq!(found, expected);
    assert_eq!(world.root_scope().len(), 3);

    // The same archetype keeps the roots at the front while its content churns.
    assert!(world.root_scope_mut().despawn(first));
    assert_eq!(world.root_scope().query::<Entity>().count(), 2);
    assert_eq!(world.root_scope().len(), 2);

    // A query that names a component skips the archetype whole, so the bare
    // entities are neither visited nor reported as carrying it.
    assert_eq!(world.root_scope().query::<&Position>().count(), 1);
    assert_eq!(
        world
            .root_scope()
            .query::<Has<Position>>()
            .filter(|&has| has)
            .count(),
        1,
    );
}

#[test]
fn a_two_component_query_requires_every_component() {
    let (world, mover, hidden, _) = scene();

    let mut found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position, &Velocity)>()
        .map(|(entity, _, _)| entity)
        .collect();
    found.sort();

    let mut expected = vec![mover, hidden];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn a_query_yields_its_items_in_the_order_written() {
    let (world, mover, _, _) = scene();

    // The component is written first and the entity second, and that is the
    // order they come out in.
    let (position, entity) = world
        .root_scope()
        .query::<(&Position, Entity)>()
        .next()
        .unwrap();
    assert_eq!(entity, mover);
    assert_eq!(position.x, 0.0);
}

#[test]
fn a_query_can_fetch_the_entity_alone() {
    let (world, _, _, _) = scene();

    let entities: Vec<Entity> = world.root_scope().query::<Entity>().collect();
    assert_eq!(entities.len(), 3);
}

#[test]
fn mutable_components_are_written_through_a_query() {
    let (world, mover, hidden, still) = scene();

    for (mut position, velocity) in world.root_scope().query::<(&mut Position, &Velocity)>() {
        position.x += velocity.dx;
        position.y += velocity.dy;
    }

    assert_eq!(
        *world.root_scope().query_one::<&Position>(mover).unwrap(),
        Position { x: 1.0, y: 0.0 }
    );
    assert_eq!(
        *world.root_scope().query_one::<&Position>(hidden).unwrap(),
        Position { x: 12.0, y: 0.0 }
    );
    assert_eq!(
        *world.root_scope().query_one::<&Position>(still).unwrap(),
        Position { x: 20.0, y: 0.0 },
        "no velocity"
    );
}

#[test]
fn optional_components_are_none_when_missing() {
    let (world, mover, _, still) = scene();

    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query::<(Entity, &Position, Option<&Velocity>)>()
        .map(|(entity, _, velocity)| (entity, velocity.map(|velocity| velocity.dx)))
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_component_alone_yields_none_for_holders_without_it() {
    let (world, mover, hidden, still) = scene();

    // An optional item matches every archetype, so all three entities are
    // visited; the one without a velocity yields `None`.
    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query::<(Entity, Option<&Velocity>)>()
        .map(|(entity, velocity)| (entity, velocity.map(|velocity| velocity.dx)))
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(hidden, Some(2.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_mutable_component_writes_only_where_present() {
    let (world, mover, hidden, still) = scene();

    for mut velocity in world
        .root_scope()
        .query::<Option<&mut Velocity>>()
        .flatten()
    {
        velocity.dx += 10.0;
    }

    assert_eq!(
        world.root_scope().query_one::<&Velocity>(mover).unwrap().dx,
        11.0
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Velocity>(hidden)
            .unwrap()
            .dx,
        12.0
    );
    assert!(
        world.root_scope().query_one::<&Velocity>(still).is_none(),
        "the still entity has no velocity to write"
    );
}

#[test]
fn a_filter_narrows_which_archetypes_are_visited() {
    let (world, mover, _, still) = scene();

    // The filter visits only the visible archetypes; one of them has a velocity
    // and the other does not.
    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query::<(Entity, Option<&Velocity>)>()
        .by(|archetype: &Archetype| archetype.has::<Visible>())
        .map(|(entity, velocity)| (entity, velocity.map(|velocity| velocity.dx)))
        .collect();

    assert_eq!(seen.len(), 2);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn a_filter_can_forbid_a_component() {
    let (world, _, _, still) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| !archetype.has::<Velocity>())
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [still]);
}

#[test]
fn a_filter_can_require_two_components() {
    let (world, _, hidden, _) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| archetype.has::<Velocity>() && archetype.has::<Hidden>())
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [hidden]);
}

#[test]
fn a_filter_can_take_the_union_of_two_components() {
    let (world, mover, hidden, _) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| archetype.has::<Velocity>() || archetype.has::<Hidden>())
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found.len(), 2);
    assert!(found.contains(&mover));
    assert!(found.contains(&hidden));
}

#[test]
fn a_filter_can_read_a_component_set_known_only_at_run_time() {
    let (world, mover, _, _) = scene();

    // A component set assembled at run time: only the mover carries all three.
    let wanted = [
        TypeId::of::<Position>(),
        TypeId::of::<Velocity>(),
        TypeId::of::<Visible>(),
    ];
    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| {
            wanted
                .iter()
                .all(|type_id| archetype.types().contains(type_id))
        })
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [mover]);
}

#[test]
fn query_one_reads_a_component_the_filter_would_have_rejected() {
    let (world, mover, hidden, _) = scene();

    // A filter narrows archetypes, which is a whole-archetype decision; a
    // one-entity lookup asks about that entity's archetype only.
    for (entity, has_velocity) in world
        .root_scope()
        .query::<(Entity, Has<Velocity>)>()
        .by(|archetype: &Archetype| archetype.has::<Hidden>())
    {
        assert_eq!(entity, hidden);
        assert!(has_velocity);
    }
    assert!(
        world
            .root_scope()
            .query_one::<Has<Velocity>>(mover)
            .unwrap()
    );
}

#[test]
fn a_query_over_entities_with_no_matches_is_empty() {
    /// No entity in the scene was spawned with this component.
    #[derive(Clone, Copy, Debug)]
    struct Unused;

    let (world, _, _, _) = scene();
    assert_eq!(world.root_scope().query::<&Unused>().count(), 0);
}

#[test]
fn iteration_is_deterministic_between_runs() {
    let (world, _, _, _) = scene();

    let first: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .map(|(entity, _)| entity)
        .collect();
    let second: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(first, second);
}

#[test]
fn a_query_skips_an_empty_archetype() {
    let mut world = World::new();
    // Creating and emptying an archetype must not confuse iteration.
    let entity = world.root_scope_mut().spawn(bundle!(
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 1.0, dy: 1.0 }
    ));
    world.root_scope_mut().despawn(entity);
    world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 5.0, y: 5.0 },));

    let mut found = world.root_scope().query::<(&Position, &Velocity)>();
    assert!(found.next().is_none());
    assert_eq!(world.root_scope().query::<&Position>().count(), 1);
}

#[test]
fn has_reports_a_component_without_borrowing_it() {
    let (world, mover, _, still) = scene();

    // An exclusive borrow of the component held across the lookup is no
    // obstacle, which is what makes `Has` usable beside an exclusive item.
    let mut position = world
        .root_scope()
        .query_one::<&mut Position>(mover)
        .unwrap();
    assert!(
        world
            .root_scope()
            .query_one::<Has<Position>>(mover)
            .unwrap()
    );
    // `Has` narrows nothing, so a component the entity lacks answers `false`
    // rather than dropping the row.
    assert_eq!(
        world.root_scope().query_one::<Has<Velocity>>(still),
        Some(false)
    );
    position.x = 42.0;
}

#[test]
fn has_narrows_nothing_and_by_narrows_for_it() {
    let (world, _, _, _) = scene();

    // Every row is visited, including the one entity without a `Velocity`.
    assert_eq!(world.root_scope().query::<Has<Velocity>>().count(), 3);
    // Narrowing to the carriers is the predicate's job.
    assert_eq!(
        world
            .root_scope()
            .query::<Has<Velocity>>()
            .by(|archetype: &Archetype| archetype.has::<Velocity>())
            .count(),
        2
    );
}

#[test]
fn by_applies_to_the_archetype_being_visited() {
    let (world, _, _, _) = scene();

    // The filter decides an archetype at a time, and may be attached after the
    // first rows were taken: it narrows what is being visited too.
    let mut midway = world.root_scope().query::<&Position>();
    let taken = midway.next().is_some();
    assert!(taken);
    let narrowed = midway
        .by(|archetype: &Archetype| archetype.has::<Velocity>())
        .count();
    let whole = world
        .root_scope()
        .query::<&Position>()
        .by(|archetype: &Archetype| archetype.has::<Velocity>())
        .count();
    assert_eq!(narrowed, whole - 1);
}

#[test]
fn has_beside_a_component_is_not_a_duplicate() {
    let (world, _, _, _) = scene();

    // Two queries over the same component, written as two distinct types, are
    // fine: each borrows once.
    assert_eq!(
        world
            .root_scope()
            .query::<(&Position, Has<Position>)>()
            .count(),
        3
    );
}

#[test]
fn query_one_yields_the_row_of_one_entity() {
    let (world, mover, hidden, still) = scene();

    assert_eq!(
        *world.root_scope().query_one::<&Position>(mover).unwrap(),
        Position { x: 0.0, y: 0.0 }
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<(&Position, Entity)>(hidden)
            .unwrap()
            .1,
        hidden
    );
    assert!(world.root_scope().query_one::<&Velocity>(still).is_none());
    assert!(world.root_scope().query_one::<Has<Visible>>(mover).unwrap());
}

#[test]
fn query_one_answers_none_for_an_entity_out_of_reach() {
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle!(1u32));
    let child = world
        .root_scope_mut()
        .scope_mut(parent)
        .unwrap()
        .spawn(bundle!(2u32));

    let child_scope = world.root_scope().scope(child).unwrap();
    assert!(child_scope.query_one::<&u32>(child).is_some());
    assert!(
        child_scope.query_one::<&u32>(parent).is_none(),
        "a scope does not see upwards"
    );
}

/// An optional item is optional about the component's absence, not about the
/// borrow: a conflict panics instead of yielding `None`, which would hide a
/// real conflict as a missing component.
#[test]
#[should_panic(expected = "already borrowed")]
fn an_optional_component_that_cannot_be_borrowed_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    let _writing = world
        .root_scope()
        .query_one::<&mut Position>(entity)
        .unwrap();
    for _ in world.root_scope().query::<Option<&Position>>() {}
}

#[test]
#[should_panic(expected = "already borrowed")]
fn an_optional_mutable_component_that_cannot_be_borrowed_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    let _reading = world.root_scope().query_one::<&Position>(entity).unwrap();
    for _ in world.root_scope().query::<Option<&mut Position>>() {}
}

/// Type ids cannot be compared in a `const` context, so naming a component
/// twice is caught when the query is iterated.
#[test]
#[should_panic(expected = "cannot name")]
fn asking_for_the_same_component_twice_panics() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    for _ in world.root_scope().query::<(&Position, &Position)>() {}
}

#[test]
#[should_panic(expected = "cannot name")]
fn mixing_a_shared_and_an_exclusive_fetch_panics() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    for _ in world.root_scope().query::<(&Position, &mut Position)>() {}
}

#[test]
#[should_panic(expected = "already borrowed")]
fn reading_a_component_while_it_is_written_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    let _writing = world
        .root_scope()
        .query_one::<&mut Position>(entity)
        .unwrap();
    let _ = world.root_scope().query_one::<&Position>(entity);
}
