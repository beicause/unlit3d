use super::*;
use crate::input::TextEvent;

/// One event's translation at a given density, collected into a fresh
/// buffer so a test can compare the whole list.
fn at(event: InputEvent, pixels_per_point: f32) -> Vec<EguiEvent> {
    let mut events = Vec::new();
    to_egui_event(&event, pixels_per_point, &mut events);
    events
}

/// One event's translation at a density of one, where points and pixels
/// coincide.
fn at_one(event: InputEvent) -> Vec<EguiEvent> {
    at(event, 1.0)
}

fn key(key: Key, pressed: bool) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key,
        pressed,
        repeat: false,
        modifiers: Modifiers::default(),
    })
}

#[test]
fn a_key_press_carries_both_readings_of_the_key() {
    let event = at_one(key(Key::W, true));
    assert_eq!(
        event,
        vec![EguiEvent::Key {
            key: egui::Key::W,
            physical_key: Some(egui::Key::W),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }],
        "egui's shortcuts read the logical key and a game control the \
             physical one, so both are filled from the crate's physical key"
    );
}

#[test]
fn a_key_release_and_repeat_survive_the_translation() {
    let release = at_one(InputEvent::Key(KeyEvent {
        key: Key::Space,
        pressed: false,
        repeat: true,
        modifiers: Modifiers::default(),
    }));
    let EguiEvent::Key {
        pressed, repeat, ..
    } = release[0]
    else {
        panic!("a key event should translate to one egui key event");
    };
    assert!(!pressed && repeat, "both flags belong to egui's own event");
}

#[test]
fn every_named_key_has_a_counterpart() {
    // The crate's keys and egui's are close enough that a name missing on
    // one side is a gap, not a deliberate omission: walk every key the
    // crate names and require a translation.
    for key in every_named_key() {
        assert!(to_key(key).is_some(), "{key:?} has no egui counterpart");
    }
}

/// The crate's `Key::Other` is the only key with no egui counterpart, and
/// its event is dropped rather than reported as a different key.
#[test]
fn an_unnamed_key_is_dropped_rather_than_misreported() {
    assert!(at_one(key(Key::Other(0x58), true)).is_empty());
}

/// `Key::Other` is a real platform key, not a key the crate forgot to
/// name, so it must not be equivalent to any named one.
#[test]
fn an_unnamed_key_is_not_any_named_one() {
    assert!(to_key(Key::Other(4)).is_none());
}

#[test]
fn a_mouse_position_is_converted_to_points() {
    let events = at(
        InputEvent::Mouse(MouseEvent::Moved {
            position: [40.0, 20.0],
        }),
        2.0,
    );
    assert_eq!(
        events,
        vec![EguiEvent::PointerMoved(Pos2::new(20.0, 10.0))],
        "the crate reports physical pixels and egui lays out in points"
    );
}

#[test]
fn a_mouse_button_keeps_its_position_button_and_state() {
    let events = at(
        InputEvent::Mouse(MouseEvent::Button {
            position: [10.0, 30.0],
            button: MouseButton::Secondary,
            pressed: true,
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        }),
        1.0,
    );
    assert_eq!(
        events,
        vec![EguiEvent::PointerButton {
            pos: Pos2::new(10.0, 30.0),
            button: EguiPointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers {
                shift: true,
                ..egui::Modifiers::default()
            },
        }]
    );
}

#[test]
fn every_mouse_button_maps_to_its_own_egui_button() {
    let buttons = [
        (MouseButton::Primary, EguiPointerButton::Primary),
        (MouseButton::Secondary, EguiPointerButton::Secondary),
        (MouseButton::Middle, EguiPointerButton::Middle),
        (MouseButton::Back, EguiPointerButton::Extra1),
        (MouseButton::Forward, EguiPointerButton::Extra2),
    ];
    for (ours, theirs) in buttons {
        assert_eq!(to_pointer_button(ours), Some(theirs));
    }
    // The two side buttons must not collapse onto one.
    assert_ne!(
        to_pointer_button(MouseButton::Back),
        to_pointer_button(MouseButton::Forward)
    );
    assert_eq!(to_pointer_button(MouseButton::Other(7)), None);
}

