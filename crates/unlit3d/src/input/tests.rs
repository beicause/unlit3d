use core::cell::Cell;
use std::rc::Rc;

use unlit_ecs::{Archetype, Has};

use super::*;

/// A pointer position used where a test only needs one that is not the
/// origin.
const CURSOR: [f32; 2] = [3.0, 4.0];

fn key_event(key: Key) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    })
}

fn mouse_event() -> InputEvent {
    InputEvent::Mouse(MouseEvent::Moved { position: CURSOR })
}

fn pointer_event() -> InputEvent {
    InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Moved,
        position: Some(CURSOR),
        modifiers: Modifiers::default(),
    })
}

fn touch_event() -> InputEvent {
    InputEvent::Touch(TouchEvent {
        id: 1,
        phase: TouchPhase::Started,
        position: CURSOR,
        force: None,
    })
}

fn text_event() -> InputEvent {
    InputEvent::Text(TextEvent("hi".to_owned()))
}

fn ime_event() -> InputEvent {
    InputEvent::Ime(ImeEvent {
        kind: ImeKind::Commit("hi".to_owned()),
    })
}

/// A counter a test's callbacks bump, so it can be read after the dispatch
/// even though the callbacks own their state.
fn counter() -> (Rc<Cell<usize>>, Rc<Cell<usize>>) {
    let shared = Rc::new(Cell::new(0));
    (shared.clone(), shared)
}

