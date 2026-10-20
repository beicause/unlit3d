//! Querying entities through the public API.

use unlit_ecs::{Entity, Or, Query, With, Without, World};
use unlit_ecs::{bundle, query};

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

    let count = world.root_scope().query(query!(&Position)).count();
    assert_eq!(count, 3);
    assert_eq!(world.root_scope().query(query!(&Velocity)).count(), 2);
    assert_eq!(world.root_scope().query(query!(&Visible)).count(), 2);
}

#[test]
fn a_two_component_query_requires_every_component() {
    let (world, mover, hidden, _) = scene();

    let mut found: Vec<Entity> = world
        .root_scope()
        .query(query!(&Position, &Velocity))
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

    let (entity, row) = world
        .root_scope()
        .query(query!(&mut Position, &Velocity))
        .next()
        .unwrap();
    let velocity = row.read::<Velocity>().dx;
    row.write::<Position>().x += velocity;

    // The first visited archetype is the one created first.
    assert_eq!(entity, mover);
    assert_eq!(world.root_scope().get::<Position>(mover).unwrap().x, 1.0);
}

#[test]
fn a_query_can_fetch_the_entity_alone() {
    let (world, _, _, _) = scene();

    let entities: Vec<Entity> = world
        .root_scope()
        .query(query!())
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(entities.len(), 3);
}

#[test]
fn mutable_components_are_written_through_a_query() {
    let (world, mover, hidden, still) = scene();

    for (_, row) in world.root_scope().query(query!(&mut Position, &Velocity)) {
        let delta = (row.read::<Velocity>().dx, row.read::<Velocity>().dy);
        let mut position = row.write::<Position>();
        position.x += delta.0;
        position.y += delta.1;
    }

    assert_eq!(world.root_scope().get::<Position>(mover).unwrap().x, 1.0);
    assert_eq!(world.root_scope().get::<Position>(hidden).unwrap().x, 12.0);
    assert_eq!(
        world.root_scope().get::<Position>(still).unwrap().x,
        20.0,
        "no velocity"
    );
}

#[test]
fn optional_components_are_none_when_missing() {
    let (world, mover, _, still) = scene();

    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query(query!(&Position, Option<&Velocity>))
        .map(|(entity, row)| {
            let velocity = row.try_read::<Velocity>().map(|velocity| velocity.dx);
            (entity, velocity)
        })
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
        .root_scope()
        .query(query!(Option<&Velocity>))
        .map(|(entity, row)| {
            let velocity = row.try_read::<Velocity>().map(|velocity| velocity.dx);
            (entity, velocity)
        })
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(hidden, Some(2.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_mutable_component_writes_only_where_present() {
    let (world, mover, hidden, still) = scene();

    for (_, row) in world.root_scope().query(query!(Option<&mut Velocity>)) {
        if let Some(mut velocity) = row.try_write::<Velocity>() {
            velocity.dx += 10.0;
        }
    }

    assert_eq!(world.root_scope().get::<Velocity>(mover).unwrap().dx, 11.0);
    assert_eq!(world.root_scope().get::<Velocity>(hidden).unwrap().dx, 12.0);
    assert!(
        world.root_scope().get::<Velocity>(still).is_none(),
        "the still entity has no velocity to write"
    );
}

#[test]
fn an_optional_component_composes_with_a_filter() {
    let (world, mover, _, still) = scene();

    // The filter visits only the visible entities; one of them has a velocity
    // and the other does not.
    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query(query!(Option<&Velocity>; With<Visible>))
        .map(|(entity, row)| {
            let velocity = row.try_read::<Velocity>().map(|velocity| velocity.dx);
            (entity, velocity)
        })
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
        .root_scope()
        .query(query!(&Velocity; With<Visible>))
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [mover]);
}

#[test]
fn without_forbids_a_component() {
    let (world, _, _, still) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query(query!(&Position; Without<Velocity>))
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [still]);
}

#[test]
fn filters_compose_in_a_tuple() {
    let (world, _, hidden, _) = scene();

    // A tuple is the conjunction: both `Velocity` and `Hidden`.
    let found: Vec<Entity> = world
        .root_scope()
        .query(query!(&Position; (With<Velocity>, With<Hidden>)))
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [hidden]);
}

#[test]
fn or_takes_the_union_of_its_filters() {
    let (world, mover, hidden, _) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query(query!(&Position; Or<(With<Velocity>, With<Hidden>)>))
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
    assert_eq!(world.root_scope().query(query!(&Unused)).count(), 0);
}

#[test]
fn iteration_is_deterministic_between_runs() {
    let (world, _, _, _) = scene();

    let first: Vec<Entity> = world
        .root_scope()
        .query(query!(&Position))
        .map(|(entity, _)| entity)
        .collect();
    let second: Vec<Entity> = world
        .root_scope()
        .query(query!(&Position))
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

    let mut found = world.root_scope().query(query!(&Position, &Velocity));
    assert!(found.next().is_none());
    assert_eq!(world.root_scope().query(query!(&Position)).count(), 1);
}

#[test]
fn a_row_reports_which_components_it_holds() {
    let (world, _, _, still) = scene();

    // The still entity is the only one without a velocity.
    let (entity, row) = world
        .root_scope()
        .query(query!(&Position; Without<Velocity>))
        .next()
        .expect("the scene has a still entity");
    assert_eq!(entity, still);
    assert!(row.has::<Position>());
    assert!(!row.has::<Velocity>());
    assert!(row.has::<Visible>());
}

/// An optional component is optional about the component's absence, not about
/// the borrow: a conflict panics instead of yielding `None`, which would hide
/// a real conflict as a missing component.
#[test]
#[should_panic(expected = "already borrowed")]
fn an_optional_component_that_cannot_be_borrowed_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    // A borrow held elsewhere conflicts with the optional fetch of `Position`
    // instead of making it `None`.
    let _writing = world.root_scope().get_mut::<Position>(entity).unwrap();
    let query = Query::new().optional_read::<Position>();
    for (_, row) in world.root_scope().query(&query) {
        let _reading = row.try_read::<Position>();
    }
}

#[test]
#[should_panic(expected = "already borrowed")]
fn an_optional_mutable_component_that_cannot_be_borrowed_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    let _reading = world.root_scope().get::<Position>(entity).unwrap();
    let query = Query::new().optional_write::<Position>();
    for (_, row) in world.root_scope().query(&query) {
        let _writing = row.try_write::<Position>();
    }
}

/// `query!` rejects this at compile time; a query built at run time panics
/// instead.
#[test]
#[should_panic(expected = "exclusive twice")]
fn asking_for_the_same_component_twice_panics() {
    let _ = Query::new().write::<Position>().write::<Position>();
}

#[test]
#[should_panic(expected = "both shared and exclusive")]
fn mixing_a_shared_and_an_exclusive_fetch_panics() {
    let _ = Query::new().write::<Position>().read::<Position>();
}

#[test]
#[should_panic(expected = "already borrowed")]
fn reading_a_component_while_it_is_written_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle!(Position { x: 0.0, y: 0.0 },));

    let _writing = world.root_scope().get_mut::<Position>(entity).unwrap();
    let _ = world.root_scope().get::<Position>(entity);
}