#[test]
fn a_mouse_leaving_the_window_becomes_pointer_gone() {
    assert_eq!(
        at_one(InputEvent::Mouse(MouseEvent::Left)),
        vec![EguiEvent::PointerGone]
    );
}

/// A device egui only hears about through the pointer level — a pen — does
/// reach egui through it. A mouse, a touch and a trackpad do not, because
/// each already feeds egui's pointer from its own event.
#[test]
fn a_mouse_a_touch_and_a_trackpad_pointer_event_is_dropped_as_a_duplicate() {
    for kind in [
        PointerKind::Mouse,
        PointerKind::Touch,
        PointerKind::Trackpad,
    ] {
        let event = InputEvent::Pointer(PointerEvent {
            kind,
            id: 0,
            action: PointerAction::Moved,
            position: Some([12.0, 24.0]),
            modifiers: Modifiers::default(),
        });
        assert!(
            at_one(event).is_empty(),
            "a {kind:?} pointer event duplicates the event that already feeds egui"
        );
    }
}

/// A device egui only hears about through the pointer level — a pen — does
/// reach egui through it.
#[test]
fn a_pen_pointer_event_reaches_egui() {
    assert_eq!(
        at_one(InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Pen,
            id: 1,
            action: PointerAction::Moved,
            position: Some([12.0, 24.0]),
            modifiers: Modifiers::default(),
        })),
        vec![EguiEvent::PointerMoved(Pos2::new(12.0, 24.0))]
    );

    assert_eq!(
        at_one(InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Pen,
            id: 1,
            action: PointerAction::Pressed,
            position: Some([1.0, 2.0]),
            modifiers: Modifiers::default(),
        })),
        vec![EguiEvent::PointerButton {
            pos: Pos2::new(1.0, 2.0),
            button: EguiPointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }]
    );

    // A pen that lifts stops hovering, so it leaves the screen after the
    // release; a press keeps the pointer where it is.
    assert_eq!(
        at_one(InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Pen,
            id: 1,
            action: PointerAction::Released { cancelled: false },
            position: Some([1.0, 2.0]),
            modifiers: Modifiers::default(),
        })),
        vec![
            EguiEvent::PointerButton {
                pos: Pos2::new(1.0, 2.0),
                button: EguiPointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
            EguiEvent::PointerGone,
        ]
    );
}

#[test]
fn a_pen_leaving_the_window_becomes_pointer_gone() {
    assert_eq!(
        at_one(InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Pen,
            id: 1,
            action: PointerAction::Left,
            position: None,
            modifiers: Modifiers::default(),
        })),
        vec![EguiEvent::PointerGone]
    );
}

#[test]
fn a_wheel_line_delta_passes_through_unscaled() {
    let events = at_one(InputEvent::Mouse(MouseEvent::Wheel {
        delta: [1.0, -3.0],
        unit: WheelUnit::Line,
        phase: TouchPhase::Moved,
        modifiers: Modifiers::default(),
    }));
    assert_eq!(
        events,
        vec![EguiEvent::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: egui::Vec2::new(1.0, -3.0),
            phase: EguiTouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::default(),
        }]
    );
}

#[test]
fn a_wheel_pixel_delta_is_converted_to_points() {
    let events = at(
        InputEvent::Mouse(MouseEvent::Wheel {
            delta: [8.0, -16.0],
            unit: WheelUnit::Pixel,
            phase: TouchPhase::Started,
            modifiers: Modifiers::default(),
        }),
        2.0,
    );
    assert_eq!(
        events,
        vec![EguiEvent::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: egui::Vec2::new(4.0, -8.0),
            phase: EguiTouchPhase::Start,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::default(),
        }],
        "egui measures a point-unit delta in points, so a pixel delta is scaled"
    );
}

