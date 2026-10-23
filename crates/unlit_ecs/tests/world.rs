//! Spawning, reading, writing and despawning entities through the public API.

use std::cell::Cell;
use std::rc::Rc;

use unlit_ecs::bundle;
use unlit_ecs::{Entity, Has, World};

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Name(&'static str);

#[test]
fn a_spawned_entity_reads_back_its_components() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 1.0, y: 2.0 }, Name("cube")]);

    assert!(world.root_scope().contains(entity));
    assert_eq!(world.root_scope().len(), 1);
    assert!(!world.root_scope().is_empty());
    assert!(
        world
            .root_scope()
            .query_one::<Has<Position>>(entity)
            .unwrap_or(false)
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<Name>>(entity)
            .unwrap_or(false)
    );
    assert!(
        !world
            .root_scope()
            .query_one::<Has<Velocity>>(entity)
            .unwrap_or(false)
    );

    assert_eq!(
        world.root_scope().query_one::<&Position>(entity).unwrap().x,
        1.0
    );
    assert_eq!(
        world.root_scope().query_one::<&Name>(entity).unwrap().0,
        "cube"
    );
}

#[test]
fn a_fresh_world_is_empty() {
    let world = World::new();
    assert!(world.root_scope().is_empty());
    assert_eq!(world.root_scope().len(), 0);
}

#[test]
fn an_empty_entity_has_no_components() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn_empty();

    assert!(world.root_scope().contains(entity));
    assert!(
        !world
            .root_scope()
            .query_one::<Has<Position>>(entity)
            .unwrap_or(false)
    );
    assert!(world.root_scope().query_one::<&Position>(entity).is_none());
}

#[test]
fn components_are_written_through_a_scoped_borrow() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 1.0, dy: 2.0 },
    ]);

    // A behaviour reads the entity's own velocity while writing its position.
    // The two live in different cells, so the borrows do not conflict.
    let moved_x = world
        .root_scope()
        .query_one::<&mut Position>(entity)
        .map(|mut position| {
            let velocity = world.root_scope().query_one::<&Velocity>(entity).unwrap();
            position.x += velocity.dx;
            position.y += velocity.dy;
            position.x
        });

    assert_eq!(moved_x, Some(1.0));
    assert_eq!(
        world.root_scope().query_one::<&Position>(entity).unwrap().y,
        2.0
    );
}

#[test]
fn get_mut_holds_an_exclusive_borrow() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);

    {
        let mut position = world
            .root_scope()
            .query_one::<&mut Position>(entity)
            .unwrap();
        position.x = 5.0;
    }

    assert_eq!(
        world.root_scope().query_one::<&Position>(entity).unwrap().x,
        5.0
    );
}

#[test]
fn with_mut_is_none_without_the_component() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);
    assert!(
        world
            .root_scope()
            .query_one::<&mut Velocity>(entity)
            .map(|_| ())
            .is_none()
    );
}

#[test]
fn a_despawned_handle_never_resolves_to_the_next_entity() {
    let mut world = World::new();
    let first = world.root_scope_mut().spawn(bundle![Name("first")]);

    assert!(world.root_scope_mut().despawn(first));
    assert!(!world.root_scope().contains(first));
    assert!(world.root_scope().query_one::<&Name>(first).is_none());
    assert!(
        !world.root_scope_mut().despawn(first),
        "the second despawn is a no-op"
    );
    assert_eq!(world.root_scope().len(), 0);

    // The index is handed out again, but the handle is not: the generation
    // changed, so the old handle stays dead.
    let second = world.root_scope_mut().spawn(bundle![Name("second")]);
    assert_eq!(second.index(), first.index(), "the index is reused");
    assert_ne!(second, first, "but the handle is not");
    assert!(world.root_scope().contains(second));
    assert!(!world.root_scope().contains(first));
}