#[test]
fn each_event_reaches_its_category_and_every_on_input() {
    let mut world = World::new();
    let (seen_input, input) = counter();
    let (seen_key, key) = counter();
    let (seen_mouse, mouse) = counter();
    let (seen_pointer, pointer) = counter();
    let (seen_touch, touch) = counter();
    let (seen_text, text) = counter();
    let (seen_ime, ime) = counter();

    world
        .root_scope_mut()
        .spawn(bundle![OnInput::new(move |_, _, _| {
            seen_input.set(seen_input.get() + 1);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(move |_, _, _| {
            seen_key.set(seen_key.get() + 1);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnMouse::new(move |_, _, _| {
            seen_mouse.set(seen_mouse.get() + 1);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnPointer::new(move |_, _, _| {
            seen_pointer.set(seen_pointer.get() + 1);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnTouch::new(move |_, _, _| {
            seen_touch.set(seen_touch.get() + 1);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnText::new(move |_, _, _| {
            seen_text.set(seen_text.get() + 1);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnIme::new(move |_, _, _| {
            seen_ime.set(seen_ime.get() + 1);
        }),]);

    let input_handle = InputWriter::new();
    world
        .root_scope_mut()
        .spawn(InputFeed(input_handle.clone()));
    let events = [
        key_event(Key::W),
        mouse_event(),
        pointer_event(),
        touch_event(),
        text_event(),
        ime_event(),
        InputEvent::ModifiersChanged(Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }),
        InputEvent::FocusChanged(false),
    ];
    let event_count = events.len();
    for event in events {
        input_handle.push(event);
    }

    assert!(dispatch_input(&world), "events were delivered");

    assert_eq!(input.get(), event_count, "OnInput hears every event");
    assert_eq!(key.get(), 1, "OnKey hears only the key event");
    assert_eq!(mouse.get(), 1, "OnMouse hears only the mouse event");
    assert_eq!(pointer.get(), 1);
    assert_eq!(touch.get(), 1);
    assert_eq!(text.get(), 1);
    assert_eq!(ime.get(), 1);
}

#[test]
fn one_entity_carries_on_key_and_on_input_together() {
    // The two components are different cells, so a single entity may have
    // both and both run in the same dispatch.
    let mut world = World::new();
    let (keys, key_count) = counter();
    let (all, all_count) = counter();

    world.root_scope_mut().spawn(bundle![
        OnKey::new(move |_, _, _| keys.set(keys.get() + 1)),
        OnInput::new(move |_, _, _| all.set(all.get() + 1)),
    ]);
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));
    input.push(mouse_event());

    dispatch_input(&world);

    assert_eq!(key_count.get(), 1);
    assert_eq!(all_count.get(), 2);
}

#[test]
fn one_behaviour_serves_every_entity_carrying_it() {
    let mut world = World::new();
    let (count, seen) = counter();
    for _ in 0..3 {
        let count = count.clone();
        world.root_scope_mut().spawn(bundle![OnKey::new(
            move |_, _, _| count.set(count.get() + 1)
        ),]);
    }

    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    dispatch_input(&world);

    assert_eq!(seen.get(), 3, "one key event per behaviour");
}

#[test]
fn a_callback_sees_the_entity_it_sits_on() {
    // The entity a behaviour is passed is the one the component sits on, so
    // a callback can reach its own sibling components.
    let mut world = World::new();
    let (matches, seen) = counter();
    world.root_scope_mut().spawn(bundle![
        7u32,
        OnKey::new(move |world, entity, _| {
            let marker = *world.root_scope().query_one::<&u32>(entity).unwrap().get();
            matches.set(marker as usize);
        }),
    ]);
    // Another entity of the same kind must not be substituted.
    world
        .root_scope_mut()
        .spawn(bundle![9u32, OnKey::new(|_, _, _| {})]);

    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    dispatch_input(&world);

    assert_eq!(seen.get(), 7);
}

#[test]
fn a_callback_may_read_the_input_state() {
    // The batch is copied out and the state folded in before any callback
    // runs, so a callback reads the state its own event produced.
    let mut world = World::new();
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    let (held, seen) = counter();
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(move |world, _, _| {
            let mut input_events_rows_2 = world.root_scope().query::<&InputEvents>();

            let ctrl = input_events_rows_2
                .iter()
                .next()
                .is_some_and(|input_events| input_events.with_state(|state| state.modifiers.ctrl));
            if ctrl {
                held.set(held.get() + 1);
            }
        }),]);
    input.push(InputEvent::Key(KeyEvent {
        key: Key::C,
        pressed: true,
        repeat: false,
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    }));

    dispatch_input(&world);

    assert_eq!(seen.get(), 1, "the callback read the state the event built");
}

#[test]
fn a_callback_may_filter_its_siblings_without_disturbing_the_dispatch() {
    // `With` borrows no cell, so a callback may enumerate the entities of
    // its own component type even though one of them is borrowed.
    let mut world = World::new();
    let (count, seen) = counter();
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(move |world, _, _| {
            let siblings = world
                .root_scope()
                .query::<Entity>()
                .by(|archetype: &Archetype| archetype.has::<OnKey>())
                .iter()
                .count();
            count.set(siblings);
        }),]);
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(|_, _, _| {})]);

    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    dispatch_input(&world);

    assert_eq!(seen.get(), 2);
}

#[test]
fn a_queued_spawn_lands_after_apply() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(|world, _, _| {
            world.root_scope().queue().spawn(bundle![42u32]);
        }),]);
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    assert!(dispatch_input(&world));
    assert_eq!(world.root_scope().len(), 2, "queued, not applied");

    world.apply().unwrap();
    assert_eq!(world.root_scope().len(), 3);
}

#[test]
fn a_callback_may_queue_its_own_despawn() {
    let mut world = World::new();
    let doomed = world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(|world, entity, _| {
            world.root_scope().queue().despawn(entity);
        }),]);
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    dispatch_input(&world);
    world.apply().unwrap();

    assert!(
        !world
            .root_scope()
            .query_one::<Has<OnKey>>(doomed)
            .is_ok_and(|mut holder| holder.get())
    );
}

#[test]
fn an_empty_event_list_delivers_nothing() {
    let mut world = World::new();
    let (count, seen) = counter();
    world.root_scope_mut().spawn(bundle![OnInput::new(
        move |_, _, _| count.set(count.get() + 1)
    ),]);
    world.root_scope_mut().spawn(bundle![InputWriter::new()]);

    assert!(!dispatch_input(&world));
    assert_eq!(seen.get(), 0);
}

