use core::cell::RefCell;
use core::ops::Deref;

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{DeviceId, Force, Touch};

use super::*;
use crate::input::{InputEvents, InputState, MouseButtons};

/// The pointer position tests move to before producing a button or wheel
/// event, deliberately not the fallback origin.
const CURSOR: [f32; 2] = [12.0, 34.0];

fn cursor_position() -> PhysicalPosition<f64> {
    PhysicalPosition::new(f64::from(CURSOR[0]), f64::from(CURSOR[1]))
}

/// An adapter, plus a subscription that sees everything it feeds from the
/// moment it exists.
///
/// The subscription is taken before any event is fed, because a subscriber
/// only hears what arrives after it subscribed.
struct TestInput {
    input: WinitInput,
    buffer: InputEvents,
    /// What the subscription has yielded so far, kept so a test can look at
    /// a stream it has already read.
    seen: RefCell<Vec<InputEvent>>,
}

impl Deref for TestInput {
    type Target = WinitInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

fn input() -> TestInput {
    let writer = InputWriter::new();
    let buffer = writer.subscribe();
    let input = WinitInput::new(writer);
    TestInput {
        input,
        buffer,
        seen: RefCell::new(Vec::new()),
    }
}

/// Everything the adapter has fed, as a world ticking after each feed sees
/// it.
///
/// The adapter feeds a writer, so this is the same stream a world
/// dispatching input reads.
fn events(input: &TestInput) -> Vec<InputEvent> {
    input.buffer.refresh();
    let mut batch = Vec::new();
    input.buffer.copy_events_into(&mut batch);
    let mut seen = input.seen.borrow_mut();
    seen.extend(batch);
    seen.clone()
}

/// Run `f` over the state the events so far left behind.
///
/// Building it means ticking first, which is what a world does before it
/// reads anything.
fn state<R>(input: &TestInput, f: impl FnOnce(&InputState) -> R) -> R {
    input.buffer.refresh();
    input.buffer.with_state(f)
}

fn moved() -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: cursor_position(),
    }
}

/// A touch event with the given phase and force.
fn touch(phase: WinitTouchPhase, force: Option<Force>) -> WindowEvent {
    WindowEvent::Touch(Touch {
        device_id: DeviceId::dummy(),
        phase,
        location: PhysicalPosition::new(5.0, 6.0),
        force,
        id: 1,
    })
}

#[test]
fn a_physical_key_carries_its_key_and_the_held_modifiers() {
    // winit's `KeyEvent` cannot be built outside winit, because its
    // platform-specific field is private, so the keyboard translation is
    // driven through the same entry point `WindowEvent::KeyboardInput`
    // calls with the fields it destructures.
    let input = input();
    let shift = ModifiersState::SHIFT.into();
    assert!(input.on_window_event(&WindowEvent::ModifiersChanged(shift)));

    assert!(input.on_key(PhysicalKey::Code(KeyCode::KeyW), true, false, None));
    assert!(input.on_key(PhysicalKey::Code(KeyCode::KeyW), false, false, None));

    let expected = to_modifiers(ModifiersState::SHIFT);
    let events = events(&input);
    assert_eq!(
        events[1],
        InputEvent::Key(KeyEvent {
            key: Key::W,
            pressed: true,
            repeat: false,
            modifiers: expected,
        })
    );
    assert_eq!(
        events[2],
        InputEvent::Key(KeyEvent {
            key: Key::W,
            pressed: false,
            repeat: false,
            modifiers: expected,
        }),
        "the release carries the modifiers too"
    );
    assert!(state(&input, |state| state.modifiers).shift);
}

