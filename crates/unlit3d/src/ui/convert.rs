//! Translating the crate's portable input events into egui's.
//!
//! [`crate::input`] keeps input free of any windowing library or UI toolkit;
//! this module is the one place the two meet, so the rest of the UI code never
//! handles an egui event directly and the crate's events stay usable by game
//! logic on their own.
//!
//! # Units
//!
//! The crate reports pointer positions in physical pixels from the window's
//! top-left; egui works in logical points. The conversion therefore takes the
//! density and divides, and every position it produces is already in the space
//! egui expects. A consumer that feeds egui must not scale again.
//!
//! # Touches
//!
//! egui's own documentation asks an integration to report a touch point
//! *both* as a touch and as the pointer: a touch is what a widget watching for
//! a finger wants, while the pointer events are what makes tapping a button
//! work at all. One touch therefore becomes several events, which is why the
//! conversion returns a list rather than one event.

use egui::TouchPhase as EguiTouchPhase;
use egui::{Event as EguiEvent, MouseWheelUnit, Pos2, TouchDeviceId, TouchId};
use egui::{ImeEvent as EguiImeEvent, PointerButton as EguiPointerButton};

use crate::input::{
    ImeEvent, ImeKind, InputEvent, Key, KeyEvent, Modifiers, PointerButton, PointerEvent,
    TouchEvent, TouchPhase, WheelUnit,
};

/// Translate one crate event into the egui events it stands for.
///
/// Returns an empty list for an event egui has no counterpart for, so a caller
/// can translate a whole frame without matching on the kind first. `pixels_per_point`
/// converts the crate's physical pixels into the points egui lays out in; it
/// must be positive.
pub fn to_egui_event(event: &InputEvent, pixels_per_point: f32) -> Vec<EguiEvent> {
    assert!(
        pixels_per_point > 0.0,
        "a pixel density must be positive to convert physical pixels into points"
    );
    match event {
        InputEvent::Key(event) => key_events(*event),
        InputEvent::Pointer(event) => pointer_events(event, pixels_per_point),
        InputEvent::Touch(event) => touch_events(*event, pixels_per_point),
        InputEvent::Text(text) => vec![EguiEvent::Text(text.0.clone())],
        InputEvent::Ime(event) => ime_events(event),
        InputEvent::ModifiersChanged(modifiers) => {
            vec![EguiEvent::ModifiersChanged(to_modifiers(*modifiers))]
        }
        InputEvent::FocusChanged(focused) => vec![EguiEvent::WindowFocused(*focused)],
    }
}

/// Translate a whole frame's events.
///
/// The order events arrive in is preserved: egui's own input handling depends
/// on a press coming before the release that follows it.
pub fn to_egui_events<'a>(
    events: impl IntoIterator<Item = &'a InputEvent>,
    pixels_per_point: f32,
) -> Vec<EguiEvent> {
    events
        .into_iter()
        .flat_map(|event| to_egui_event(event, pixels_per_point))
        .collect()
}

/// A key event. The crate carries the text a key produced as a separate
/// [`TextEvent`], so a key press becomes one egui event here and the text
/// arrives on its own.
fn key_events(event: KeyEvent) -> Vec<EguiEvent> {
    // A key only the platform knows has no name in egui's closed key space, so
    // there is nothing to forward. Dropping the event is better than picking a
    // key egui does recognize, which would report a press the user never made.
    let Some(key) = to_key(event.key) else {
        return Vec::new();
    };
    // `egui` wants the logical key, which its shortcuts are written against,
    // and the physical key, which a game control addresses. The crate's `Key`
    // is a physical key, so it fills both: a key the crate names has the same
    // identity either way, and the crate does not track the layout-dependent
    // logical key at all.
    vec![EguiEvent::Key {
        key,
        physical_key: Some(key),
        pressed: event.pressed,
        repeat: event.repeat,
        modifiers: to_modifiers(event.modifiers),
    }]
}

