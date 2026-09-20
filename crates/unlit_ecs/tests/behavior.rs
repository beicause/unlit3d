//! Behaviour components.
//!
//! A behaviour component holds the behaviour itself: a function pointer or a
//! closure that receives the shared world and the entity it runs on, so it can
//! read and write components and queue structural changes the way a system or
//! an observer would. The library has no scheduler and no observer registry;
//! the caller is the system. It decides which entities to drive, in what order,
//! and on which thread.
//!
//! Godot-style callbacks are just behaviour components composed on an entity:
//! one for the per-frame update, one for the fixed-step tick, one for input. An
//! entity takes only the callbacks it needs, and the caller runs the ones it
//! wants. A callback that wants its own state keeps it in a sibling component,
//! because a behaviour component is borrowed while it runs.

use std::cell::Cell;
use std::rc::Rc;

use unlit_ecs::{Entity, LocalWorld, Resource, SendWorld};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Position {
    x: f32,
    y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Speed(f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Left,
    Right,
}

/// A callback that may read and write the world.
type WorldCallback = Box<dyn FnMut(&LocalWorld, Entity)>;

/// The per-frame update, held as a closure.
struct OnFrame(WorldCallback);

impl OnFrame {
    fn new(f: impl FnMut(&LocalWorld, Entity) + 'static) -> Self {
        Self(Box::new(f))
    }

    fn run(&mut self, world: &LocalWorld, entity: Entity) {
        (self.0)(world, entity)
    }
}

/// The per-frame update, held as a plain function pointer.
///
/// A `fn` pointer is an ordinary primitive type, so a struct may hold one as a
/// field. It captures nothing, so it is `Copy`, pointer-sized, and `Send +
/// Sync`.
#[derive(Clone, Copy)]
struct OnFrameFn(fn(&LocalWorld, Entity));

impl OnFrameFn {
    fn run(&mut self, world: &LocalWorld, entity: Entity) {
        (self.0)(world, entity)
    }
}

/// The fixed-step tick, held as a closure. Its interval state lives in
/// [`StepState`], so the callback is free to borrow the world while it runs.
struct OnFixedStep(WorldCallback);

impl OnFixedStep {
    fn new(f: impl FnMut(&LocalWorld, Entity) + 'static) -> Self {
        Self(Box::new(f))
    }

    fn run(&mut self, world: &LocalWorld, entity: Entity) {
        (self.0)(world, entity)
    }
}

/// The state a fixed-step behaviour accumulates between frames.
struct StepState {
    interval: f32,
    accumulator: f32,
    ticks: u32,
}

/// An input callback: it receives the key the caller collected.
type InputCallback = Box<dyn FnMut(Key, &LocalWorld, Entity)>;

/// The input callback, held as a closure.
struct OnInput(InputCallback);

impl OnInput {
    fn new(f: impl FnMut(Key, &LocalWorld, Entity) + 'static) -> Self {
        Self(Box::new(f))
    }

    fn run(&mut self, world: &LocalWorld, entity: Entity, key: Key) {
        (self.0)(key, world, entity)
    }
}

// -- the driver's side of the contract --------------------------------------

/// Advance a fixed-step behaviour by `delta_seconds`, running its callback once
/// per elapsed interval.
fn drive_fixed_step(world: &LocalWorld, entity: Entity, delta_seconds: f32) {
    let fired = world
        .with_mut::<StepState, _>(entity, |state| {
            state.accumulator += delta_seconds;
            let mut fired = 0;
            while state.accumulator >= state.interval {
                state.accumulator -= state.interval;
                state.ticks += 1;
                fired += 1;
            }
            fired
        })
        .unwrap_or(0);

    for _ in 0..fired {
        let _ = world.with_mut::<OnFixedStep, _>(entity, |behaviour| behaviour.run(world, entity));
    }
}

#[test]
fn a_closure_behaviour_runs_and_reaches_the_world() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((
        Position { x: 0.0, y: 0.0 },
        OnFrame::new(|world, entity| {
            let _ = world.with_mut::<Position, _>(entity, |position| position.y += 1.0);
        }),
    ));

    // Driving borrows the behaviour for the call; the closure borrows the world
    // and the entity, so the two do not conflict.
    let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));

    assert_eq!(world.get::<Position>(entity).unwrap().y, 1.0);
}

#[test]
fn a_function_pointer_behaviour_runs() {
    fn advance(world: &LocalWorld, entity: Entity) {
        let _ = world.with_mut::<Position, _>(entity, |position| position.x += 2.0);
    }

    let mut world = LocalWorld::new();
    let entity = world.spawn((Position { x: 0.0, y: 0.0 }, OnFrameFn(advance)));

    let _ = world.with_mut::<OnFrameFn, _>(entity, |behaviour| behaviour.run(&world, entity));
    let _ = world.with_mut::<OnFrameFn, _>(entity, |behaviour| behaviour.run(&world, entity));

    assert_eq!(world.get::<Position>(entity).unwrap().x, 4.0, "ran twice");
}

