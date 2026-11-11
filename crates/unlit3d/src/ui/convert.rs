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
//! # Which event feeds egui's pointer
//!
//! One physical action can arrive in several categories, because the crate
//! reports it once per level of detail. egui's pointer is fed from the
//! **most detailed** source that exists for the device, and the coarser
//! duplicate is dropped:
//!
//! | device | events | what egui's pointer is fed from |
//! |---|---|---|
//! | mouse | [`MouseEvent`], [`PointerEvent`] | [`MouseEvent`], which names the button and the wheel unit |
//! | touch | [`TouchEvent`], [`PointerEvent`] | [`TouchEvent`], which names the touch and reports the lift ordering |
//! | pen and other | [`PointerEvent`] | [`PointerEvent`], there being nothing more detailed |
//!
//! # Touches
//!
//! egui's own documentation asks an integration to report a touch point
//! *both* as a touch and as the pointer: a touch is what a widget watching for
//! a finger wants, while the pointer events are what makes tapping a button
//! work at all. One touch therefore becomes several events, which the
//! conversion appends to the caller's buffer rather than returning one.

use egui::TouchPhase as EguiTouchPhase;
use egui::{Event as EguiEvent, MouseWheelUnit, Pos2, TouchDeviceId, TouchId};
use egui::{ImeEvent as EguiImeEvent, PointerButton as EguiPointerButton};

use crate::input::{
    ImeEvent, ImeKind, InputEvent, Key, KeyEvent, Modifiers, MouseButton, MouseEvent,
    PointerAction, PointerEvent, PointerKind, TouchEvent, TouchPhase, WheelUnit,
};

/// Translate one crate event into the egui events it stands for, appending
/// them to `out`.
///
/// Appends nothing for an event egui has no counterpart for, so a caller can
/// translate a whole frame without matching on the kind first. A pointer
/// event that describes an action another event of the same frame already
/// describes in more detail is likewise dropped — see the module docs for which
/// source feeds egui's pointer per device. `pixels_per_point` converts the
/// crate's physical pixels into the points egui lays out in; it must be
/// positive. Keeping the buffer with the caller means a frame — or a run of
/// frames — reuses one allocation rather than allocating per event.
pub fn to_egui_event(event: &InputEvent, pixels_per_point: f32, out: &mut Vec<EguiEvent>) {
    assert!(
        pixels_per_point > 0.0,
        "a pixel density must be positive to convert physical pixels into points"
    );
    match event {
        InputEvent::Key(event) => key_events(*event, out),
        InputEvent::Mouse(event) => mouse_events(event, pixels_per_point, out),
        InputEvent::Pointer(event) => pointer_events(event, pixels_per_point, out),
        InputEvent::Touch(event) => touch_events(*event, pixels_per_point, out),
        InputEvent::Text(text) => out.push(EguiEvent::Text(text.0.clone())),
        InputEvent::Ime(event) => ime_events(event, out),
        InputEvent::ModifiersChanged(modifiers) => {
            out.push(EguiEvent::ModifiersChanged(to_modifiers(*modifiers)));
        }
        InputEvent::FocusChanged(focused) => out.push(EguiEvent::WindowFocused(*focused)),
        // egui learns the screen's size from the frame's target and its
        // density from the viewport, not from events, so these carry nothing
        // it needs.
        InputEvent::Resized { .. } | InputEvent::ScaleFactorChanged { .. } => {}
    }
}

/// Translate a whole frame's events into `out`, appending them.
///
/// The order events arrive in is preserved: egui's own input handling depends
/// on a press coming before the release that follows it. The caller clears
/// `out` first and keeps it across frames, so a run of frames reuses one
/// allocation for the events egui is handed.
pub fn to_egui_events_into<'a>(
    events: impl IntoIterator<Item = &'a InputEvent>,
    pixels_per_point: f32,
    out: &mut Vec<EguiEvent>,
) {
    for event in events {
        to_egui_event(event, pixels_per_point, out);
    }
}