#[test]
fn despawning_drops_the_component_values() {
    struct Tracked(Rc<Cell<u32>>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let drops = Rc::new(Cell::new(0));
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Tracked(drops.clone())]);
    assert_eq!(drops.get(), 0);

    world.root_scope_mut().despawn(entity);
    assert_eq!(drops.get(), 1, "the component left with the entity");
}

#[test]
fn a_reserved_handle_becomes_live_only_when_spawned() {
    let mut world = World::new();
    let entity = world.reserve_entity();

    assert!(!world.root_scope().contains(entity));
    assert_eq!(world.root_scope().len(), 0);

    world
        .root_scope_mut()
        .spawn_at(entity, bundle![Name("late"), 7u32]);
    assert!(world.root_scope().contains(entity));
    assert_eq!(
        world
            .root_scope()
            .query_one::<&u32>(entity)
            .unwrap()
            .to_owned(),
        7
    );
    assert_eq!(
        world.root_scope().query_one::<&Name>(entity).unwrap().0,
        "late"
    );
}

#[test]
fn a_reserved_handle_can_be_released_without_spawning() {
    let mut world = World::new();
    let reserved = world.reserve_entity();

    assert!(world.release_entity(reserved));
    assert!(!world.release_entity(reserved), "already released");

    // Releasing returns the index to the pool.
    let entity = world.root_scope_mut().spawn_empty();
    assert_eq!(entity.index(), reserved.index());
}

#[test]
#[should_panic(expected = "is not a reserved handle")]
fn spawning_twice_on_one_handle_panics() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn_empty();
    world
        .root_scope_mut()
        .spawn_at(entity, bundle![Name("again")]);
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_bundle_cannot_repeat_a_component() {
    let mut world = World::new();
    world.root_scope_mut().spawn(bundle![1u32, 2u32]);
}

#[test]
fn a_handle_no_world_produced_refers_to_nothing() {
    let mut world = World::new();
    world.root_scope_mut().spawn(bundle![Name("real")]);

    let alien = Entity::from_raw(u32::MAX, u32::MAX);
    assert!(!world.root_scope().contains(alien));
    assert!(world.root_scope().query_one::<&Name>(alien).is_none());
    assert!(!world.root_scope_mut().despawn(alien));
}

#[test]
fn a_handle_round_trips_through_its_bits() {
    let handle = Entity::from_raw(42, 3);
    assert_eq!(handle.index(), 42);
    assert_eq!(handle.generation(), 3);
    assert_eq!(handle.to_bits(), (3u64 << 32) | 42);
}

#[test]
fn a_wide_bundle_spawns_every_component() {
    let mut world = World::new();
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
        Name("wide"),
    ]);

    assert_eq!(*world.root_scope().query_one::<&u8>(entity).unwrap(), 1);
    assert_eq!(*world.root_scope().query_one::<&u64>(entity).unwrap(), 4);
    assert_eq!(*world.root_scope().query_one::<&i64>(entity).unwrap(), 8);
    assert!(*world.root_scope().query_one::<&bool>(entity).unwrap());
    assert_eq!(*world.root_scope().query_one::<&char>(entity).unwrap(), 'x');
    assert_eq!(
        world
            .root_scope()
            .query_one::<&f64>(entity)
            .unwrap()
            .to_owned(),
        2.5
    );
    assert_eq!(
        world.root_scope().query_one::<&Name>(entity).unwrap().0,
        "wide"
    );
}

#[test]
fn entities_sharing_a_component_set_share_an_archetype() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }, Name("a")]);
    let before = world.archetypes().as_slice().len();

    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 1.0, y: 1.0 }, Name("b")]);
    assert_eq!(
        world.archetypes().as_slice().len(),
        before,
        "same set, same archetype"
    );

    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 2.0, y: 2.0 }]);
    assert_eq!(
        world.archetypes().as_slice().len(),
        before + 1,
        "a wider set is its own"
    );
}