#[test]
fn no_input_state_resource_is_not_an_error() {
    let mut world = World::new();
    let (count, seen) = counter();
    world.root_scope_mut().spawn(bundle![OnInput::new(
        move |_, _, _| count.set(count.get() + 1)
    ),]);

    assert!(!dispatch_input(&world));
    assert_eq!(seen.get(), 0);
}

#[test]
fn events_without_behaviours_are_delivered_to_nobody() {
    let mut world = World::new();
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    // Events did arrive, so the caller still owes an `apply`.
    assert!(dispatch_input(&world));
}

#[test]
fn push_updates_the_state_it_leaves_behind() {
    let mut state = InputState::default();

    state.push(InputEvent::ModifiersChanged(Modifiers {
        shift: true,
        ..Modifiers::default()
    }));
    state.push(InputEvent::FocusChanged(false));
    state.push(InputEvent::Mouse(MouseEvent::Moved { position: CURSOR }));
    state.push(InputEvent::Mouse(MouseEvent::Button {
        position: CURSOR,
        button: MouseButton::Primary,
        pressed: true,
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Pressed,
        position: Some(CURSOR),
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Touch(TouchEvent {
        id: 7,
        phase: TouchPhase::Started,
        position: CURSOR,
        force: Some(0.5),
    }));

    assert!(state.modifiers.shift);
    assert!(!state.focused);
    assert_eq!(state.cursor, Some(CURSOR));
    assert!(state.buttons.contains(MouseButtons::PRIMARY));
    assert_eq!(state.pointer, Some(CURSOR));
    assert!(state.pointer_down);
    assert_eq!(state.touches, [(7, CURSOR)]);

    // An unnamed button has no bit to occupy, but its event is still
    // delivered.
    state.push(InputEvent::Mouse(MouseEvent::Button {
        position: CURSOR,
        button: MouseButton::Other(9),
        pressed: true,
        modifiers: Modifiers::default(),
    }));
    assert_eq!(state.buttons.held().count(), 1);

    // Releasing clears the bit; ending the touch drops it from the active
    // set.
    state.push(InputEvent::Mouse(MouseEvent::Button {
        position: CURSOR,
        button: MouseButton::Primary,
        pressed: false,
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Released { cancelled: false },
        position: Some(CURSOR),
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Mouse(MouseEvent::Left));
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Left,
        position: None,
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Touch(TouchEvent {
        id: 7,
        phase: TouchPhase::Moved,
        position: [1.0, 2.0],
        force: None,
    }));
    assert!(state.buttons.is_empty());
    assert!(!state.pointer_down);
    assert_eq!(state.cursor, None);
    assert_eq!(state.pointer, None);
    assert_eq!(state.touches, [(7, [1.0, 2.0])], "moved, not duplicated");

    state.push(InputEvent::Touch(TouchEvent {
        id: 7,
        phase: TouchPhase::Ended,
        position: [1.0, 2.0],
        force: None,
    }));
    assert!(state.touches.is_empty());
    assert_eq!(state.buttons.held().count(), 0);
}

#[test]
fn a_pointer_down_state_is_device_agnostic() {
    // A finger sets the down flag without touching a mouse button: that is
    // the whole point of the pointer level, and what lets one drag
    // behaviour serve a mouse and a touch alike.
    let mut state = InputState::default();

    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 1,
        action: PointerAction::Pressed,
        position: Some(CURSOR),
        modifiers: Modifiers::default(),
    }));

    assert!(state.pointer_down);
    assert_eq!(state.pointer, Some(CURSOR));
    assert!(
        state.buttons.is_empty(),
        "a touch is not a mouse button being held"
    );
    assert_eq!(state.cursor, None, "a touch does not move the mouse");

    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 1,
        action: PointerAction::Released { cancelled: true },
        position: None,
        modifiers: Modifiers::default(),
    }));
    assert!(!state.pointer_down, "a cancelled pointer is no longer down");
}