/// A key event. The crate carries the text a key produced as a separate
/// [`TextEvent`](crate::input::TextEvent), so a key press becomes one egui event here and the text
/// arrives on its own.
fn key_events(event: KeyEvent, out: &mut Vec<EguiEvent>) {
    // A key only the platform knows has no name in egui's closed key space, so
    // there is nothing to forward. Dropping the event is better than picking a
    // key egui does recognize, which would report a press the user never made.
    let Some(key) = to_key(event.key) else {
        return;
    };
    // `egui` wants the logical key, which its shortcuts are written against,
    // and the physical key, which a game control addresses. The crate's `Key`
    // is a physical key, so it fills both: a key the crate names has the same
    // identity either way, and the crate does not track the layout-dependent
    // logical key at all.
    out.push(EguiEvent::Key {
        key,
        physical_key: Some(key),
        pressed: event.pressed,
        repeat: event.repeat,
        modifiers: to_modifiers(event.modifiers),
    });
}

/// A mouse event.
///
/// A mouse feeds egui's pointer through its own events rather than through the
/// device-agnostic [`PointerEvent`] beside them: a [`MouseEvent::Button`] names
/// the button egui needs, and a [`MouseEvent::Wheel`] names the unit, neither
/// of which a pointer event carries.
fn mouse_events(event: &MouseEvent, pixels_per_point: f32, out: &mut Vec<EguiEvent>) {
    match event {
        MouseEvent::Moved { position } => {
            out.push(EguiEvent::PointerMoved(to_pos(*position, pixels_per_point)));
        }
        MouseEvent::Button {
            position,
            button,
            pressed,
            modifiers,
        } => {
            // egui names five buttons and nothing else, so a button beyond
            // them cannot be reported. Appending nothing says so rather than
            // mapping it onto one egui does know.
            let Some(button) = to_pointer_button(*button) else {
                return;
            };
            out.push(EguiEvent::PointerButton {
                pos: to_pos(*position, pixels_per_point),
                button,
                pressed: *pressed,
                modifiers: to_modifiers(*modifiers),
            });
        }
        MouseEvent::Left => out.push(EguiEvent::PointerGone),
        MouseEvent::Wheel {
            delta,
            unit,
            phase,
            modifiers,
        } => out.push(EguiEvent::MouseWheel {
            unit: to_wheel_unit(*unit),
            // egui's wheel delta is in points whichever unit it is measured
            // in, so a pixel delta is scaled like a position.
            delta: match unit {
                WheelUnit::Pixel => egui::Vec2::new(delta[0], delta[1]) / pixels_per_point,
                WheelUnit::Line | WheelUnit::Page => egui::Vec2::new(delta[0], delta[1]),
            },
            phase: to_touch_phase(*phase),
            // winit reports the delta and the phase but not what produced
            // them, so egui is told the source is unknown rather than handed
            // a guess that would change how a scroll area behaves.
            source: egui::MouseWheelSource::Unknown,
            modifiers: to_modifiers(*modifiers),
        }),
    }
}

/// A pointer event, in egui's point space.
///
/// A gesture has no point counterpart at any other level, so it always reaches
/// egui. The point actions do not: only a device egui hears about through this
/// level alone — a pen, or a device the platform did not classify — reaches
/// egui's pointer this way, because a mouse and a touch produce a
/// [`MouseEvent`] or a [`TouchEvent`] carrying the same point action in more
/// detail and egui is fed that instead. Forwarding the pointer too would report
/// one physical move twice.
fn pointer_events(event: &PointerEvent, pixels_per_point: f32, out: &mut Vec<EguiEvent>) {
    match event.action {
        PointerAction::Zoom(delta) => {
            out.push(EguiEvent::Zoom(delta));
            return;
        }
        PointerAction::Rotate(delta) => {
            out.push(EguiEvent::Rotate(delta));
            return;
        }
        PointerAction::Moved
        | PointerAction::Pressed
        | PointerAction::Released { .. }
        | PointerAction::Left => {}
    }
    let Some(position) = pointer_position(event, pixels_per_point) else {
        return;
    };
    match event.action {
        PointerAction::Moved => out.push(EguiEvent::PointerMoved(position)),
        PointerAction::Pressed => out.push(EguiEvent::PointerButton {
            pos: position,
            button: EguiPointerButton::Primary,
            pressed: true,
            modifiers: to_modifiers(event.modifiers),
        }),
        PointerAction::Released { .. } => {
            out.push(EguiEvent::PointerButton {
                pos: position,
                button: EguiPointerButton::Primary,
                pressed: false,
                modifiers: to_modifiers(event.modifiers),
            });
            // A pen that lifts or is cancelled is not hovering any more, so it
            // leaves the screen; the mouse's own `Left` covers the mouse.
            out.push(EguiEvent::PointerGone);
        }
        PointerAction::Left => out.push(EguiEvent::PointerGone),
        PointerAction::Zoom(_) | PointerAction::Rotate(_) => unreachable!("handled above"),
    }
}