#[test]
fn a_function_pointer_field_is_copy_so_one_callback_serves_many_entities() {
    fn advance(world: &LocalWorld, entity: Entity) {
        let _ = world.with_mut::<Position, _>(entity, |position| position.x += 2.0);
    }

    let mut world = LocalWorld::new();
    let handler = OnFrameFn(advance);

    // `Copy`: the same callback goes onto several entities, and the code is
    // shared rather than duplicated per entity.
    let first = world.spawn((Position { x: 0.0, y: 0.0 }, handler));
    let second = world.spawn((Position { x: 0.0, y: 0.0 }, handler));

    for entity in [first, second] {
        let _ = world.with_mut::<OnFrameFn, _>(entity, |behaviour| behaviour.run(&world, entity));
    }

    assert_eq!(world.get::<Position>(first).unwrap().x, 2.0);
    assert_eq!(world.get::<Position>(second).unwrap().x, 2.0);
}

#[test]
fn a_function_pointer_field_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    // Capturing nothing, a `fn` pointer crosses threads; a `!Send` closure
    // cannot.
    assert_send_sync::<OnFrameFn>();
}

#[test]
fn one_named_function_serves_both_component_shapes() {
    // A `fn` pointer implements `FnMut`, which is why the design names "a
    // function pointer or a closure" as one category.
    fn advance(world: &LocalWorld, entity: Entity) {
        let _ = world.with_mut::<Position, _>(entity, |position| position.x += 2.0);
    }

    let mut world = LocalWorld::new();
    let entity = world.spawn((
        Position { x: 0.0, y: 0.0 },
        OnFrameFn(advance),
        OnFrame::new(advance),
    ));

    let _ = world.with_mut::<OnFrameFn, _>(entity, |behaviour| behaviour.run(&world, entity));
    let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));

    assert_eq!(world.get::<Position>(entity).unwrap().x, 4.0);
}

#[test]
fn a_function_pointer_behaviour_runs_on_the_send_world() {
    /// A callback that needs only the `Send` world.
    #[derive(Clone, Copy)]
    struct Bump(fn(&SendWorld, Entity));

    fn bump(world: &SendWorld, entity: Entity) {
        let _ = world.with_mut::<u32, _>(entity, |value| *value += 1);
    }

    let mut world = SendWorld::new();
    let entity = world.spawn((0u32, Bump(bump)));
    let world_ref = &world;

    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ =
                world_ref.with_mut::<Bump, _>(entity, |behaviour| (behaviour.0)(world_ref, entity));
        });
    });

    assert_eq!(*world.get::<u32>(entity).unwrap(), 1);
}

#[test]
fn a_behaviour_can_act_on_the_whole_world() {
    let mut world = LocalWorld::new();
    let scale = world.spawn((Resource, 10.0f32));
    let seen = Rc::new(Cell::new(0.0f32));
    let sink = seen.clone();

    let system = world.spawn((OnFrame::new(move |world, _entity| {
        // This closure is a system: it acts on every matching entity, not just
        // the one it is attached to.
        let count = world.query::<&Speed>().count() as f32;
        let scale = *world.get::<f32>(scale).unwrap();
        sink.set(count * scale);
    }),));
    world.spawn((Speed(1.0),));
    world.spawn((Speed(2.0),));

    let _ = world.with_mut::<OnFrame, _>(system, |behaviour| behaviour.run(&world, system));

    assert_eq!(seen.get(), 20.0, "two movers at scale 10");
}

#[test]
fn a_behaviour_reads_a_resource_entity() {
    let mut world = LocalWorld::new();
    let clock = world.spawn((Resource, 0.25f32));
    let seen = Rc::new(Cell::new(0.0f32));
    let sink = seen.clone();

    let entity = world.spawn((OnFrame::new(move |world, _entity| {
        // A resource is reachable from anywhere because the closure holds its
        // handle.
        sink.set(*world.get::<f32>(clock).unwrap());
    }),));

    let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));

    assert_eq!(seen.get(), 0.25);
}

#[test]
fn a_behaviour_keeps_state_itself() {
    // A closure is `FnMut`, so it may carry its own state across calls.
    let frames = Rc::new(Cell::new(0u32));
    let seen = frames.clone();

    let mut world = LocalWorld::new();
    let entity = world.spawn((OnFrame::new(move |_world, _entity| {
        seen.set(seen.get() + 1);
    }),));

    for _ in 0..3 {
        let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));
    }

    assert_eq!(frames.get(), 3);
}