#[test]
fn two_fingers_are_two_pointers() {
    // A single down flag cannot express this: with one finger lifted the
    // other is still down, and a drag that read the flag would think the
    // gesture had ended.
    let mut state = InputState::default();
    let press = |id, position| {
        InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Touch,
            id,
            action: PointerAction::Pressed,
            position: Some(position),
            modifiers: Modifiers::default(),
        })
    };

    state.push(press(1, [10.0, 10.0]));
    state.push(press(2, [40.0, 10.0]));
    assert_eq!(
        state.pointers,
        [
            PointerContact {
                kind: PointerKind::Touch,
                id: 1,
                position: [10.0, 10.0],
            },
            PointerContact {
                kind: PointerKind::Touch,
                id: 2,
                position: [40.0, 10.0],
            },
        ],
        "both contacts are tracked, in the order they went down"
    );

    // Moving one finger moves only its own contact.
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 2,
        action: PointerAction::Moved,
        position: Some([50.0, 10.0]),
        modifiers: Modifiers::default(),
    }));
    assert_eq!(state.pointers[0].position, [10.0, 10.0], "the other stays");
    assert_eq!(state.pointers[1].position, [50.0, 10.0]);
    assert_eq!(state.pointer, Some([50.0, 10.0]), "the latest move is it");

    // Lifting one leaves the other down.
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 1,
        action: PointerAction::Released { cancelled: false },
        position: Some([10.0, 10.0]),
        modifiers: Modifiers::default(),
    }));
    assert!(state.pointer_down, "one finger is still on the screen");
    assert_eq!(state.pointers.len(), 1);
    assert_eq!(state.pointers[0].id, 2);

    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 2,
        action: PointerAction::Released { cancelled: false },
        position: Some([50.0, 10.0]),
        modifiers: Modifiers::default(),
    }));
    assert!(!state.pointer_down, "the screen is clear again");
    assert!(state.pointers.is_empty());
}

#[test]
fn a_mouse_and_a_finger_can_be_down_at_once() {
    // The two kinds have independent id spaces, so the mouse's `0` and a
    // touch's id do not collide.
    let mut state = InputState::default();
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Pressed,
        position: Some([1.0, 1.0]),
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 0,
        action: PointerAction::Pressed,
        position: Some([2.0, 2.0]),
        modifiers: Modifiers::default(),
    }));

    assert_eq!(state.pointers.len(), 2, "the same id on two kinds differ");
    assert!(state.pointer_down);

    // Releasing the mouse leaves the finger down.
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Released { cancelled: false },
        position: Some([1.0, 1.0]),
        modifiers: Modifiers::default(),
    }));
    assert_eq!(state.pointers.len(), 1);
    assert_eq!(state.pointers[0].kind, PointerKind::Touch);
    assert!(state.pointer_down);
}

#[test]
fn a_hover_does_not_press_a_pointer() {
    // A move over the window is a hover: it sets where the pointer is
    // without claiming anything is down.
    let mut state = InputState::default();
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Mouse,
        id: 0,
        action: PointerAction::Moved,
        position: Some(CURSOR),
        modifiers: Modifiers::default(),
    }));

    assert_eq!(state.pointer, Some(CURSOR));
    assert!(!state.pointer_down);
    assert!(state.pointers.is_empty());
}

#[test]
fn a_cancelled_pointer_keeps_the_position_it_was_last_seen_at() {
    // A cancellation may arrive without a position, and the last known one
    // is what a consumer abandons the drag from.
    let mut state = InputState::default();
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 1,
        action: PointerAction::Moved,
        position: Some(CURSOR),
        modifiers: Modifiers::default(),
    }));
    state.push(InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Touch,
        id: 1,
        action: PointerAction::Released { cancelled: true },
        position: None,
        modifiers: Modifiers::default(),
    }));

    assert_eq!(state.pointer, Some(CURSOR));
    assert!(!state.pointer_down);
}