#[test]
fn a_pressed_key_also_pushes_the_text_it_produced() {
    let input = input();

    assert!(input.on_key(PhysicalKey::Code(KeyCode::KeyA), true, false, Some("a")));

    assert_eq!(
        events(&input),
        [
            InputEvent::Key(KeyEvent {
                key: Key::A,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Text(TextEvent("a".to_owned())),
        ]
    );
}

#[test]
fn a_released_key_pushes_no_text() {
    let input = input();

    assert!(input.on_key(PhysicalKey::Code(KeyCode::KeyA), false, false, Some("a")));

    assert_eq!(events(&input).len(), 1);
}

#[test]
fn every_named_key_maps_to_its_variant() {
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Escape)), Key::Escape);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Space)), Key::Space);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Enter)), Key::Enter);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Tab)), Key::Tab);
    assert_eq!(
        key_of(PhysicalKey::Code(KeyCode::Backspace)),
        Key::Backspace
    );
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Delete)), Key::Delete);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::ArrowUp)), Key::ArrowUp);
    assert_eq!(
        key_of(PhysicalKey::Code(KeyCode::ArrowDown)),
        Key::ArrowDown
    );
    assert_eq!(
        key_of(PhysicalKey::Code(KeyCode::ArrowLeft)),
        Key::ArrowLeft
    );
    assert_eq!(
        key_of(PhysicalKey::Code(KeyCode::ArrowRight)),
        Key::ArrowRight
    );
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Home)), Key::Home);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::End)), Key::End);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::PageUp)), Key::PageUp);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::PageDown)), Key::PageDown);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::KeyZ)), Key::Z);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Digit0)), Key::Num0);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::Digit3)), Key::Num3);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::F1)), Key::F1);
    assert_eq!(key_of(PhysicalKey::Code(KeyCode::F35)), Key::F35);
}

#[test]
fn a_key_the_crate_does_not_name_keeps_a_platform_code() {
    // The numpad is deliberately not collapsed into the top-row digits, so
    // it comes through as an unnamed key.
    let numpad = key_of(PhysicalKey::Code(KeyCode::Numpad0));
    assert!(matches!(numpad, Key::Other(_)), "{numpad:?}");
    assert_ne!(numpad, key_of(PhysicalKey::Code(KeyCode::Numpad1)));

    assert_eq!(
        key_of(PhysicalKey::Unidentified(NativeKeyCode::Xkb(9))),
        Key::Other(9)
    );
}

#[test]
fn modifiers_changed_updates_the_state_and_pushes_the_event() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::ModifiersChanged(
        ModifiersState::CONTROL.into()
    )));

    assert_eq!(
        events(&input),
        [InputEvent::ModifiersChanged(to_modifiers(
            ModifiersState::CONTROL
        ))]
    );
    let held = state(&input, |state| state.modifiers);
    assert!(held.ctrl);
    assert_eq!(
        held.command,
        !cfg!(target_os = "macos"),
        "Ctrl commands off macOS"
    );
    assert!(!held.shift);
}

#[test]
fn cursor_moved_pushes_a_mouse_and_a_pointer_move() {
    let input = input();

    assert!(input.on_window_event(&moved()));

    // One physical move, two levels: the mouse names where the cursor is,
    // the pointer names where and from which device, so a drag written
    // against either hears the same move exactly once.
    assert_eq!(
        events(&input),
        [
            InputEvent::Mouse(MouseEvent::Moved { position: CURSOR }),
            InputEvent::Pointer(PointerEvent {
                kind: PointerKind::Mouse,
                id: MOUSE_POINTER,
                action: PointerAction::Moved,
                position: Some(CURSOR),
                modifiers: Modifiers::default(),
            }),
        ]
    );
    assert_eq!(state(&input, |state| state.cursor), Some(CURSOR));
    assert_eq!(state(&input, |state| state.pointer), Some(CURSOR));
}

#[test]
fn a_button_press_uses_the_current_cursor() {
    let input = input();
    assert!(input.on_window_event(&moved()));

    assert!(input.on_window_event(&WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: winit::event::ElementState::Pressed,
        button: MouseButton::Left,
    }));

    assert_eq!(
        events(&input)[2],
        InputEvent::Mouse(MouseEvent::Button {
            position: CURSOR,
            button: CrateButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        })
    );
    assert_eq!(
        events(&input)[3],
        InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Mouse,
            id: MOUSE_POINTER,
            action: PointerAction::Pressed,
            position: Some(CURSOR),
            modifiers: Modifiers::default(),
        })
    );
    assert!(state(&input, |state| state
        .buttons
        .contains(MouseButtons::PRIMARY)));
    assert!(state(&input, |state| state.pointer_down));
}