/// A pointer event, in egui's point space.
fn pointer_events(event: &PointerEvent, pixels_per_point: f32) -> Vec<EguiEvent> {
    match event {
        PointerEvent::Moved { position } => {
            vec![EguiEvent::PointerMoved(to_pos(*position, pixels_per_point))]
        }
        PointerEvent::Button {
            position,
            button,
            pressed,
            modifiers,
        } => {
            // egui names five buttons and nothing else, so a button beyond
            // them cannot be reported. An empty list says so rather than
            // mapping it onto one egui does know.
            let Some(button) = to_pointer_button(*button) else {
                return Vec::new();
            };
            vec![EguiEvent::PointerButton {
                pos: to_pos(*position, pixels_per_point),
                button,
                pressed: *pressed,
                modifiers: to_modifiers(*modifiers),
            }]
        }
        PointerEvent::Left => vec![EguiEvent::PointerGone],
        PointerEvent::Wheel {
            delta,
            unit,
            phase,
            modifiers,
        } => vec![EguiEvent::MouseWheel {
            unit: to_wheel_unit(*unit),
            // egui's wheel delta is in points whichever unit it is measured
            // in, so a pixel delta is scaled like a position.
            delta: match unit {
                WheelUnit::Pixel => egui::Vec2::new(delta[0], delta[1]) / pixels_per_point,
                WheelUnit::Line | WheelUnit::Page => egui::Vec2::new(delta[0], delta[1]),
            },
            phase: to_touch_phase(*phase),
            modifiers: to_modifiers(*modifiers),
        }],
        PointerEvent::Zoom(delta) => vec![EguiEvent::Zoom(*delta)],
        PointerEvent::Rotate(delta) => vec![EguiEvent::Rotate(*delta)],
    }
}

/// A touch, reported as egui asks: the touch itself plus the pointer events
/// that make a widget respond to it.
fn touch_events(event: TouchEvent, pixels_per_point: f32) -> Vec<EguiEvent> {
    let pos = to_pos(event.position, pixels_per_point);
    let mut events = vec![EguiEvent::Touch {
        // The crate numbers touches per device without naming the device, so
        // every touch belongs to one synthetic device.
        device_id: TouchDeviceId(0),
        id: TouchId(event.id),
        phase: to_touch_phase(event.phase),
        pos,
        force: event.force,
    }];

    match event.phase {
        // A finger down is the pointer arriving and pressing its primary
        // button, which is what a tap on a widget is.
        TouchPhase::Started => {
            events.push(EguiEvent::PointerMoved(pos));
            events.push(EguiEvent::PointerButton {
                pos,
                button: EguiPointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default().into(),
            });
        }
        TouchPhase::Moved => events.push(EguiEvent::PointerMoved(pos)),
        // Lifting ends the touch, and egui wants the release before the
        // pointer leaves the screen.
        TouchPhase::Ended | TouchPhase::Cancelled => {
            events.push(EguiEvent::PointerButton {
                pos,
                button: EguiPointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default().into(),
            });
            // Nothing follows the last finger, so the pointer is gone. A
            // cancellation is not a hover, so it leaves the screen too.
            events.push(EguiEvent::PointerGone);
        }
    }
    events
}

/// An input-method event.
///
/// The two sides measure text differently: the crate counts bytes because a
/// windowing library reports byte offsets, while egui counts characters. A
/// preedit's active range is therefore re-measured in characters here, and a
/// delete request is converted the other way.
fn ime_events(event: &ImeEvent) -> Vec<EguiEvent> {
    let inner = match &event.kind {
        ImeKind::Preedit { text, active_range } => EguiImeEvent::Preedit {
            text: text.clone(),
            active_range_chars: active_range
                .clone()
                .map(|range| bytes_to_chars(text, range)),
        },
        ImeKind::Commit(text) => EguiImeEvent::Commit(text.clone()),
        ImeKind::DeleteSurrounding { before, after } => {
            // Without the surrounding text there is no way to know how many
            // characters a byte count covers, so the counts are passed through
            // as the crate stated them. A consumer that produced them from
            // characters (which the crate's own docs allow) is exact.
            EguiImeEvent::DeleteSurrounding {
                before_chars: *before,
                after_chars: *after,
            }
        }
        // egui deprecated both variants: it no longer tracks whether an input
        // method is on, so neither has a counterpart to forward.
        ImeKind::Enabled | ImeKind::Disabled => return Vec::new(),
    };
    vec![EguiEvent::Ime(inner)]
}

/// Re-express a byte range within `text` as the character range covering the
/// same span.
///
/// A byte offset that lands inside a character is not a boundary the text has,
/// so it is rounded to the nearest boundary that exists rather than panicking.
fn bytes_to_chars(text: &str, range: core::ops::Range<usize>) -> core::ops::Range<usize> {
    let start = text
        .char_indices()
        .position(|(offset, _)| offset >= range.start)
        .unwrap_or_else(|| text.chars().count());
    let end = text
        .char_indices()
        .position(|(offset, _)| offset >= range.end)
        .unwrap_or_else(|| text.chars().count());
    start..end
}

/// A position in physical pixels as egui's logical point.
fn to_pos(position: [f32; 2], pixels_per_point: f32) -> Pos2 {
    Pos2::new(
        position[0] / pixels_per_point,
        position[1] / pixels_per_point,
    )
}