#[test]
fn a_location_names_the_archetype_and_row_of_a_live_entity() {
    let mut world = World::new();
    let first = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 1.0, y: 0.0 }]);
    let second = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 2.0, y: 0.0 }]);
    let other = world
        .root_scope_mut()
        .spawn(bundle![Velocity { dx: 0.0, dy: 0.0 }]);

    let location = world.root_scope().location(first).expect("spawned");
    let archetype = world
        .root_scope()
        .archetype(location.archetype())
        .expect("the location names an existing archetype");
    assert_eq!(archetype.entities()[location.row()], first);

    // Entities of one component set share an archetype but not a row.
    let second_location = world.root_scope().location(second).expect("spawned");
    assert_eq!(second_location.archetype(), location.archetype());
    assert_ne!(second_location.row(), location.row());
    // A different component set is a different archetype.
    assert_ne!(
        world
            .root_scope()
            .location(other)
            .expect("spawned")
            .archetype(),
        location.archetype()
    );

    assert!(
        world.root_scope().archetype(u32::MAX).is_none(),
        "no such archetype"
    );
    assert!(
        world.archetypes().get(u32::MAX).is_none(),
        "the pool answers the same"
    );
}

#[test]
fn an_archetype_names_its_own_components() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }, Name("a")]);
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 1.0, y: 1.0 }, Name("b")]);

    let archetype = world.root_scope().archetype_of(entity).expect("spawned");
    let mut names = archetype.names().to_vec();
    names.sort_unstable();
    let mut expected = vec![
        core::any::type_name::<Position>(),
        core::any::type_name::<Name>(),
        core::any::type_name::<unlit_ecs::ScopeOf>(),
    ];
    expected.sort_unstable();
    assert_eq!(names, expected);
    assert_eq!(
        archetype.names().len(),
        archetype.types().len(),
        "one name per type"
    );
    for (name, type_id) in archetype.names().iter().zip(archetype.types()) {
        assert_eq!(Some(*name), world.type_name(*type_id), "the two agree");
    }

    // The pool hands out the same archetype by id.
    let location = world.root_scope().location(entity).unwrap();
    let by_id = world
        .archetypes()
        .get(location.archetype())
        .expect("the location names one");
    assert_eq!(by_id.names(), archetype.names());
}

#[test]
fn a_despawned_entity_has_no_location() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);
    assert!(world.root_scope().location(entity).is_some());
    world.root_scope_mut().despawn(entity);
    assert!(world.root_scope().location(entity).is_none());
}

#[test]
fn grouping_entities_by_archetype_reads_the_same_components_as_get() {
    // This is the access shape the renderer's resolve pass uses: walk the
    // archetypes once, then fetch every row of each through one query.
    let mut world = World::new();
    let entities: Vec<Entity> = (0..8)
        .map(|i| {
            if i % 2 == 0 {
                world.root_scope_mut().spawn(bundle![Position {
                    x: i as f32,
                    y: 0.0,
                },])
            } else {
                world.root_scope_mut().spawn(bundle![
                    Position {
                        x: i as f32,
                        y: 0.0,
                    },
                    Velocity { dx: 1.0, dy: 2.0 },
                ])
            }
        })
        .collect();

    let resolved: Vec<(Entity, f32, Option<f32>)> = world
        .root_scope()
        .query::<(&Position, Option<&Velocity>, Entity)>()
        .map(|(position, option, entity)| {
            let position = position;
            let velocity = option.map(|velocity| velocity.dx);
            (entity, position.x, velocity)
        })
        .collect();
    assert_eq!(resolved.len(), entities.len());

    for &entity in &entities {
        let expected_x = world.root_scope().query_one::<&Position>(entity).unwrap().x;
        let expected_velocity = world
            .root_scope()
            .query_one::<&Velocity>(entity)
            .map(|velocity| velocity.dx);
        let found = resolved
            .iter()
            .find(|(candidate, _, _)| *candidate == entity)
            .expect("every entity is visited");
        assert_eq!(found.1, expected_x);
        assert_eq!(found.2, expected_velocity);
    }
}