#[test]
fn a_button_press_without_a_cursor_falls_back_to_the_origin() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: winit::event::ElementState::Released,
        button: MouseButton::Right,
    }));

    assert_eq!(
        events(&input),
        [
            InputEvent::Mouse(MouseEvent::Button {
                position: NO_CURSOR,
                button: CrateButton::Secondary,
                pressed: false,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Pointer(PointerEvent {
                kind: PointerKind::Mouse,
                id: MOUSE_POINTER,
                action: PointerAction::Released { cancelled: false },
                position: Some(NO_CURSOR),
                modifiers: Modifiers::default(),
            }),
        ]
    );
    assert!(!state(&input, |state| state.pointer_down));
}

#[test]
fn a_wheel_delta_keeps_its_unit() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(1.0, -2.0),
        phase: WinitTouchPhase::Moved,
    }));
    assert!(input.on_window_event(&WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(3.0, 4.0)),
        phase: WinitTouchPhase::Ended,
    }));

    // A wheel is a mouse's own: there is no pointer event beside it, so a
    // device-agnostic consumer is not told the page scrolled.
    assert_eq!(
        events(&input),
        [
            InputEvent::Mouse(MouseEvent::Wheel {
                delta: [1.0, -2.0],
                unit: WheelUnit::Line,
                phase: TouchPhase::Moved,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Mouse(MouseEvent::Wheel {
                delta: [3.0, 4.0],
                unit: WheelUnit::Pixel,
                phase: TouchPhase::Ended,
                modifiers: Modifiers::default(),
            }),
        ]
    );
}

#[test]
fn a_pinch_and_a_rotation_become_pointer_gestures() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::PinchGesture {
        device_id: DeviceId::dummy(),
        delta: 1.5,
        phase: WinitTouchPhase::Moved,
    }));
    assert!(input.on_window_event(&WindowEvent::RotationGesture {
        device_id: DeviceId::dummy(),
        delta: -0.25,
        phase: WinitTouchPhase::Moved,
    }));

    assert_eq!(
        events(&input),
        [
            InputEvent::Pointer(PointerEvent {
                kind: GESTURE_KIND,
                id: MOUSE_POINTER,
                action: PointerAction::Zoom(1.5),
                position: None,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Pointer(PointerEvent {
                kind: GESTURE_KIND,
                id: MOUSE_POINTER,
                action: PointerAction::Rotate(-0.25),
                position: None,
                modifiers: Modifiers::default(),
            }),
        ]
    );
}

#[test]
fn a_touch_carries_its_phase_and_normalized_force() {
    let input = input();
    let force = Force::Calibrated {
        force: 1.0,
        max_possible_force: 2.0,
        altitude_angle: None,
    };

    assert!(input.on_window_event(&touch(WinitTouchPhase::Started, Some(force))));
    // A finger reaches a device-agnostic drag through its pointer event,
    // which is what makes the same behaviour work on a touch screen.
    assert_eq!(
        events(&input),
        [
            InputEvent::Touch(TouchEvent {
                id: 1,
                phase: TouchPhase::Started,
                position: [5.0, 6.0],
                force: Some(0.5),
            }),
            InputEvent::Pointer(PointerEvent {
                kind: PointerKind::Touch,
                id: 1,
                action: PointerAction::Pressed,
                position: Some([5.0, 6.0]),
                modifiers: Modifiers::default(),
            }),
        ]
    );
    assert_eq!(
        state(&input, |state| state.touches.clone()),
        [(1, [5.0, 6.0])]
    );
    assert!(state(&input, |state| state.pointer_down));
    assert!(
        state(&input, |state| state.buttons.is_empty()),
        "a touch is not a mouse button"
    );

    assert!(input.on_window_event(&touch(WinitTouchPhase::Moved, None)));
    assert!(input.on_window_event(&touch(WinitTouchPhase::Ended, None)));
    assert_eq!(
        events(&input)[4],
        InputEvent::Touch(TouchEvent {
            id: 1,
            phase: TouchPhase::Ended,
            position: [5.0, 6.0],
            force: None,
        })
    );
    assert!(!state(&input, |state| state.pointer_down));
    assert!(state(&input, |state| state.touches.is_empty()));
}

#[test]
fn a_cancelled_touch_abandons_its_pointer() {
    // The platform taking a touch away ends the drag without a position,
    // so a consumer knows not to treat it as a lift where the finger was.
    let input = input();

    assert!(input.on_window_event(&touch(WinitTouchPhase::Started, None)));
    assert!(input.on_window_event(&touch(WinitTouchPhase::Cancelled, None)));

    assert_eq!(
        events(&input)[3],
        InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Touch,
            id: 1,
            action: PointerAction::Released { cancelled: true },
            position: None,
            modifiers: Modifiers::default(),
        })
    );
    assert!(!state(&input, |state| state.pointer_down));
}