/// The crate's modifier set as egui's, field for field.
fn to_modifiers(modifiers: Modifiers) -> egui::Modifiers {
    modifiers.into()
}

impl From<Modifiers> for egui::Modifiers {
    fn from(modifiers: Modifiers) -> Self {
        Self {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: modifiers.mac_cmd,
            command: modifiers.command,
        }
    }
}

/// The crate's wheel unit as egui's. The crate has no point unit, and a
/// pixel is what a trackpad reports, which is what egui calls a point.
fn to_wheel_unit(unit: WheelUnit) -> MouseWheelUnit {
    match unit {
        WheelUnit::Line => MouseWheelUnit::Line,
        WheelUnit::Page => MouseWheelUnit::Page,
        WheelUnit::Pixel => MouseWheelUnit::Point,
    }
}

/// The crate's pointer button as egui's, or `None` for a button egui does not
/// name. The crate names the two side buttons and egui numbers them, in the
/// same order.
fn to_pointer_button(button: PointerButton) -> Option<EguiPointerButton> {
    Some(match button {
        PointerButton::Primary => EguiPointerButton::Primary,
        PointerButton::Secondary => EguiPointerButton::Secondary,
        PointerButton::Middle => EguiPointerButton::Middle,
        PointerButton::Back => EguiPointerButton::Extra1,
        PointerButton::Forward => EguiPointerButton::Extra2,
        PointerButton::Other(_) => return None,
    })
}

/// A gesture phase in egui's spelling: the crate's `Ended` is egui's `End`.
fn to_touch_phase(phase: TouchPhase) -> EguiTouchPhase {
    match phase {
        TouchPhase::Started => EguiTouchPhase::Start,
        TouchPhase::Moved => EguiTouchPhase::Move,
        TouchPhase::Ended => EguiTouchPhase::End,
        TouchPhase::Cancelled => EguiTouchPhase::Cancel,
    }
}