/// The position a pointer event's point action is reported at, or `None` when
/// the event must not reach egui's pointer at all.
///
/// A mouse, a touch and a trackpad are that case: each moves the pointer
/// through the same stream that already produces a [`MouseEvent`] or a
/// [`TouchEvent`], so only a device with no such event — a pen, or one the
/// platform did not classify — reaches egui here. The position falls back to
/// the origin for an action that carries none, which egui treats as a pointer
/// at the top-left corner rather than as no pointer at all.
fn pointer_position(event: &PointerEvent, pixels_per_point: f32) -> Option<Pos2> {
    match event.kind {
        PointerKind::Mouse | PointerKind::Touch | PointerKind::Trackpad => return None,
        PointerKind::Pen | PointerKind::Other => {}
    }
    Some(
        event
            .position
            .map_or(Pos2::ZERO, |position| to_pos(position, pixels_per_point)),
    )
}

/// A touch, reported as egui asks: the touch itself plus the pointer events
/// that make a widget respond to it.
fn touch_events(event: TouchEvent, pixels_per_point: f32, out: &mut Vec<EguiEvent>) {
    let pos = to_pos(event.position, pixels_per_point);
    out.push(EguiEvent::Touch {
        // The crate numbers touches per device without naming the device, so
        // every touch belongs to one synthetic device.
        device_id: TouchDeviceId(0),
        id: TouchId(event.id),
        phase: to_touch_phase(event.phase),
        pos,
        force: event.force,
    });

    match event.phase {
        // A finger down is the pointer arriving and pressing its primary
        // button, which is what a tap on a widget is.
        TouchPhase::Started => {
            out.push(EguiEvent::PointerMoved(pos));
            out.push(EguiEvent::PointerButton {
                pos,
                button: EguiPointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default().into(),
            });
        }
        TouchPhase::Moved => out.push(EguiEvent::PointerMoved(pos)),
        // Lifting ends the touch, and egui wants the release before the
        // pointer leaves the screen.
        TouchPhase::Ended | TouchPhase::Cancelled => {
            out.push(EguiEvent::PointerButton {
                pos,
                button: EguiPointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default().into(),
            });
            // Nothing follows the last finger, so the pointer is gone. A
            // cancellation is not a hover, so it leaves the screen too.
            out.push(EguiEvent::PointerGone);
        }
    }
}

/// An input-method event.
///
/// The two sides measure text differently: the crate counts bytes because a
/// windowing library reports byte offsets, while egui counts characters. A
/// preedit's active range is therefore re-measured in characters here, and a
/// delete request is converted the other way.
fn ime_events(event: &ImeEvent, out: &mut Vec<EguiEvent>) {
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
        ImeKind::Enabled | ImeKind::Disabled => return,
    };
    out.push(EguiEvent::Ime(inner));
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

/// The crate's mouse button as egui's, or `None` for a button egui does not
/// name. The crate names the two side buttons and egui numbers them, in the
/// same order.
fn to_pointer_button(button: MouseButton) -> Option<EguiPointerButton> {
    Some(match button {
        MouseButton::Primary => EguiPointerButton::Primary,
        MouseButton::Secondary => EguiPointerButton::Secondary,
        MouseButton::Middle => EguiPointerButton::Middle,
        MouseButton::Back => EguiPointerButton::Extra1,
        MouseButton::Forward => EguiPointerButton::Extra2,
        MouseButton::Other(_) => return None,
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
mod tests;