#[test]
fn a_behaviour_can_queue_structural_changes() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((OnFrame::new(|world, _entity| {
        world.queue().spawn((1u32,));
    }),));

    let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));
    assert_eq!(world.len(), 1, "queued, not applied");

    world.apply();
    assert_eq!(world.len(), 2);
}

#[test]
#[should_panic(expected = "already borrowed")]
fn a_behaviour_cannot_reborrow_its_own_component() {
    // The behaviour is borrowed while it runs, so reaching for itself is a
    // borrow conflict. That is the boundary the caller has to stay inside.
    let mut world = LocalWorld::new();
    let entity = world.spawn((OnFrame::new(|world, entity| {
        let _ = world.get::<OnFrame>(entity);
    }),));

    let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));
}

#[test]
fn a_fixed_interval_callback_runs_once_per_interval() {
    let mut world = LocalWorld::new();
    let ticks = Rc::new(Cell::new(0u32));
    let seen = ticks.clone();

    let entity = world.spawn((
        Position { x: 0.0, y: 0.0 },
        StepState {
            interval: 2.0,
            accumulator: 0.0,
            ticks: 0,
        },
        // The callback may borrow the state, because the driver is not holding
        // it while the callback runs.
        OnFixedStep::new(move |world, entity| {
            seen.set(seen.get() + 1);
            let _ = world.with_mut::<Position, _>(entity, |position| position.x += 1.0);
        }),
    ));

    // Five frames of one second each against a two-second interval.
    for _ in 0..5 {
        drive_fixed_step(&world, entity, 1.0);
    }

    assert_eq!(ticks.get(), 2, "two intervals elapsed");
    let state = world.get::<StepState>(entity).unwrap();
    assert_eq!(state.ticks, 2);
    assert_eq!(state.accumulator, 1.0, "the leftover carries forward");
    assert_eq!(world.get::<Position>(entity).unwrap().x, 2.0);
}

#[test]
fn an_input_callback_reacts_to_a_key() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((
        Position { x: 0.0, y: 0.0 },
        Speed(4.0),
        OnInput::new(|key, world, entity| {
            let speed = world.get::<Speed>(entity).unwrap().0;
            let step = match key {
                Key::Left => -speed,
                Key::Right => speed,
            };
            let _ = world.with_mut::<Position, _>(entity, |position| position.x += step);
        }),
    ));

    for key in [Key::Right, Key::Right, Key::Left] {
        let _ =
            world.with_mut::<OnInput, _>(entity, |behaviour| behaviour.run(&world, entity, key));
    }

    assert_eq!(world.get::<Position>(entity).unwrap().x, 4.0);
}

#[test]
fn callbacks_compose_as_separate_components() {
    // The Godot-style callbacks are just behaviour components: an entity takes
    // the ones it needs, and the caller drives each of them.
    let mut world = LocalWorld::new();
    let entity = world.spawn((
        Position { x: 0.0, y: 0.0 },
        StepState {
            interval: 1.0,
            accumulator: 0.0,
            ticks: 0,
        },
        OnFrame::new(|world, entity| {
            let _ = world.with_mut::<Position, _>(entity, |position| position.y += 1.0);
        }),
        OnFixedStep::new(|world, entity| {
            let _ = world.with_mut::<Position, _>(entity, |position| position.x += 10.0);
        }),
        OnInput::new(|key, world, entity| {
            let step = match key {
                Key::Left => -1.0,
                Key::Right => 1.0,
            };
            let _ = world.with_mut::<Position, _>(entity, |position| position.y += step);
        }),
    ));

    // One frame: the frame callback, two fixed steps, then the input.
    let _ = world.with_mut::<OnFrame, _>(entity, |behaviour| behaviour.run(&world, entity));
    drive_fixed_step(&world, entity, 2.5);
    let _ = world.with_mut::<OnInput, _>(entity, |behaviour| {
        behaviour.run(&world, entity, Key::Right)
    });

    let position = world.get::<Position>(entity).unwrap();
    assert_eq!(position.y, 2.0, "one frame plus one input step");
    assert_eq!(position.x, 20.0, "two fixed steps");
    assert_eq!(world.get::<StepState>(entity).unwrap().ticks, 2);
}

#[test]
fn the_caller_chooses_which_behaviours_run() {
    let mut world = LocalWorld::new();
    let ran = Rc::new(Cell::new(0u32));
    let mut driven = Vec::new();

    for _ in 0..3 {
        let counter = ran.clone();
        driven.push(world.spawn((OnFrame::new(move |_world, _entity| {
            counter.set(counter.get() + 1);
        }),)));
    }

    // Only the first two entities are driven this frame.
    for entity in &driven[..2] {
        let _ = world.with_mut::<OnFrame, _>(*entity, |behaviour| behaviour.run(&world, *entity));
    }

    assert_eq!(ran.get(), 2);
}