/// The crate's key as egui's, or `None` for a key egui cannot name.
///
/// Every key the crate names has an egui counterpart, so this is a plain
/// rename except for [`Key::Other`]: egui's key space is closed and has no
/// "some other key" variant, so a platform key stays untranslatable.
fn to_key(key: Key) -> Option<egui::Key> {
    Some(match key {
        Key::Escape => egui::Key::Escape,
        Key::Space => egui::Key::Space,
        Key::Enter => egui::Key::Enter,
        Key::Tab => egui::Key::Tab,
        Key::Backspace => egui::Key::Backspace,
        Key::Delete => egui::Key::Delete,
        Key::ArrowUp => egui::Key::ArrowUp,
        Key::ArrowDown => egui::Key::ArrowDown,
        Key::ArrowLeft => egui::Key::ArrowLeft,
        Key::ArrowRight => egui::Key::ArrowRight,
        Key::Home => egui::Key::Home,
        Key::End => egui::Key::End,
        Key::PageUp => egui::Key::PageUp,
        Key::PageDown => egui::Key::PageDown,
        Key::A => egui::Key::A,
        Key::B => egui::Key::B,
        Key::C => egui::Key::C,
        Key::D => egui::Key::D,
        Key::E => egui::Key::E,
        Key::F => egui::Key::F,
        Key::G => egui::Key::G,
        Key::H => egui::Key::H,
        Key::I => egui::Key::I,
        Key::J => egui::Key::J,
        Key::K => egui::Key::K,
        Key::L => egui::Key::L,
        Key::M => egui::Key::M,
        Key::N => egui::Key::N,
        Key::O => egui::Key::O,
        Key::P => egui::Key::P,
        Key::Q => egui::Key::Q,
        Key::R => egui::Key::R,
        Key::S => egui::Key::S,
        Key::T => egui::Key::T,
        Key::U => egui::Key::U,
        Key::V => egui::Key::V,
        Key::W => egui::Key::W,
        Key::X => egui::Key::X,
        Key::Y => egui::Key::Y,
        Key::Z => egui::Key::Z,
        Key::Num0 => egui::Key::Num0,
        Key::Num1 => egui::Key::Num1,
        Key::Num2 => egui::Key::Num2,
        Key::Num3 => egui::Key::Num3,
        Key::Num4 => egui::Key::Num4,
        Key::Num5 => egui::Key::Num5,
        Key::Num6 => egui::Key::Num6,
        Key::Num7 => egui::Key::Num7,
        Key::Num8 => egui::Key::Num8,
        Key::Num9 => egui::Key::Num9,
        Key::F1 => egui::Key::F1,
        Key::F2 => egui::Key::F2,
        Key::F3 => egui::Key::F3,
        Key::F4 => egui::Key::F4,
        Key::F5 => egui::Key::F5,
        Key::F6 => egui::Key::F6,
        Key::F7 => egui::Key::F7,
        Key::F8 => egui::Key::F8,
        Key::F9 => egui::Key::F9,
        Key::F10 => egui::Key::F10,
        Key::F11 => egui::Key::F11,
        Key::F12 => egui::Key::F12,
        Key::F13 => egui::Key::F13,
        Key::F14 => egui::Key::F14,
        Key::F15 => egui::Key::F15,
        Key::F16 => egui::Key::F16,
        Key::F17 => egui::Key::F17,
        Key::F18 => egui::Key::F18,
        Key::F19 => egui::Key::F19,
        Key::F20 => egui::Key::F20,
        Key::F21 => egui::Key::F21,
        Key::F22 => egui::Key::F22,
        Key::F23 => egui::Key::F23,
        Key::F24 => egui::Key::F24,
        Key::F25 => egui::Key::F25,
        Key::F26 => egui::Key::F26,
        Key::F27 => egui::Key::F27,
        Key::F28 => egui::Key::F28,
        Key::F29 => egui::Key::F29,
        Key::F30 => egui::Key::F30,
        Key::F31 => egui::Key::F31,
        Key::F32 => egui::Key::F32,
        Key::F33 => egui::Key::F33,
        Key::F34 => egui::Key::F34,
        Key::F35 => egui::Key::F35,
        Key::Other(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::TextEvent;

    /// One event's translation at a density of one, where points and pixels
    /// coincide.
    fn at_one(event: InputEvent) -> Vec<EguiEvent> {
        to_egui_event(&event, 1.0)
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
    fn a_pointer_position_is_converted_to_points() {
        let events = to_egui_event(
            &InputEvent::Pointer(PointerEvent::Moved {
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
    fn a_pointer_button_keeps_its_position_button_and_state() {
        let events = to_egui_event(
            &InputEvent::Pointer(PointerEvent::Button {
                position: [10.0, 30.0],
                button: PointerButton::Secondary,
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
    fn every_pointer_button_maps_to_its_own_egui_button() {
        let buttons = [
            (PointerButton::Primary, EguiPointerButton::Primary),
            (PointerButton::Secondary, EguiPointerButton::Secondary),
            (PointerButton::Middle, EguiPointerButton::Middle),
            (PointerButton::Back, EguiPointerButton::Extra1),
            (PointerButton::Forward, EguiPointerButton::Extra2),
        ];
        for (ours, theirs) in buttons {
            assert_eq!(to_pointer_button(ours), Some(theirs));
        }
        // The two side buttons must not collapse onto one.
        assert_ne!(
            to_pointer_button(PointerButton::Back),
            to_pointer_button(PointerButton::Forward)
        );
        assert_eq!(to_pointer_button(PointerButton::Other(7)), None);
    }

    #[test]
    fn a_pointer_leaving_the_window_becomes_pointer_gone() {
        assert_eq!(
            at_one(InputEvent::Pointer(PointerEvent::Left)),
            vec![EguiEvent::PointerGone]
        );
    }

    #[test]
    fn a_wheel_line_delta_passes_through_unscaled() {
        let events = at_one(InputEvent::Pointer(PointerEvent::Wheel {
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
                modifiers: egui::Modifiers::default(),
            }]
        );
    }

    #[test]
    fn a_wheel_pixel_delta_is_converted_to_points() {
        let events = to_egui_event(
            &InputEvent::Pointer(PointerEvent::Wheel {
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
                modifiers: egui::Modifiers::default(),
            }],
            "egui measures a point-unit delta in points, so a pixel delta is scaled"
        );
    }

    #[test]
    fn zoom_and_rotate_pass_their_amount_through() {
        assert_eq!(
            at_one(InputEvent::Pointer(PointerEvent::Zoom(1.5))),
            vec![EguiEvent::Zoom(1.5)]
        );
        assert_eq!(
            at_one(InputEvent::Pointer(PointerEvent::Rotate(0.5))),
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
            InputEvent::Pointer(PointerEvent::Moved {
                position: [1.0, 1.0],
            }),
            InputEvent::Pointer(PointerEvent::Button {
                position: [1.0, 1.0],
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Pointer(PointerEvent::Left),
        ];
        let converted = to_egui_events(&events, 1.0);
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
        assert!(to_egui_events([], 1.0).is_empty());
    }

    /// The density must be usable, since every position is divided by it.
    #[test]
    #[should_panic(expected = "must be positive")]
    fn a_zero_density_is_rejected() {
        let _ = to_egui_event(
            &InputEvent::Pointer(PointerEvent::Moved {
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
}
