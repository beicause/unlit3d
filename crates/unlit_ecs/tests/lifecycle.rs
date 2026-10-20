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
    let entity = world.root_scope_mut().spawn((
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
    world.root_scope_mut().spawn((OnSpawn::new()
        .with(move |_view, _| a.borrow_mut().push("first".to_owned()))
        .with(move |_view, _| b.borrow_mut().push("second".to_owned())),));
    assert_eq!(*seen.borrow(), ["first", "second"]);
}

#[test]
fn a_despawn_callback_runs_before_the_row_is_removed() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let entity = world.root_scope_mut().spawn((
        Marker(7),
        OnDespawn::new().with(move |scope, doomed| {
            // The entity still has its components, which is the whole reason
            // a despawn callback cannot be deferred.
            let marker = scope.get::<Marker>(doomed).expect("still alive").0;
            sink.borrow_mut().push(format!("despawn {marker}"));
        }),
    ));
    world.root_scope_mut().despawn(entity);
    assert_eq!(*seen.borrow(), ["despawn 7"]);
}

#[test]
fn a_cascade_runs_children_before_their_parent() {
    let seen = log();
    let mut world = World::new();
    let outer = Rc::clone(&seen);
    let scene = world.root_scope_mut().spawn((
        Marker(1),
        OnDespawn::new().with(move |_view, _| outer.borrow_mut().push("scene".to_owned())),
    ));
    let inner = Rc::clone(&seen);
    world.scope_mut(scene).unwrap().spawn((
        Marker(2),
        OnDespawn::new().with(move |_view, _| inner.borrow_mut().push("child".to_owned())),
    ));
    world.root_scope_mut().despawn(scene);
    assert_eq!(*seen.borrow(), ["child", "scene"]);
}

#[test]
fn a_spawn_callback_sees_its_own_components() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.root_scope_mut().spawn((
        Marker(42),
        OnSpawn::new().with(move |scope, spawned| {
            *sink.borrow_mut() = scope.get::<Marker>(spawned).map(|marker| marker.0);
        }),
    ));
    assert_eq!(*observed.borrow(), Some(42));
}

#[test]
fn a_callback_can_queue_a_spawn_that_runs_before_the_next_one() {
    // What a callback queues lands before the next callback of the same
    // component, so a later callback sees the world an earlier one built.
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let later = Rc::clone(&seen);
    world.root_scope_mut().spawn((OnSpawn::new()
        .with(move |scope, _| {
            scope.queue().spawn((
                Marker(9),
                OnSpawn::new().with({
                    let nested = Rc::clone(&sink);
                    move |_view, _| nested.borrow_mut().push("nested".to_owned())
                }),
            ));
        })
        .with(move |view, _| {
            let mut markers = view.query::<&Marker>();
            let nested = markers.any(|(_, marker)| marker.0 == 9);
            later.borrow_mut().push(format!("later sees {nested}"));
        }),));
    assert_eq!(*seen.borrow(), ["nested", "later sees true"]);
}

#[test]
fn a_later_callback_runs_against_what_an_earlier_one_queued() {
    // The callbacks of one component are driven through the world's own runner,
    // which applies between them: the second sees the spawn the first queued.
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    world.root_scope_mut().spawn((OnSpawn::new()
        .with({
            let sink = Rc::clone(&seen);
            move |scope, _| {
                sink.borrow_mut().push("first".to_owned());
                scope.queue().spawn((Marker(9),));
            }
        })
        .with(move |scope, _| {
            let mut markers = scope.query::<&Marker>();
            let spawned = markers.any(|(_, marker)| marker.0 == 9);
            sink.borrow_mut().push(format!("second sees {spawned}"));
        }),));
    assert_eq!(*seen.borrow(), ["first", "second sees true"]);
}

#[test]
fn a_despawn_cascade_applies_what_it_queued_after_every_callback() {
    // A queued despawn of the entity whose callback is running must not restart
    // the cascade: the row is still there, and the callbacks still to run read it.
    let seen = log();
    let mut world = World::new();
    let inner = Rc::clone(&seen);
    let outer = Rc::clone(&seen);
    let entity = world.root_scope_mut().spawn((
        Marker(1),
        OnDespawn::new()
            .with(move |view, doomed| {
                inner.borrow_mut().push("first".to_owned());
                view.queue().despawn(doomed);
            })
            .with(move |view, doomed| {
                let alive = view.get::<Marker>(doomed).is_some();
                outer
                    .borrow_mut()
                    .push(format!("second sees alive {alive}"));
            }),
    ));
    world.root_scope_mut().despawn(entity);
    assert_eq!(*seen.borrow(), ["first", "second sees alive true"]);
    assert!(!world.root_scope().contains(entity));
}

