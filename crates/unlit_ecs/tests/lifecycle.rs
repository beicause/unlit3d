//! Lifecycle behaviour components: the callbacks a spawn and a despawn run.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{OnDespawn, OnSpawn, World};

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
    let entity = world.root_view_mut().spawn((
        Marker(1),
        OnSpawn::new().with(move |_view, spawned| {
            sink.borrow_mut().push(format!("spawn {}", spawned.index()));
        }),
    ));
    assert_eq!(*seen.borrow(), [format!("spawn {}", entity.index())]);
}

#[test]
fn several_spawn_callbacks_run_in_registration_order() {
    let seen = log();
    let mut world = World::new();
    let a = Rc::clone(&seen);
    let b = Rc::clone(&seen);
    world.root_view_mut().spawn((OnSpawn::new()
        .with(move |_view, _| a.borrow_mut().push("first".to_owned()))
        .with(move |_view, _| b.borrow_mut().push("second".to_owned())),));
    assert_eq!(*seen.borrow(), ["first", "second"]);
}

#[test]
fn a_despawn_callback_runs_before_the_row_is_removed() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let entity = world.root_view_mut().spawn((
        Marker(7),
        OnDespawn::new().with(move |view, doomed| {
            // The entity still has its components, which is the whole reason
            // a despawn callback cannot be deferred.
            let marker = view.get::<Marker>(doomed).expect("still alive").0;
            sink.borrow_mut().push(format!("despawn {marker}"));
        }),
    ));
    world.root_view_mut().despawn(entity);
    assert_eq!(*seen.borrow(), ["despawn 7"]);
}

#[test]
fn a_cascade_runs_children_before_their_parent() {
    let seen = log();
    let mut world = World::new();
    let outer = Rc::clone(&seen);
    let scene = world.root_view_mut().spawn((
        Marker(1),
        OnDespawn::new().with(move |_view, _| outer.borrow_mut().push("scene".to_owned())),
    ));
    let inner = Rc::clone(&seen);
    world.view_mut(scene).unwrap().spawn((
        Marker(2),
        OnDespawn::new().with(move |_view, _| inner.borrow_mut().push("child".to_owned())),
    ));
    world.root_view_mut().despawn(scene);
    assert_eq!(*seen.borrow(), ["child", "scene"]);
}

#[test]
fn a_spawn_callback_sees_its_own_components() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.root_view_mut().spawn((
        Marker(42),
        OnSpawn::new().with(move |view, spawned| {
            *sink.borrow_mut() = view.get::<Marker>(spawned).map(|marker| marker.0);
        }),
    ));
    assert_eq!(*observed.borrow(), Some(42));
}

#[test]
fn a_callback_can_queue_a_spawn_that_runs_on_apply() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    world
        .root_view_mut()
        .spawn((OnSpawn::new().with(move |view, _| {
            view.queue().spawn((
                Marker(9),
                OnSpawn::new().with({
                    let nested = Rc::clone(&sink);
                    move |_view, _| nested.borrow_mut().push("nested".to_owned())
                }),
            ));
        }),));
    assert!(seen.borrow().is_empty(), "queued, not spawned");
    world.apply();
    assert_eq!(*seen.borrow(), ["nested"]);
}

#[test]
fn an_entity_without_lifecycle_components_runs_nothing() {
    let mut world = World::new();
    let entity = world.root_view_mut().spawn((Marker(1),));
    assert!(world.root_view_mut().despawn(entity));
}

#[test]
fn link_does_not_trigger_a_lifecycle_callback() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let scene = world.root_view_mut().spawn((Marker(1),));
    let child = world.root_view_mut().spawn((
        Marker(2),
        OnSpawn::new().with(move |_view, _| sink.borrow_mut().push("spawn".to_owned())),
    ));
    seen.borrow_mut().clear();
    world.view_mut(scene).unwrap().link(child, scene);
    assert!(seen.borrow().is_empty(), "relinking is not a spawn");
}

#[test]
fn a_callback_gets_the_view_of_the_entity_s_own_scope() {
    let mut world = World::new();
    let scene = world.root_view_mut().spawn((Marker(1),));
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.view_mut(scene).unwrap().spawn((
        Marker(2),
        OnSpawn::new().with(move |view, _| {
            *sink.borrow_mut() = Some(view.scope());
        }),
    ));
    assert_eq!(*observed.borrow(), Some(scene));
}

#[test]
fn a_callback_can_descend_into_the_entity_it_sits_on() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    let _scene = world.root_view_mut().spawn((
        Marker(1),
        OnSpawn::new().with(move |view, spawned| {
            // The entity's own scope is its parent, so its subtree is one
            // level down.
            *sink.borrow_mut() = view.view(spawned).map(|scene| scene.len());
        }),
    ));
    // The scene's own view counts the scene itself, so an empty subtree reads
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
    let entity = world.root_view_mut().spawn((
        Marker(1),
        OnDespawn::new().with(move |_view, _| sink.borrow_mut().push("gone".to_owned())),
    ));
    world.root_view().queue().despawn(entity);
    assert!(seen.borrow().is_empty(), "queued, not despawned");
    world.apply();
    assert_eq!(*seen.borrow(), ["gone"]);
    assert!(!world.root_view().contains(entity));
}

#[test]
fn a_queued_spawn_runs_the_callback_on_apply() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let queued = world.root_view().queue().spawn((
        Marker(1),
        OnSpawn::new().with(move |_view, _| sink.borrow_mut().push("born".to_owned())),
    ));
    assert!(seen.borrow().is_empty());
    world.apply();
    assert_eq!(*seen.borrow(), ["born"]);
    assert!(world.root_view().contains(queued));
}
