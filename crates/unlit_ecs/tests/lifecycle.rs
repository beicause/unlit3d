//! Lifecycle behaviour components: the callback a spawn and a despawn run.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{OnDespawn, OnSpawn, World, bundle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marker(u32);

/// A log the callbacks append to, so their order is observable.
fn log() -> Rc<RefCell<Vec<String>>> {
    Rc::new(RefCell::new(Vec::new()))
}

#[test]
fn a_spawn_callback_runs_once() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let entity = world.root_scope_mut().spawn(bundle![
        Marker(1),
        OnSpawn::new(move |_view, spawned| {
            sink.borrow_mut().push(format!("spawn {}", spawned.index()));
        }),
    ]);
    assert_eq!(*seen.borrow(), [format!("spawn {}", entity.index())]);
}

#[test]
fn a_despawn_callback_runs_before_the_row_is_removed() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let entity = world.root_scope_mut().spawn(bundle![
        Marker(7),
        OnDespawn::new(move |scope, doomed| {
            // The entity still has its components, which is the whole reason
            // a despawn callback cannot be deferred.
            let marker = scope.query_one::<&Marker>(doomed).expect("still alive").0;
            sink.borrow_mut().push(format!("despawn {marker}"));
        }),
    ]);
    world.root_scope_mut().despawn(entity);
    assert_eq!(*seen.borrow(), ["despawn 7"]);
}

#[test]
fn a_cascade_runs_children_before_their_parent() {
    let seen = log();
    let mut world = World::new();
    let outer = Rc::clone(&seen);
    let scene = world.root_scope_mut().spawn(bundle![
        Marker(1),
        OnDespawn::new(move |_view, _| outer.borrow_mut().push("scene".to_owned())),
    ]);
    let inner = Rc::clone(&seen);
    world.scope_mut(scene).unwrap().spawn(bundle![
        Marker(2),
        OnDespawn::new(move |_view, _| inner.borrow_mut().push("child".to_owned())),
    ]);
    world.root_scope_mut().despawn(scene);
    assert_eq!(*seen.borrow(), ["child", "scene"]);
}

#[test]
fn a_spawn_callback_sees_its_own_components() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.root_scope_mut().spawn(bundle![
        Marker(42),
        OnSpawn::new(move |scope, spawned| {
            *sink.borrow_mut() = scope
                .query_one::<&Marker>(spawned)
                .map(|marker| marker.0)
                .ok();
        }),
    ]);
    assert_eq!(*observed.borrow(), Some(42));
}

#[test]
fn a_callback_can_queue_a_spawn_that_runs_on_apply() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    world
        .root_scope_mut()
        .spawn(bundle![OnSpawn::new(move |scope, _| {
            scope.queue().spawn(bundle![
                Marker(9),
                OnSpawn::new({
                    let nested = Rc::clone(&sink);
                    move |_view, _| nested.borrow_mut().push("nested".to_owned())
                }),
            ]);
        }),]);
    assert!(seen.borrow().is_empty(), "queued, not spawned");
    world.apply();
    assert_eq!(*seen.borrow(), ["nested"]);
}

#[test]
fn an_entity_without_lifecycle_components_runs_nothing() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![Marker(1)]);
    assert!(world.root_scope_mut().despawn(entity));
}

#[test]
fn link_does_not_trigger_a_lifecycle_callback() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let scene = world.root_scope_mut().spawn(bundle![Marker(1)]);
    let child = world.root_scope_mut().spawn(bundle![
        Marker(2),
        OnSpawn::new(move |_view, _| sink.borrow_mut().push("spawn".to_owned())),
    ]);
    seen.borrow_mut().clear();
    world.scope_mut(scene).unwrap().link(child, scene);
    assert!(seen.borrow().is_empty(), "relinking is not a spawn");
}

#[test]
fn a_callback_gets_the_scope_of_the_entity_s_own_scope() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Marker(1)]);
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.scope_mut(scene).unwrap().spawn(bundle![
        Marker(2),
        OnSpawn::new(move |scope, _| {
            *sink.borrow_mut() = Some(scope.entity());
        }),
    ]);
    assert_eq!(*observed.borrow(), Some(scene));
}

#[test]
fn a_callback_can_descend_into_the_entity_it_sits_on() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    let _scene = world.root_scope_mut().spawn(bundle![
        Marker(1),
        OnSpawn::new(move |scope, spawned| {
            // The entity's own scope is its parent, so its subtree is one
            // level down.
            *sink.borrow_mut() = scope.scope(spawned).map(|scene| scene.len()).ok();
        }),
    ]);
    // The scene's own scope counts the scene itself, so an empty subtree reads
    // as one, not zero.
    assert_eq!(
        *observed.borrow(),
        Some(1),
        "the scene and nothing below it"
    );
}

#[test]
fn a_queued_despawn_runs_the_callback_on_apply() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let entity = world.root_scope_mut().spawn(bundle![
        Marker(1),
        OnDespawn::new(move |_view, _| sink.borrow_mut().push("gone".to_owned())),
    ]);
    world.root_scope().queue().despawn(entity);
    assert!(seen.borrow().is_empty(), "queued, not despawned");
    world.apply();
    assert_eq!(*seen.borrow(), ["gone"]);
    assert!(!world.root_scope().contains(entity));
}

#[test]
fn a_queued_spawn_runs_the_callback_on_apply() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let queued = world.root_scope().queue().spawn(bundle![
        Marker(1),
        OnSpawn::new(move |_view, _| sink.borrow_mut().push("born".to_owned())),
    ]);
    assert!(seen.borrow().is_empty());
    world.apply();
    assert_eq!(*seen.borrow(), ["born"]);
    assert!(world.root_scope().contains(queued));
}

#[test]
#[should_panic(expected = "already borrowed")]
fn a_callback_cannot_reborrow_its_own_lifecycle_component() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![OnSpawn::new(|scope, spawned| {
            // The runner holds the component for the whole run, the same way every
            // other behaviour component is held while it runs.
            let _ = scope.query_one::<&mut OnSpawn>(spawned);
        }),]);
}

#[test]
fn a_panicking_callback_leaves_the_component_registered() {
    let mut world = World::new();
    let entity = world.reserve_entity("reserved");
    let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.root_scope_mut().spawn_at(
            entity,
            bundle![
                Marker(1),
                OnSpawn::new(|_, _| panic!("this callback fails")),
            ],
        );
    }));
    assert!(spawned.is_err(), "the panic reaches the caller");
    assert!(
        world.root_scope().query_one::<&OnSpawn>(entity).is_ok(),
        "the callback is still on the row it was spawned with"
    );
}