#[test]
fn an_entity_without_lifecycle_components_runs_nothing() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn((Marker(1),));
    assert!(world.root_scope_mut().despawn(entity));
}

#[test]
fn link_does_not_trigger_a_lifecycle_callback() {
    let seen = log();
    let mut world = World::new();
    let sink = Rc::clone(&seen);
    let scene = world.root_scope_mut().spawn((Marker(1),));
    let child = world.root_scope_mut().spawn((
        Marker(2),
        OnSpawn::new().with(move |_view, _| sink.borrow_mut().push("spawn".to_owned())),
    ));
    seen.borrow_mut().clear();
    world.scope_mut(scene).unwrap().link(child, scene);
    assert!(seen.borrow().is_empty(), "relinking is not a spawn");
}

#[test]
fn a_callback_gets_the_view_of_the_entity_s_own_scope() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn((Marker(1),));
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.scope_mut(scene).unwrap().spawn((
        Marker(2),
        OnSpawn::new().with(move |scope, _| {
            *sink.borrow_mut() = Some(scope.entity());
        }),
    ));
    assert_eq!(*observed.borrow(), Some(scene));
}

#[test]
fn a_callback_can_descend_into_the_entity_it_sits_on() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    let _scene = world.root_scope_mut().spawn((
        Marker(1),
        OnSpawn::new().with(move |scope, spawned| {
            // The entity's own scope is its parent, so its subtree is one
            // level down.
            *sink.borrow_mut() = scope.scope(spawned).map(|scene| scene.len());
        }),
    ));
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
    let entity = world.root_scope_mut().spawn((
        Marker(1),
        OnDespawn::new().with(move |_view, _| sink.borrow_mut().push("gone".to_owned())),
    ));
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
    let queued = world.root_scope().queue().spawn((
        Marker(1),
        OnSpawn::new().with(move |_view, _| sink.borrow_mut().push("born".to_owned())),
    ));
    assert!(seen.borrow().is_empty());
    world.apply();
    assert_eq!(*seen.borrow(), ["born"]);
    assert!(world.root_scope().contains(queued));
}

#[test]
fn a_callback_may_touch_its_own_lifecycle_component() {
    let mut world = World::new();
    let observed = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&observed);
    world.root_scope_mut().spawn((
        Marker(1),
        OnSpawn::new().with(move |scope, spawned| {
            // The runner holds no borrow while the callbacks run, so reading
            // the component the callback sits on is not a conflict.
            *sink.borrow_mut() = scope.has::<OnSpawn>(spawned).then_some(());
        }),
    ));
    assert_eq!(*observed.borrow(), Some(()));
}

#[test]
fn a_callback_may_register_another_callback() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn((OnSpawn::new().with(|scope, spawned| {
            if let Some(mut hooks) = scope.get_mut::<OnSpawn>(spawned) {
                hooks.push(|_: &unlit_ecs::Scope<'_>, _: unlit_ecs::Entity| {});
            }
        }),));
    let hooks = world.root_scope().get::<OnSpawn>(entity).unwrap();
    assert_eq!(hooks.0.len(), 2, "the registered callback was kept");
}

#[test]
fn a_callback_registered_during_a_run_goes_last_and_waits_its_turn() {
    let seen = log();
    let mut world = World::new();
    let first = Rc::clone(&seen);
    let second = Rc::clone(&seen);
    let late = Rc::clone(&seen);
    let entity = world.root_scope_mut().spawn((OnSpawn::new()
        .with(move |scope, spawned| {
            first.borrow_mut().push("first".to_owned());
            let late = Rc::clone(&late);
            if let Some(mut hooks) = scope.get_mut::<OnSpawn>(spawned) {
                hooks.push(move |_, _| late.borrow_mut().push("late".to_owned()));
            }
        })
        .with(move |_, _| second.borrow_mut().push("second".to_owned())),));
    assert_eq!(
        *seen.borrow(),
        ["first", "second"],
        "the run stays as long as it was when it started"
    );
    let hooks = world.root_scope().get::<OnSpawn>(entity).unwrap();
    assert_eq!(
        hooks.0.len(),
        3,
        "the new callback is kept for the next run"
    );
}

#[test]
fn a_panicking_callback_leaves_the_ones_behind_it_registered() {
    let mut world = World::new();
    let entity = world.reserve_entity();
    let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.root_scope_mut().spawn_at(
            entity,
            (
                Marker(1),
                OnSpawn::new()
                    .with(|_, _| panic!("this callback fails"))
                    .with(|_, _| {}),
            ),
        );
    }));
    assert!(spawned.is_err(), "the panic reaches the caller");
    let hooks = world
        .root_scope()
        .get::<OnSpawn>(entity)
        .expect("the row is in place");
    assert_eq!(
        hooks.0.len(),
        1,
        "the callback behind the one that panicked is still registered"
    );
}