#[test]
fn a_dispatch_leaves_the_state_the_events_built() {
    // The documented frame loop, end to end: deliver, apply, and the
    // state a later frame reads is still there.
    let mut world = World::new();
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(InputEvent::Resized {
        size_px: (800, 600),
    });
    input.push(InputEvent::ScaleFactorChanged { scale_factor: 1.5 });
    input.push(InputEvent::FocusChanged(false));
    input.push(key_event(Key::S));

    assert!(dispatch_input(&world));
    world.apply().unwrap();
    let mut input_events_rows_3 = world.root_scope().query::<&InputEvents>();

    let input_events = input_events_rows_3
        .iter()
        .next()
        .expect("the world has a buffer");
    input_events.with_state(|state| {
        assert_eq!(state.size_px, (800, 600));
        assert_eq!(state.scale_factor, 1.5);
        assert!(!state.focused);
    });
}

/// A world that ticks less often than input arrives still reads every
/// event, in order: what is queued waits for the dispatch that takes it.
#[test]
fn a_slow_consumer_reads_every_event() {
    let mut world = World::new();
    let (count, seen) = counter();
    world.root_scope_mut().spawn(bundle![OnKey::new(
        move |_, _, _| count.set(count.get() + 1)
    ),]);
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));

    // Three frames of input arrive before the world dispatches once.
    input.push(key_event(Key::A));
    input.push(key_event(Key::B));
    input.push(key_event(Key::C));

    assert!(dispatch_input(&world));
    assert_eq!(seen.get(), 3, "every event waited for this dispatch");

    // And the queue is empty afterwards, so nothing is delivered twice.
    assert!(!dispatch_input(&world));
    assert_eq!(seen.get(), 3);
}

/// Everything in a world reads the same batch: a reader cannot take an
/// event away from the reader after it.
#[test]
fn every_reader_of_a_world_sees_the_same_batch() {
    let input = InputWriter::new();
    let buffer = input.subscribe();

    input.push(key_event(Key::A));
    buffer.refresh();

    assert_eq!(
        buffer.with_batch(|batch| batch.to_vec()),
        [key_event(Key::A)]
    );
    assert_eq!(
        buffer.with_batch(|batch| batch.to_vec()),
        [key_event(Key::A)],
        "reading leaves the batch for the next reader"
    );

    // The next tick replaces it, so an event is delivered exactly once.
    buffer.refresh();
    assert!(buffer.with_batch(|batch| batch.to_vec()).is_empty());
}

/// Two worlds fed by one writer build their own state: neither sees the
/// other's, and each is built from the events it ticked on.
#[test]
fn each_world_builds_its_own_state() {
    let input = InputWriter::new();
    let first = input.subscribe();
    let second = input.subscribe();

    input.push(InputEvent::ModifiersChanged(Modifiers {
        shift: true,
        ..Modifiers::default()
    }));

    // One world ticks, the other has not yet.
    first.refresh();
    assert!(first.with_state(|state| state.modifiers.shift));
    assert!(
        !second.with_state(|state| state.modifiers.shift),
        "a world that has not ticked is still in its previous state"
    );

    second.refresh();
    assert!(second.with_state(|state| state.modifiers.shift));
}

/// A world that ticks slower still ends up with the state its own event
/// stream describes, in order, whatever the writer's rate.
#[test]
fn a_slow_world_builds_the_state_its_events_describe() {
    let input = InputWriter::new();
    let buffer = input.subscribe();

    input.push(InputEvent::ModifiersChanged(Modifiers {
        ctrl: true,
        ..Modifiers::default()
    }));
    input.push(InputEvent::ModifiersChanged(Modifiers::default()));

    buffer.refresh();

    assert!(
        !buffer.with_state(|state| state.modifiers.ctrl),
        "the later event is what the state ends up as"
    );
    assert_eq!(
        buffer.with_batch(|batch| batch.to_vec()).len(),
        2,
        "both events are still delivered"
    );
}

#[test]
#[should_panic(expected = "already borrowed")]
fn a_behaviour_cannot_reborrow_its_own_component() {
    // The boundary a callback has to stay inside, pinned here so it cannot
    // regress into a silently different failure.
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(|world, entity, _| {
            let _ = world.root_scope().query_one::<&OnKey>(entity);
        }),]);
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(key_event(Key::A));

    dispatch_input(&world);
}