#[test]
fn zoom_and_rotate_pass_their_amount_through() {
    assert_eq!(
        at_one(InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Touch,
            id: 1,
            action: PointerAction::Zoom(1.5),
            position: None,
            modifiers: Modifiers::default(),
        })),
        vec![EguiEvent::Zoom(1.5)]
    );
    assert_eq!(
        at_one(InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Touch,
            id: 1,
            action: PointerAction::Rotate(0.5),
            position: None,
            modifiers: Modifiers::default(),
        })),
        vec![EguiEvent::Rotate(0.5)]
    );
}

#[test]
fn text_is_forwarded_as_its_own_event() {
    assert_eq!(
        at_one(InputEvent::Text(TextEvent("héllo".to_string()))),
        vec![EguiEvent::Text("héllo".to_string())]
    );
}

#[test]
fn modifiers_are_forwarded_with_command_intact() {
    let modifiers = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::default()
    };
    assert_eq!(
        at_one(InputEvent::ModifiersChanged(modifiers)),
        vec![EguiEvent::ModifiersChanged(egui::Modifiers {
            ctrl: true,
            command: true,
            ..egui::Modifiers::default()
        })]
    );
}

/// A touch becomes the touch egui wants plus the pointer events that make
/// a widget react to it; a start and an end differ in which pointer
/// transitions they carry.
#[test]
fn a_touch_reports_itself_and_the_pointer() {
    let start = at_one(InputEvent::Touch(TouchEvent {
        id: 7,
        phase: TouchPhase::Started,
        position: [12.0, 24.0],
        force: Some(0.5),
    }));
    assert_eq!(
        start,
        vec![
            EguiEvent::Touch {
                device_id: TouchDeviceId(0),
                id: TouchId(7),
                phase: EguiTouchPhase::Start,
                pos: Pos2::new(12.0, 24.0),
                force: Some(0.5),
            },
            EguiEvent::PointerMoved(Pos2::new(12.0, 24.0)),
            EguiEvent::PointerButton {
                pos: Pos2::new(12.0, 24.0),
                button: EguiPointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ],
        "a finger down must also press the pointer, or a tap hits nothing"
    );

    let end = at_one(InputEvent::Touch(TouchEvent {
        id: 7,
        phase: TouchPhase::Ended,
        position: [12.0, 24.0],
        force: None,
    }));
    assert_eq!(
        end,
        vec![
            EguiEvent::Touch {
                device_id: TouchDeviceId(0),
                id: TouchId(7),
                phase: EguiTouchPhase::End,
                pos: Pos2::new(12.0, 24.0),
                force: None,
            },
            EguiEvent::PointerButton {
                pos: Pos2::new(12.0, 24.0),
                button: EguiPointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
            EguiEvent::PointerGone,
        ],
        "the release must come before the pointer leaves"
    );
}

#[test]
fn a_moved_touch_only_moves_the_pointer() {
    let events = at_one(InputEvent::Touch(TouchEvent {
        id: 1,
        phase: TouchPhase::Moved,
        position: [4.0, 5.0],
        force: None,
    }));
    assert_eq!(events.len(), 2, "the touch and one pointer move");
    assert!(matches!(events[1], EguiEvent::PointerMoved(_)));
}

#[test]
fn a_cancelled_touch_also_releases_the_pointer() {
    let events = at_one(InputEvent::Touch(TouchEvent {
        id: 1,
        phase: TouchPhase::Cancelled,
        position: [1.0, 1.0],
        force: None,
    }));
    assert!(
        matches!(events.last(), Some(EguiEvent::PointerGone)),
        "a cancelled touch is not a hover"
    );
}

#[test]
fn a_preedit_range_is_remeasured_in_characters() {
    // "héllo": `é` is two bytes, so byte offsets and character offsets
    // disagree from there on.
    let text = "héllo";
    let events = at_one(InputEvent::Ime(ImeEvent {
        kind: ImeKind::Preedit {
            text: text.to_string(),
            active_range: Some(1..4),
        },
    }));
    assert_eq!(
        events,
        vec![EguiEvent::Ime(EguiImeEvent::Preedit {
            text: text.to_string(),
            active_range_chars: Some(1..3),
        })],
        "egui counts characters where the crate counts bytes"
    );
}

#[test]
fn a_preedit_without_a_range_keeps_it_absent() {
    let events = at_one(InputEvent::Ime(ImeEvent {
        kind: ImeKind::Preedit {
            text: "ab".to_string(),
            active_range: None,
        },
    }));
    assert_eq!(
        events,
        vec![EguiEvent::Ime(EguiImeEvent::Preedit {
            text: "ab".to_string(),
            active_range_chars: None,
        })]
    );
}

#[test]
fn a_commit_and_a_delete_surrounding_reach_egui() {
    assert_eq!(
        at_one(InputEvent::Ime(ImeEvent {
            kind: ImeKind::Commit("done".to_string()),
        })),
        vec![EguiEvent::Ime(EguiImeEvent::Commit("done".to_string()))]
    );
    assert_eq!(
        at_one(InputEvent::Ime(ImeEvent {
            kind: ImeKind::DeleteSurrounding {
                before: 2,
                after: 3,
            },
        })),
        vec![EguiEvent::Ime(EguiImeEvent::DeleteSurrounding {
            before_chars: 2,
            after_chars: 3,
        })]
    );
}

/// egui deprecated its own on/off variants, so there is nothing to
/// forward.
#[test]
fn enabling_and_disabling_the_input_method_produce_nothing() {
    assert!(
        at_one(InputEvent::Ime(ImeEvent {
            kind: ImeKind::Enabled
        }))
        .is_empty()
    );
    assert!(
        at_one(InputEvent::Ime(ImeEvent {
            kind: ImeKind::Disabled
        }))
        .is_empty()
    );
}

#[test]
fn focus_changes_reach_egui() {
    assert_eq!(
        at_one(InputEvent::FocusChanged(false)),
        vec![EguiEvent::WindowFocused(false)]
    );
}

/// A whole frame is translated in order, and the pointer transitions keep
/// their sequence: egui's own press/release tracking depends on it.
#[test]
fn a_frame_keeps_its_event_order() {
    let events = vec![
        InputEvent::Mouse(MouseEvent::Moved {
            position: [1.0, 1.0],
        }),
        InputEvent::Mouse(MouseEvent::Button {
            position: [1.0, 1.0],
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        }),
        InputEvent::Mouse(MouseEvent::Left),
    ];
    let mut converted = Vec::new();
    to_egui_events_into(&events, 1.0, &mut converted);
    assert_eq!(
        converted,
        vec![
            EguiEvent::PointerMoved(Pos2::new(1.0, 1.0)),
            EguiEvent::PointerButton {
                pos: Pos2::new(1.0, 1.0),
                button: EguiPointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            EguiEvent::PointerGone,
        ]
    );
}

#[test]
fn an_empty_frame_converts_to_nothing() {
    let mut converted = Vec::new();
    to_egui_events_into([], 1.0, &mut converted);
    assert!(converted.is_empty());
}

/// The density must be usable, since every position is divided by it.
#[test]
#[should_panic(expected = "must be positive")]
fn a_zero_density_is_rejected() {
    let _ = at(
        InputEvent::Mouse(MouseEvent::Moved {
            position: [0.0, 0.0],
        }),
        0.0,
    );
}

/// Every key the crate names, so a new variant cannot be added without
/// either a translation or a deliberate decision to skip it.
fn every_named_key() -> Vec<Key> {
    let mut keys = vec![
        Key::Escape,
        Key::Space,
        Key::Enter,
        Key::Tab,
        Key::Backspace,
        Key::Delete,
        Key::ArrowUp,
        Key::ArrowDown,
        Key::ArrowLeft,
        Key::ArrowRight,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
    ];
    keys.extend([
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ]);
    keys.extend([
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ]);
    keys.extend([
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
        Key::F25,
        Key::F26,
        Key::F27,
        Key::F28,
        Key::F29,
        Key::F30,
        Key::F31,
        Key::F32,
        Key::F33,
        Key::F34,
        Key::F35,
    ]);
    keys
}
