//! Singleton keys: a key a bundle declares is held by one live entity per world.

use std::cell::Cell;
use std::rc::Rc;

use unlit_ecs::bundle;
use unlit_ecs::{Entity, EntityAccessError, Has, OnDespawn, World};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Name(&'static str);

/// Keys are types that are never stored: the type names the singleton.
struct Tasks;
struct Other;

/// The error a refused `Tasks` spawn answers with.
fn taken() -> EntityAccessError {
    EntityAccessError::SingletonTaken {
        key: std::any::type_name::<Tasks>(),
    }
}

/// A world with a scene and a sibling of the scene, both under the root.
fn fixture() -> (World, Entity, Entity) {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    let sibling = world.root_scope_mut().spawn(bundle![Name("other")]);
    (world, scene, sibling)
}

#[test]
fn the_holder_is_found_from_the_root_and_adds_no_component() {
    let mut world = World::new();
    let tasks = world
        .root_scope_mut()
        .spawn(bundle![Name("tasks")].singleton::<Tasks>());
    assert_eq!(world.root_scope().singleton::<Tasks>(), Some(tasks));
    assert!(
        !world.root_scope().query_one::<Has<Tasks>>(tasks).get(),
        "the key is not a component the holder carries",
    );
}

#[test]
#[should_panic(expected = "already held by a live entity")]
fn a_second_spawn_with_the_key_panics() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![Name("first")].singleton::<Tasks>());
    world
        .root_scope_mut()
        .spawn(bundle![Name("second")].singleton::<Tasks>());
}

#[test]
fn the_key_is_world_wide_not_scope_local() {
    let (mut world, scene, sibling) = fixture();
    let holder = world
        .scope_mut(scene)
        .spawn(bundle![Name("tasks")].singleton::<Tasks>());
    // The sibling cannot see the holder, so it finds nothing, yet the key is taken.
    assert_eq!(world.scope(sibling).singleton::<Tasks>(), None);
    let reserved = world.reserve_entity("late");
    assert_eq!(
        world
            .scope_mut(sibling)
            .spawn_at(reserved, bundle![Name("late")].singleton::<Tasks>()),
        Err(taken()),
    );
    assert!(
        world.release_entity(reserved),
        "a refused spawn keeps its handle reserved",
    );
    assert_eq!(world.scope(scene).singleton::<Tasks>(), Some(holder));
}

#[test]
fn despawning_the_holder_releases_the_key() {
    let mut world = World::new();
    let first = world
        .root_scope_mut()
        .spawn(bundle![Name("first")].singleton::<Tasks>());
    world.root_scope_mut().despawn(first).unwrap();
    assert_eq!(world.root_scope().singleton::<Tasks>(), None);
    let second = world
        .root_scope_mut()
        .spawn(bundle![Name("second")].singleton::<Tasks>());
    assert_eq!(world.root_scope().singleton::<Tasks>(), Some(second));
}

#[test]
fn a_holder_removed_with_its_scope_releases_the_key() {
    let (mut world, scene, _) = fixture();
    world
        .scope_mut(scene)
        .spawn(bundle![Name("tasks")].singleton::<Tasks>());
    world.root_scope_mut().despawn(scene).unwrap();
    assert_eq!(world.root_scope().singleton::<Tasks>(), None);
    world
        .root_scope_mut()
        .spawn(bundle![Name("again")].singleton::<Tasks>());
}

#[test]
fn the_key_is_still_held_while_the_holder_despawns() {
    let mut world = World::new();
    let seen = Rc::new(Cell::new(None));
    let writer = Rc::clone(&seen);
    let holder = world.root_scope_mut().spawn(
        bundle![
            Name("tasks"),
            OnDespawn::new(move |scope, _doomed| {
                writer.set(scope.readonly().singleton::<Tasks>());
            }),
        ]
        .singleton::<Tasks>(),
    );
    world.root_scope_mut().despawn(holder).unwrap();
    assert_eq!(
        seen.get(),
        Some(holder),
        "the callback runs while the key is held",
    );
    assert_eq!(world.root_scope().singleton::<Tasks>(), None);
}

#[test]
fn a_queued_second_spawn_with_the_key_is_refused_at_apply() {
    let mut world = World::new();
    let queue = world.root_scope().queue();
    let first = queue.spawn(bundle![Name("first")].singleton::<Tasks>());
    let second = queue.spawn(bundle![Name("second")].singleton::<Tasks>());
    assert_eq!(world.apply(), Err(taken()));
    assert_eq!(world.root_scope().singleton::<Tasks>(), Some(first));
    assert!(
        world.release_entity(second),
        "the refused handle stays reserved",
    );
}

#[test]
fn distinct_keys_are_independent_of_each_other_and_of_plain_spawns() {
    let mut world = World::new();
    let tasks = world
        .root_scope_mut()
        .spawn(bundle![Name("tasks")].singleton::<Tasks>());
    let other = world
        .root_scope_mut()
        .spawn(bundle![Name("other")].singleton::<Other>());
    world.root_scope_mut().spawn(bundle![Name("plain")]);
    world.root_scope_mut().spawn(bundle![Name("plain")]);
    assert_eq!(world.root_scope().singleton::<Tasks>(), Some(tasks));
    assert_eq!(world.root_scope().singleton::<Other>(), Some(other));
    assert_eq!(world.root_scope().len(), 4);
}
