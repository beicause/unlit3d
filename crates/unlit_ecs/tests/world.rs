//! Spawning, reading, writing and despawning entities through the public API.

use std::cell::Cell;
use std::rc::Rc;

use unlit_ecs::{Entity, LocalWorld};

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
    let mut world = LocalWorld::new();
    let entity = world.spawn((Position { x: 1.0, y: 2.0 }, Name("cube")));

    assert!(world.contains(entity));
    assert_eq!(world.len(), 1);
    assert!(!world.is_empty());
    assert!(world.has::<Position>(entity));
    assert!(world.has::<Name>(entity));
    assert!(!world.has::<Velocity>(entity));

    assert_eq!(world.get::<Position>(entity).unwrap().x, 1.0);
    assert_eq!(world.get::<Name>(entity).unwrap().0, "cube");
}

#[test]
fn a_fresh_world_is_empty() {
    let world = LocalWorld::new();
    assert!(world.is_empty());
    assert_eq!(world.len(), 0);
}

#[test]
fn an_empty_entity_has_no_components() {
    let mut world = LocalWorld::new();
    let entity = world.spawn_empty();

    assert!(world.contains(entity));
    assert!(!world.has::<Position>(entity));
    assert!(world.get::<Position>(entity).is_none());
}

#[test]
fn components_are_written_through_a_scoped_borrow() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((Position { x: 0.0, y: 0.0 }, Velocity { dx: 1.0, dy: 2.0 }));

    // A behaviour reads the entity's own velocity while writing its position.
    // The two live in different cells, so the borrows do not conflict.
    let moved_x = world.with_mut::<Position, _>(entity, |position| {
        let velocity = world.get::<Velocity>(entity).unwrap();
        position.x += velocity.dx;
        position.y += velocity.dy;
        position.x
    });

    assert_eq!(moved_x, Some(1.0));
    assert_eq!(world.get::<Position>(entity).unwrap().y, 2.0);
}

#[test]
fn get_mut_holds_an_exclusive_borrow() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((Position { x: 0.0, y: 0.0 },));

    {
        let mut position = world.get_mut::<Position>(entity).unwrap();
        position.x = 5.0;
    }

    assert_eq!(world.get::<Position>(entity).unwrap().x, 5.0);
}

#[test]
fn with_mut_is_none_without_the_component() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((Position { x: 0.0, y: 0.0 },));
    assert!(world.with_mut::<Velocity, _>(entity, |_| ()).is_none());
}

#[test]
fn a_despawned_handle_never_resolves_to_the_next_entity() {
    let mut world = LocalWorld::new();
    let first = world.spawn((Name("first"),));

    assert!(world.despawn(first));
    assert!(!world.contains(first));
    assert!(world.get::<Name>(first).is_none());
    assert!(!world.despawn(first), "the second despawn is a no-op");
    assert_eq!(world.len(), 0);

    // The index is handed out again, but the handle is not: the generation
    // changed, so the old handle stays dead.
    let second = world.spawn((Name("second"),));
    assert_eq!(second.index(), first.index(), "the index is reused");
    assert_ne!(second, first, "but the handle is not");
    assert!(world.contains(second));
    assert!(!world.contains(first));
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
    let mut world = LocalWorld::new();
    let entity = world.spawn((Tracked(drops.clone()),));
    assert_eq!(drops.get(), 0);

    world.despawn(entity);
    assert_eq!(drops.get(), 1, "the component left with the entity");
}

#[test]
fn a_reserved_handle_becomes_live_only_when_spawned() {
    let mut world = LocalWorld::new();
    let entity = world.reserve_entity();

    assert!(!world.contains(entity));
    assert_eq!(world.len(), 0);

    world.spawn_at(entity, (Name("late"), 7u32));
    assert!(world.contains(entity));
    assert_eq!(world.get::<u32>(entity).unwrap().to_owned(), 7);
    assert_eq!(world.get::<Name>(entity).unwrap().0, "late");
}

#[test]
fn a_reserved_handle_can_be_released_without_spawning() {
    let mut world = LocalWorld::new();
    let reserved = world.reserve_entity();

    assert!(world.release_entity(reserved));
    assert!(!world.release_entity(reserved), "already released");

    // Releasing returns the index to the pool.
    let entity = world.spawn_empty();
    assert_eq!(entity.index(), reserved.index());
}

#[test]
#[should_panic(expected = "already spawned")]
fn spawning_twice_on_one_handle_panics() {
    let mut world = LocalWorld::new();
    let entity = world.spawn_empty();
    world.spawn_at(entity, (Name("again"),));
}

#[test]
#[should_panic(expected = "same component twice")]
fn a_bundle_cannot_repeat_a_component() {
    let mut world = LocalWorld::new();
    world.spawn((1u32, 2u32));
}

#[test]
fn the_placeholder_handle_refers_to_nothing() {
    let mut world = LocalWorld::new();
    world.spawn((Name("real"),));

    assert!(!world.contains(Entity::PLACEHOLDER));
    assert!(world.get::<Name>(Entity::PLACEHOLDER).is_none());
    assert!(!world.despawn(Entity::PLACEHOLDER));
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
    let mut world = LocalWorld::new();
    let entity = world.spawn((
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
    ));

    assert_eq!(*world.get::<u8>(entity).unwrap(), 1);
    assert_eq!(*world.get::<u64>(entity).unwrap(), 4);
    assert_eq!(*world.get::<i64>(entity).unwrap(), 8);
    assert!(*world.get::<bool>(entity).unwrap());
    assert_eq!(*world.get::<char>(entity).unwrap(), 'x');
    assert_eq!(world.get::<f64>(entity).unwrap().to_owned(), 2.5);
    assert_eq!(world.get::<Name>(entity).unwrap().0, "wide");
}

#[test]
fn entities_sharing_a_component_set_share_an_archetype() {
    let mut world = LocalWorld::new();
    world.spawn((Position { x: 0.0, y: 0.0 }, Name("a")));
    let before = world.archetype_count();

    world.spawn((Position { x: 1.0, y: 1.0 }, Name("b")));
    assert_eq!(world.archetype_count(), before, "same set, same archetype");

    world.spawn((Position { x: 2.0, y: 2.0 },));
    assert_eq!(
        world.archetype_count(),
        before + 1,
        "a wider set is its own"
    );
}
