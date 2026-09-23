//! Querying entities through the public API.

use unlit_ecs::{Entity, LocalWorld, Or, With, Without};

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

fn scene() -> (LocalWorld, Entity, Entity, Entity) {
    let mut world = LocalWorld::new();
    // Moving and visible.
    let mover = world.spawn((
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 1.0, dy: 0.0 },
        Visible,
    ));
    // Moving but hidden.
    let hidden = world.spawn((
        Position { x: 10.0, y: 0.0 },
        Velocity { dx: 2.0, dy: 0.0 },
        Hidden,
    ));
    // Visible but still.
    let still = world.spawn((Position { x: 20.0, y: 0.0 }, Visible));
    (world, mover, hidden, still)
}

#[test]
fn a_single_component_query_visits_every_holder() {
    let (world, _, _, _) = scene();

    let count = world.query::<&Position>().count();
    assert_eq!(count, 3);
    assert_eq!(world.query::<&Velocity>().count(), 2);
    assert_eq!(world.query::<&Visible>().count(), 2);
}

#[test]
fn a_tuple_query_requires_every_component() {
    let (world, mover, hidden, _) = scene();

    let mut found: Vec<Entity> = world
        .query::<(&Position, &Velocity)>()
        .map(|(entity, _)| entity)
        .collect();
    found.sort();

    let mut expected = vec![mover, hidden];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn a_query_yields_the_entity_and_its_components() {
    let (world, mover, _, _) = scene();

    let (entity, (mut position, velocity)) =
        world.query::<(&mut Position, &Velocity)>().next().unwrap();
    position.x += velocity.dx;
    drop((position, velocity));

    // The first visited archetype is the one created first.
    assert_eq!(entity, mover);
    assert_eq!(world.get::<Position>(mover).unwrap().x, 1.0);
}

#[test]
fn a_query_can_fetch_the_entity_alone() {
    let (world, _, _, _) = scene();

    let entities: Vec<Entity> = world.query::<Entity>().map(|(entity, _)| entity).collect();
    assert_eq!(entities.len(), 3);
}

#[test]
fn mutable_components_are_written_through_a_query() {
    let (world, mover, hidden, still) = scene();

    world.for_each::<(&mut Position, &Velocity), _>(|(mut position, velocity)| {
        position.x += velocity.dx;
        position.y += velocity.dy;
    });

    assert_eq!(world.get::<Position>(mover).unwrap().x, 1.0);
    assert_eq!(world.get::<Position>(hidden).unwrap().x, 12.0);
    assert_eq!(world.get::<Position>(still).unwrap().x, 20.0, "no velocity");
}

#[test]
fn optional_components_are_none_when_missing() {
    let (world, mover, _, still) = scene();

    let seen: Vec<(Entity, Option<f32>)> = world
        .query::<(&Position, Option<&Velocity>)>()
        .map(|(entity, (_, velocity))| (entity, velocity.map(|v| v.dx)))
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_component_alone_yields_none_for_holders_without_it() {
    let (world, mover, hidden, still) = scene();

    // `Option<&Velocity>` matches every archetype, so all three entities are
    // visited; the one without a velocity yields `None`.
    let seen: Vec<(Entity, Option<f32>)> = world
        .query::<Option<&Velocity>>()
        .map(|(entity, velocity)| (entity, velocity.map(|v| v.dx)))
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(hidden, Some(2.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_mutable_component_writes_only_where_present() {
    let (world, mover, hidden, still) = scene();

    world.for_each::<(Option<&mut Velocity>,), _>(|(velocity,)| {
        if let Some(mut velocity) = velocity {
            velocity.dx += 10.0;
        }
    });

    assert_eq!(world.get::<Velocity>(mover).unwrap().dx, 11.0);
    assert_eq!(world.get::<Velocity>(hidden).unwrap().dx, 12.0);
    assert!(
        world.get::<Velocity>(still).is_none(),
        "the still entity has no velocity to write"
    );
}

#[test]
fn an_optional_component_composes_with_a_filter() {
    let (world, mover, _, still) = scene();

    // The filter visits only the visible entities; one of them has a velocity
    // and the other does not.
    let seen: Vec<(Entity, Option<f32>)> = world
        .query_filtered::<Option<&Velocity>, With<Visible>>()
        .map(|(entity, velocity)| (entity, velocity.map(|v| v.dx)))
        .collect();

    assert_eq!(seen.len(), 2);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn with_requires_a_component_without_fetching_it() {
    let (world, mover, _, _) = scene();

    // Only the mover has both: the hidden entity is not visible, and the still
    // one does not move.
    let found: Vec<Entity> = world
        .query_filtered::<&Velocity, With<Visible>>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [mover]);
}

#[test]
fn without_forbids_a_component() {
    let (world, _, _, still) = scene();

    let found: Vec<Entity> = world
        .query_filtered::<&Position, Without<Velocity>>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [still]);
}

#[test]
fn filters_compose_in_a_tuple() {
    let (world, _, hidden, _) = scene();

    // A tuple is the conjunction: both `Velocity` and `Hidden`.
    let found: Vec<Entity> = world
        .query_filtered::<&Position, (With<Velocity>, With<Hidden>)>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [hidden]);
}

#[test]
fn or_takes_the_union_of_its_filters() {
    let (world, mover, hidden, _) = scene();

    let found: Vec<Entity> = world
        .query_filtered::<&Position, Or<(With<Velocity>, With<Hidden>)>>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found.len(), 2);
    assert!(found.contains(&mover));
    assert!(found.contains(&hidden));
}

#[test]
fn a_query_over_entities_with_no_matches_is_empty() {
    /// No entity in the scene was spawned with this component.
    #[derive(Clone, Copy, Debug)]
    struct Unused;

    let (world, _, _, _) = scene();
    assert_eq!(world.query::<&Unused>().count(), 0);
}

#[test]
fn iteration_is_deterministic_between_runs() {
    let (world, _, _, _) = scene();

    let first: Vec<Entity> = world
        .query::<&Position>()
        .map(|(entity, _)| entity)
        .collect();
    let second: Vec<Entity> = world
        .query::<&Position>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(first, second);
}

#[test]
fn a_query_skips_an_empty_archetype() {
    let mut world = LocalWorld::new();
    // Creating and emptying an archetype must not confuse iteration.
    let entity = world.spawn((Position { x: 0.0, y: 0.0 }, Velocity { dx: 1.0, dy: 1.0 }));
    world.despawn(entity);
    world.spawn((Position { x: 5.0, y: 5.0 },));

    let mut found = world.query::<(&Position, &Velocity)>();
    assert!(found.next().is_none());
    assert_eq!(world.query::<&Position>().count(), 1);
}

#[test]
#[should_panic(expected = "already borrowed")]
fn asking_for_the_same_component_twice_panics() {
    let mut world = LocalWorld::new();
    world.spawn((Position { x: 0.0, y: 0.0 },));

    // Both items stay alive, so the second exclusive borrow must fail.
    let items: Vec<_> = world.query::<(&mut Position, &mut Position)>().collect();
    drop(items);
}

#[test]
#[should_panic(expected = "already borrowed")]
fn reading_a_component_while_it_is_written_panics() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((Position { x: 0.0, y: 0.0 },));

    let _writing = world.get_mut::<Position>(entity).unwrap();
    let _ = world.get::<Position>(entity);
}
