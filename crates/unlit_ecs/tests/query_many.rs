//! `Scope::query_many`: a list of entities checked whole, then visited in the
//! order it was given.

use unlit_ecs::{Entity, EntityAccessError, World, bundle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Value(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tag(&'static str);

/// Two archetypes: `Value` alone, and `Value` with a `Tag`. The entities of each
/// are spawned in turn, so the list can name them in any order.
fn fixture() -> (World, Vec<Entity>) {
    let mut world = World::new();
    let mut entities = Vec::new();
    for index in 0..6 {
        let entity = if index % 2 == 0 {
            world.root_scope_mut().spawn(bundle![Value(index)])
        } else {
            world
                .root_scope_mut()
                .spawn(bundle![Value(index), Tag("odd")])
        };
        entities.push(entity);
    }
    (world, entities)
}

fn collect_values(world: &World, entities: &[Entity]) -> Vec<u32> {
    let mut values = Vec::new();
    world
        .root_scope()
        .query_many::<&Value>(entities)
        .for_each(|value| values.push(value.0));
    values
}

#[test]
fn items_come_out_in_the_order_the_list_names_them() {
    let (world, entities) = fixture();
    let reversed: Vec<Entity> = entities.iter().rev().copied().collect();
    assert_eq!(collect_values(&world, &entities), [0, 1, 2, 3, 4, 5]);
    assert_eq!(collect_values(&world, &reversed), [5, 4, 3, 2, 1, 0]);
}

#[test]
fn an_empty_list_visits_nothing() {
    let (world, _) = fixture();
    assert!(collect_values(&world, &[]).is_empty());
}

#[test]
fn an_archetype_that_does_not_match_the_query_refuses_the_whole_list() {
    let (world, entities) = fixture();
    // The `Tag` is only on the odd entities, so the even ones do not match.
    let result = world
        .root_scope()
        .try_query_many::<(&Value, &Tag)>(&entities);
    assert!(matches!(
        result,
        Err(EntityAccessError::NotMatched { entity }) if entity == entities[0]
    ));
}

#[test]
fn a_dead_entity_refuses_the_whole_list_before_anything_is_handed_out() {
    let (mut world, entities) = fixture();
    world.root_scope_mut().despawn(entities[3]).unwrap();
    let result = world.root_scope().try_query_many::<&mut Value>(&entities);
    assert!(matches!(
        result,
        Err(EntityAccessError::NotAlive { entity }) if entity == entities[3]
    ));
}

#[test]
fn an_entity_outside_the_scope_refuses_the_list() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Tag("scene")]);
    let inside = world.scope_mut(scene).spawn(bundle![Value(1)]);
    let outside = world.root_scope_mut().spawn(bundle![Value(2)]);

    let scope = world.scope(scene);
    assert!(scope.try_query_many::<&Value>(&[inside]).is_ok());
    assert!(matches!(
        scope.try_query_many::<&Value>(&[inside, outside]),
        Err(EntityAccessError::NotVisible { entity, .. }) if entity == outside
    ));
}

#[test]
fn a_repeated_entity_refuses_the_list() {
    let (world, entities) = fixture();
    let result =
        world
            .root_scope()
            .try_query_many::<&Value>(&[entities[1], entities[4], entities[1]]);
    assert!(matches!(
        result,
        Err(EntityAccessError::Repeated { entity }) if entity == entities[1]
    ));
}

#[test]
fn writes_through_the_holder_reach_the_entities() {
    let (world, entities) = fixture();
    {
        let mut holder = world.root_scope().query_many::<&mut Value>(&entities);
        holder.for_each(|value| value.0 += 100);
    }
    let scope = world.root_scope();
    let values: Vec<u32> = entities
        .iter()
        .map(|&entity| scope.query_one::<&Value>(entity).get().0)
        .collect();
    assert_eq!(values, [100, 101, 102, 103, 104, 105]);
}

#[test]
#[should_panic(expected = "already borrowed")]
fn the_components_stay_claimed_while_the_holder_lives() {
    let (world, entities) = fixture();
    let _holder = world.root_scope().query_many::<&mut Value>(&entities);
    // A query claims when it starts visiting, so the conflict shows at `iter`.
    let _ = world.root_scope().query::<&Value>().iter().count();
}

#[test]
fn the_claims_are_released_with_the_holder() {
    let (world, entities) = fixture();
    drop(world.root_scope().query_many::<&mut Value>(&entities));
    assert_eq!(world.root_scope().query::<&mut Value>().iter().count(), 6);
}