#[test]
fn ime_preedit_and_commit_become_ime_events() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::Ime(Ime::Preedit(
        "にほん".to_owned(),
        Some((3, 6))
    ))));
    assert!(input.on_window_event(&WindowEvent::Ime(Ime::Commit("日本".to_owned()))));
    assert!(input.on_window_event(&WindowEvent::Ime(Ime::Disabled)));

    assert_eq!(
        events(&input),
        [
            InputEvent::Ime(ImeEvent {
                kind: ImeKind::Preedit {
                    text: "にほん".to_owned(),
                    active_range: Some(3..6),
                },
            }),
            InputEvent::Ime(ImeEvent {
                kind: ImeKind::Commit("日本".to_owned()),
            }),
            InputEvent::Ime(ImeEvent {
                kind: ImeKind::Disabled,
            }),
        ]
    );
}

#[test]
fn losing_focus_updates_the_state() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::Focused(false)));

    assert_eq!(events(&input), [InputEvent::FocusChanged(false)]);
    assert!(!state(&input, |state| state.focused));
}

#[test]
fn leaving_the_window_forgets_the_cursor() {
    let input = input();
    assert!(input.on_window_event(&moved()));

    assert!(input.on_window_event(&WindowEvent::CursorLeft {
        device_id: DeviceId::dummy(),
    }));

    assert_eq!(events(&input)[2], InputEvent::Mouse(MouseEvent::Left));
    assert_eq!(
        events(&input)[3],
        InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Mouse,
            id: MOUSE_POINTER,
            action: PointerAction::Left,
            position: None,
            modifiers: Modifiers::default(),
        })
    );
    assert_eq!(state(&input, |state| state.cursor), None);
    assert_eq!(state(&input, |state| state.pointer), None);
}

#[test]
fn a_resize_travels_as_an_event_and_lands_in_the_state() {
    let input = input();

    assert!(input.on_window_event(&WindowEvent::Resized(PhysicalSize::new(800, 600))));

    // The world that reads it sees the event, and its own state records
    // it: writing the state directly would reach one world only.
    assert_eq!(
        events(&input),
        [InputEvent::Resized {
            size_px: (800, 600),
        }]
    );
    assert_eq!(state(&input, |state| state.size_px), (800, 600));
}

#[test]
fn a_scale_factor_change_travels_as_an_event_and_lands_in_the_state() {
    // `WindowEvent::ScaleFactorChanged` carries an `InnerSizeWriter` that
    // cannot be built outside winit, so the translation its match arm
    // performs is driven directly.
    let input = input();

    assert!(input.on_scale_factor(1.5));

    assert_eq!(
        events(&input),
        [InputEvent::ScaleFactorChanged { scale_factor: 1.5 }]
    );
    assert_eq!(state(&input, |state| state.scale_factor), 1.5);
}

#[test]
fn an_ignored_event_pushes_nothing() {
    let input = input();

    assert!(!input.on_window_event(&WindowEvent::RedrawRequested));
    assert!(!input.on_window_event(&WindowEvent::Occluded(true)));

    assert!(events(&input).is_empty());
    assert_eq!(state(&input, |state| state.scale_factor), 1.0);
}
