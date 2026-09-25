//! Feeding a winit window's events into the world's [`InputState`].
//!
//! [`WinitInput`] is the winit half of [`crate::input`]: a caller forwards
//! every [`WindowEvent`] the window produces, and the adapter translates the
//! ones that carry input into [`InputEvent`]s, pushing each into the
//! [`InputState`] resource it spawned. It decides nothing about what the input
//! *means* — that is a behaviour component's job — so it holds no state of its
//! own beyond the resource entity.
//!
//! # Coordinates
//!
//! Positions pass through as winit reports them: physical pixels with the
//! origin at the window's top-left corner. Window pixels are what game logic
//! wants, because a click maps to the pixel under the cursor; a consumer that
//! needs logical points, UI layout for instance, divides by
//! [`InputState::scale_factor`] itself, which is why the state carries that
//! factor.
//!
//! # Which key a key event names
//!
//! A key comes from the event's *physical* key, so a control bound to `W`
//! stays on the key above `S` on every keyboard layout. A `KeyCode` this crate
//! does not name becomes [`Key::Other`] carrying the platform's own code:
//!
//! 1. the scancode, where winit exposes one through
//!    [`PhysicalKeyExtScancode`](winit::platform::scancode::PhysicalKeyExtScancode)
//!    (Windows, macOS and the free Unixes);
//! 2. otherwise the native code winit reports for a key it could not identify;
//! 3. otherwise the `KeyCode`'s discriminant, which is stable only within one
//!    winit version and is therefore the last resort.
//!
//! The event's `logical_key` is not used: a game control addressing a physical
//! key must not move with the layout. The text the key produced comes from
//! `KeyEvent::text` instead, as a separate [`TextEvent`].

use unlit_ecs::{Entity, LocalWorld, Resource};
use winit::event::{
    Ime, MouseButton, MouseScrollDelta, TouchPhase as WinitTouchPhase, WindowEvent,
};
use winit::keyboard::{KeyCode, ModifiersState, NativeKeyCode, PhysicalKey};

use super::{
    ImeEvent, ImeKind, InputEvent, InputState, Key, KeyEvent, Modifiers,
    MouseButton as CrateButton, MouseEvent, PointerAction, PointerEvent, PointerKind, TextEvent,
    TouchEvent, TouchPhase, WheelUnit,
};

/// The position a mouse event falls back to while the cursor is outside the
/// window and [`InputState::cursor`] is `None`.
const NO_CURSOR: [f32; 2] = [0.0, 0.0];

/// The pointer id of the one pointer a mouse is.
///
/// A mouse cannot be several contacts at once, so it always reports this id. A
/// gesture shares it because a gesture is not a contact either: it never
/// presses or releases, so it cannot be mistaken for the mouse being down.
const MOUSE_POINTER: u64 = 0;

/// The pointer kind a pinch or rotation gesture is attributed to.
///
/// winit reports these gestures on two platforms it does not distinguish
/// devices on: on macOS they come from an [`NSResponder`] magnification or
/// rotation event, which a trackpad sends and a gesture-capable mouse can too,
/// while on iOS they come from a touch screen's own gesture recognizers.
///
/// [`NSResponder`]: https://developer.apple.com/documentation/appkit/nsresponder
#[cfg(target_os = "macos")]
const GESTURE_KIND: PointerKind = PointerKind::Trackpad;
/// The pointer kind a pinch or rotation gesture is attributed to.
///
/// See the macOS definition: everywhere else winit reports these gestures they
/// come from a touch screen.
#[cfg(not(target_os = "macos"))]
const GESTURE_KIND: PointerKind = PointerKind::Touch;

/// The code [`Key::Other`] carries for a key winit identified neither as a
/// `KeyCode` nor by a native code. Two such keys are indistinguishable, but
/// there is nothing left to tell them apart by.
const UNIDENTIFIED_CODE: u32 = 0;

/// Feeds a winit window's events into the world's [`InputState`].
///
/// The adapter is cheap to copy around: it holds only the entity of the
/// resource it writes, so a frame loop can keep it beside the window.
pub struct WinitInput {
    entity: Entity,
}

impl WinitInput {
    /// Spawn the `InputState` resource this adapter feeds and return it.
    ///
    /// The world must not already carry an [`InputState`] from somewhere else:
    /// two of them would split the input stream, and
    /// [`dispatch_input`](super::dispatch_input) reads only the first it finds.
    pub fn new(world: &mut LocalWorld) -> Self {
        let entity = world.spawn((Resource, InputState::default()));
        Self { entity }
    }

    /// The `InputState` resource entity.
    pub fn state(&self) -> Entity {
        self.entity
    }

    /// Translate the parts of `event` that carry input and push them.
    ///
    /// Returns whether anything was pushed. An event that carries no input —
    /// [`WindowEvent::RedrawRequested`], a theme change — is ignored, and an
    /// event that only updates device state without a listener caring,
    /// resizing for instance, pushes nothing. Neither panics.
    ///
    /// One physical action can become several events, because the categories
    /// overlap: a mouse move is a [`MouseEvent`] *and* a [`PointerEvent`], and
    /// a touch is a [`TouchEvent`] *and* a [`PointerEvent`]. That is what lets
    /// a device-agnostic drag and a mouse-only one both be written against the
    /// same frame.
    pub fn on_window_event(&self, world: &LocalWorld, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput { event, .. } => self.on_key(
                world,
                event.physical_key,
                event.state.is_pressed(),
                event.repeat,
                event.text.as_deref(),
            ),
            WindowEvent::ModifiersChanged(modifiers) => self.emit(
                world,
                InputEvent::ModifiersChanged(to_modifiers(modifiers.state())),
            ),
            WindowEvent::CursorMoved { position, .. } => {
                let position = [position.x as f32, position.y as f32];
                self.push_all(
                    world,
                    [
                        InputEvent::Mouse(MouseEvent::Moved { position }),
                        InputEvent::Pointer(PointerEvent {
                            kind: PointerKind::Mouse,
                            id: MOUSE_POINTER,
                            action: PointerAction::Moved,
                            position: Some(position),
                            modifiers: self.modifiers(world),
                        }),
                    ],
                )
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state.is_pressed();
                let button = to_button(*button);
                world
                    .with_mut::<InputState, _>(self.entity, |state| {
                        let position = state.cursor.unwrap_or(NO_CURSOR);
                        let modifiers = state.modifiers;
                        state.push(InputEvent::Mouse(MouseEvent::Button {
                            position,
                            button,
                            pressed,
                            modifiers,
                        }));
                        state.push(InputEvent::Pointer(PointerEvent {
                            kind: PointerKind::Mouse,
                            id: MOUSE_POINTER,
                            action: if pressed {
                                PointerAction::Pressed
                            } else {
                                PointerAction::Released { cancelled: false }
                            },
                            position: Some(position),
                            modifiers,
                        }));
                    })
                    .is_some()
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let (delta, unit) = to_wheel(delta);
                let phase = to_phase(*phase);
                world
                    .with_mut::<InputState, _>(self.entity, |state| {
                        // A wheel step is a mouse's alone: a touch produces no
                        // wheel event, so there is no pointer event to pair it
                        // with.
                        state.push(InputEvent::Mouse(MouseEvent::Wheel {
                            delta,
                            unit,
                            phase,
                            modifiers: state.modifiers,
                        }));
                    })
                    .is_some()
            }
            // A pinch or rotation is a gesture, not a button and not a touch
            // point, so it enters the stream as a pointer action. The touch
            // points a two-finger gesture is built from arrive separately as
            // their own `WindowEvent::Touch`es.
            WindowEvent::PinchGesture { delta, .. } => self.emit(
                world,
                InputEvent::Pointer(PointerEvent {
                    kind: GESTURE_KIND,
                    // A gesture is not a contact, so it shares the mouse's id.
                    id: MOUSE_POINTER,
                    action: PointerAction::Zoom(*delta as f32),
                    position: None,
                    modifiers: self.modifiers(world),
                }),
            ),
            WindowEvent::RotationGesture { delta, .. } => self.emit(
                world,
                InputEvent::Pointer(PointerEvent {
                    kind: GESTURE_KIND,
                    // A gesture is not a contact, so it shares the mouse's id.
                    id: MOUSE_POINTER,
                    action: PointerAction::Rotate(*delta),
                    position: None,
                    modifiers: self.modifiers(world),
                }),
            ),
            WindowEvent::Touch(touch) => {
                let phase = to_phase(touch.phase);
                let position = [touch.location.x as f32, touch.location.y as f32];
                let (action, pointer_position) = pointer_of_touch(phase, position);
                self.push_all(
                    world,
                    [
                        InputEvent::Touch(TouchEvent {
                            id: touch.id,
                            phase,
                            position,
                            force: touch.force.map(|force| force.normalized() as f32),
                        }),
                        InputEvent::Pointer(PointerEvent {
                            kind: PointerKind::Touch,
                            id: touch.id,
                            action,
                            position: pointer_position,
                            modifiers: self.modifiers(world),
                        }),
                    ],
                )
            }
            WindowEvent::Ime(ime) => self.emit(
                world,
                InputEvent::Ime(ImeEvent {
                    kind: to_ime_kind(ime),
                }),
            ),
            WindowEvent::Focused(focused) => self.emit(world, InputEvent::FocusChanged(*focused)),
            WindowEvent::CursorLeft { .. } => self.push_all(
                world,
                [
                    // The pointer leaves with the mouse: a touch that is still
                    // down on the screen is not gone because the cursor left the
                    // canvas.
                    InputEvent::Mouse(MouseEvent::Left),
                    InputEvent::Pointer(PointerEvent {
                        kind: PointerKind::Mouse,
                        id: MOUSE_POINTER,
                        action: PointerAction::Left,
                        position: None,
                        modifiers: self.modifiers(world),
                    }),
                ],
            ),
            // These two carry no event of their own; they only refresh the
            // state later events read from.
            WindowEvent::Resized(size) => {
                let _ = world.with_mut::<InputState, _>(self.entity, |state| {
                    state.set_size_px(size.width, size.height);
                });
                false
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor(world, *scale_factor)
            }
            _ => false,
        }
    }

    /// The translation [`WindowEvent::KeyboardInput`] performs.
    ///
    /// It is split out because winit does not let another crate build a
    /// [`KeyEvent`](winit::event::KeyEvent) — its platform-specific field is
    /// private — so a caller of this adapter can only be tested through the
    /// pieces of it that are addressable, and a unit test has to drive those
    /// directly.
    fn on_key(
        &self,
        world: &LocalWorld,
        physical_key: PhysicalKey,
        pressed: bool,
        repeat: bool,
        text: Option<&str>,
    ) -> bool {
        world
            .with_mut::<InputState, _>(self.entity, |state| {
                state.push(InputEvent::Key(KeyEvent {
                    key: key_of(physical_key),
                    pressed,
                    repeat,
                    modifiers: state.modifiers,
                }));
                // A key press also carries the character the layout produced
                // there; `logical_key` is deliberately not consulted, because
                // `text` already has that character, alone or with the dead
                // key that preceded it.
                if let (true, Some(text)) = (pressed, text) {
                    state.push(InputEvent::Text(TextEvent(text.to_owned())));
                }
            })
            .is_some()
    }

    /// The translation [`WindowEvent::ScaleFactorChanged`] performs.
    ///
    /// Split out for the same reason as [`Self::on_key`]: winit's event carries
    /// an [`InnerSizeWriter`](winit::event::InnerSizeWriter) that no other
    /// crate can build. The window size that changes with the factor arrives as
    /// its own [`WindowEvent::Resized`].
    fn on_scale_factor(&self, world: &LocalWorld, scale_factor: f64) -> bool {
        let _ = world.with_mut::<InputState, _>(self.entity, |state| {
            state.set_scale_factor(scale_factor as f32);
        });
        false
    }

    /// Push one event and report that something was pushed.
    fn emit(&self, world: &LocalWorld, event: InputEvent) -> bool {
        world
            .with_mut::<InputState, _>(self.entity, |state| state.push(event))
            .is_some()
    }

    /// Push several events from one window event under a single borrow.
    ///
    /// The events keep the order they are given in, which is what lets a
    /// device-specific event precede the device-agnostic one describing the
    /// same action.
    fn push_all(&self, world: &LocalWorld, events: impl IntoIterator<Item = InputEvent>) -> bool {
        let mut pushed = false;
        let any = world
            .with_mut::<InputState, _>(self.entity, |state| {
                for event in events {
                    state.push(event);
                    pushed = true;
                }
            })
            .is_some();
        any && pushed
    }

    /// The modifier keys held right now, for an event that does not carry its
    /// own.
    fn modifiers(&self, world: &LocalWorld) -> Modifiers {
        world
            .get::<InputState>(self.entity)
            .map_or_else(Modifiers::default, |state| state.modifiers)
    }
}

/// The pointer action a touch phase stands for, and the position it carries.
///
/// A cancelled touch has no position to report: the platform took the gesture
/// away, so a consumer must abandon the drag rather than continue it from a
/// point the device never reached.
fn pointer_of_touch(phase: TouchPhase, position: [f32; 2]) -> (PointerAction, Option<[f32; 2]>) {
    match phase {
        TouchPhase::Started => (PointerAction::Pressed, Some(position)),
        TouchPhase::Moved => (PointerAction::Moved, Some(position)),
        TouchPhase::Ended => (PointerAction::Released { cancelled: false }, Some(position)),
        TouchPhase::Cancelled => (PointerAction::Released { cancelled: true }, None),
    }
}

/// The crate's name for a winit physical key.
fn key_of(physical_key: PhysicalKey) -> Key {
    match physical_key {
        PhysicalKey::Code(code) => key_code(code),
        PhysicalKey::Unidentified(native) => Key::Other(native_code(native)),
    }
}

/// The crate's name for one of winit's key codes.
///
/// The named keys are the ones game controls address; everything else —
/// numpad keys, media keys, keys only some layouts have — keeps the platform's
/// own code. Numpad digits and Enter stay [`Key::Other`] on purpose: the
/// crate's [`Key::Num0`] and [`Key::Enter`] mean the top row and the main
/// Enter, and collapsing the two would make a keypad control impossible.
fn key_code(code: KeyCode) -> Key {
    match code {
        KeyCode::Escape => Key::Escape,
        KeyCode::Space => Key::Space,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::KeyA => Key::A,
        KeyCode::KeyB => Key::B,
        KeyCode::KeyC => Key::C,
        KeyCode::KeyD => Key::D,
        KeyCode::KeyE => Key::E,
        KeyCode::KeyF => Key::F,
        KeyCode::KeyG => Key::G,
        KeyCode::KeyH => Key::H,
        KeyCode::KeyI => Key::I,
        KeyCode::KeyJ => Key::J,
        KeyCode::KeyK => Key::K,
        KeyCode::KeyL => Key::L,
        KeyCode::KeyM => Key::M,
        KeyCode::KeyN => Key::N,
        KeyCode::KeyO => Key::O,
        KeyCode::KeyP => Key::P,
        KeyCode::KeyQ => Key::Q,
        KeyCode::KeyR => Key::R,
        KeyCode::KeyS => Key::S,
        KeyCode::KeyT => Key::T,
        KeyCode::KeyU => Key::U,
        KeyCode::KeyV => Key::V,
        KeyCode::KeyW => Key::W,
        KeyCode::KeyX => Key::X,
        KeyCode::KeyY => Key::Y,
        KeyCode::KeyZ => Key::Z,
        KeyCode::Digit0 => Key::Num0,
        KeyCode::Digit1 => Key::Num1,
        KeyCode::Digit2 => Key::Num2,
        KeyCode::Digit3 => Key::Num3,
        KeyCode::Digit4 => Key::Num4,
        KeyCode::Digit5 => Key::Num5,
        KeyCode::Digit6 => Key::Num6,
        KeyCode::Digit7 => Key::Num7,
        KeyCode::Digit8 => Key::Num8,
        KeyCode::Digit9 => Key::Num9,
        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        KeyCode::F13 => Key::F13,
        KeyCode::F14 => Key::F14,
        KeyCode::F15 => Key::F15,
        KeyCode::F16 => Key::F16,
        KeyCode::F17 => Key::F17,
        KeyCode::F18 => Key::F18,
        KeyCode::F19 => Key::F19,
        KeyCode::F20 => Key::F20,
        KeyCode::F21 => Key::F21,
        KeyCode::F22 => Key::F22,
        KeyCode::F23 => Key::F23,
        KeyCode::F24 => Key::F24,
        KeyCode::F25 => Key::F25,
        KeyCode::F26 => Key::F26,
        KeyCode::F27 => Key::F27,
        KeyCode::F28 => Key::F28,
        KeyCode::F29 => Key::F29,
        KeyCode::F30 => Key::F30,
        KeyCode::F31 => Key::F31,
        KeyCode::F32 => Key::F32,
        KeyCode::F33 => Key::F33,
        KeyCode::F34 => Key::F34,
        KeyCode::F35 => Key::F35,
        // `KeyCode` is non-exhaustive, so an unnamed code binds here.
        unnamed => Key::Other(platform_code(PhysicalKey::Code(unnamed))),
    }
}

/// The platform's own code for a key this crate does not name.
///
/// The scancode is preferred where winit offers one, because it is a property
/// of the keyboard rather than of the winit version that named the key.
fn platform_code(physical_key: PhysicalKey) -> u32 {
    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        all(
            unix,
            not(any(
                target_os = "android",
                target_os = "ios",
                target_os = "macos",
                target_os = "redox",
                target_os = "emscripten",
            ))
        ),
    ))]
    {
        use winit::platform::scancode::PhysicalKeyExtScancode;
        if let Some(scancode) = physical_key.to_scancode() {
            return scancode;
        }
    }
    match physical_key {
        PhysicalKey::Code(code) => code as u32,
        PhysicalKey::Unidentified(native) => native_code(native),
    }
}

/// The native code winit reports for a key it could not identify.
fn native_code(native: NativeKeyCode) -> u32 {
    match native {
        NativeKeyCode::Android(code) | NativeKeyCode::Xkb(code) => code,
        NativeKeyCode::MacOS(code) | NativeKeyCode::Windows(code) => u32::from(code),
        NativeKeyCode::Unidentified => UNIDENTIFIED_CODE,
    }
}

/// The crate's modifier state for winit's.
///
/// `command` is the key a platform's shortcuts are built on, so it is Control
/// off macOS and Command on it, while `mac_cmd` is Command wherever it exists.
fn to_modifiers(state: ModifiersState) -> Modifiers {
    Modifiers {
        alt: state.alt_key(),
        ctrl: state.control_key(),
        shift: state.shift_key(),
        mac_cmd: cfg!(target_os = "macos") && state.super_key(),
        command: if cfg!(target_os = "macos") {
            state.super_key()
        } else {
            state.control_key()
        },
    }
}

/// The crate's name for a winit mouse button.
fn to_button(button: MouseButton) -> CrateButton {
    match button {
        MouseButton::Left => CrateButton::Primary,
        MouseButton::Right => CrateButton::Secondary,
        MouseButton::Middle => CrateButton::Middle,
        MouseButton::Back => CrateButton::Back,
        MouseButton::Forward => CrateButton::Forward,
        MouseButton::Other(code) => CrateButton::Other(code),
    }
}

/// The crate's name for a winit touch phase.
fn to_phase(phase: WinitTouchPhase) -> TouchPhase {
    match phase {
        WinitTouchPhase::Started => TouchPhase::Started,
        WinitTouchPhase::Moved => TouchPhase::Moved,
        WinitTouchPhase::Ended => TouchPhase::Ended,
        WinitTouchPhase::Cancelled => TouchPhase::Cancelled,
    }
}

/// A wheel delta and the unit it is measured in.
///
/// A device that reports lines is a stepped wheel and one that reports pixels
/// is a trackpad; keeping the distinction lets a consumer decide how far a
/// line scrolls.
fn to_wheel(delta: &MouseScrollDelta) -> ([f32; 2], WheelUnit) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => ([*x, *y], WheelUnit::Line),
        MouseScrollDelta::PixelDelta(position) => {
            ([position.x as f32, position.y as f32], WheelUnit::Pixel)
        }
    }
}

/// The crate's name for a winit input-method event.
///
/// winit 0.30 has no variant corresponding to
/// [`ImeKind::DeleteSurrounding`]: an input method never asks this adapter to
/// delete text around the cursor, though the crate's own [`ImeEvent`] can still
/// express it for a caller that drives the state directly.
fn to_ime_kind(ime: &Ime) -> ImeKind {
    match ime {
        Ime::Enabled => ImeKind::Enabled,
        Ime::Preedit(text, cursor) => ImeKind::Preedit {
            text: text.clone(),
            active_range: cursor.map(|(start, end)| start..end),
        },
        Ime::Commit(text) => ImeKind::Commit(text.clone()),
        Ime::Disabled => ImeKind::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::event::{DeviceId, Force, Touch};

    use super::*;
    use crate::input::MouseButtons;

    /// The pointer position tests move to before producing a button or wheel
    /// event, deliberately not the fallback origin.
    const CURSOR: [f32; 2] = [12.0, 34.0];

    fn cursor_position() -> PhysicalPosition<f64> {
        PhysicalPosition::new(f64::from(CURSOR[0]), f64::from(CURSOR[1]))
    }

    fn input() -> (LocalWorld, WinitInput) {
        let mut world = LocalWorld::new();
        let input = WinitInput::new(&mut world);
        (world, input)
    }

    /// The frame's events, copied out so a test can compare them after further
    /// events have been fed in.
    fn events(world: &LocalWorld, input: &WinitInput) -> Vec<InputEvent> {
        world
            .get::<InputState>(input.state())
            .unwrap()
            .events()
            .to_vec()
    }

    /// Run `f` over the state the adapter feeds.
    fn state<R>(world: &LocalWorld, input: &WinitInput, f: impl FnOnce(&InputState) -> R) -> R {
        let state = world.get::<InputState>(input.state()).unwrap();
        f(&state)
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
        let (world, input) = input();
        let shift = ModifiersState::SHIFT.into();
        assert!(input.on_window_event(&world, &WindowEvent::ModifiersChanged(shift)));

        assert!(input.on_key(&world, PhysicalKey::Code(KeyCode::KeyW), true, false, None));
        assert!(input.on_key(&world, PhysicalKey::Code(KeyCode::KeyW), false, false, None));

        let expected = to_modifiers(ModifiersState::SHIFT);
        let events = events(&world, &input);
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
        assert!(state(&world, &input, |state| state.modifiers).shift);
    }

    #[test]
    fn a_pressed_key_also_pushes_the_text_it_produced() {
        let (world, input) = input();

        assert!(input.on_key(
            &world,
            PhysicalKey::Code(KeyCode::KeyA),
            true,
            false,
            Some("a")
        ));

        assert_eq!(
            events(&world, &input),
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
        let (world, input) = input();

        assert!(input.on_key(
            &world,
            PhysicalKey::Code(KeyCode::KeyA),
            false,
            false,
            Some("a")
        ));

        assert_eq!(events(&world, &input).len(), 1);
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
        let (world, input) = input();

        assert!(input.on_window_event(
            &world,
            &WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into())
        ));

        assert_eq!(
            events(&world, &input),
            [InputEvent::ModifiersChanged(to_modifiers(
                ModifiersState::CONTROL
            ))]
        );
        let held = state(&world, &input, |state| state.modifiers);
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
        let (world, input) = input();

        assert!(input.on_window_event(&world, &moved()));

        // One physical move, two levels: the mouse names where the cursor is,
        // the pointer names where and from which device, so a drag written
        // against either hears the same move exactly once.
        assert_eq!(
            events(&world, &input),
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
        assert_eq!(state(&world, &input, |state| state.cursor), Some(CURSOR));
        assert_eq!(state(&world, &input, |state| state.pointer), Some(CURSOR));
    }

    #[test]
    fn a_button_press_uses_the_current_cursor() {
        let (world, input) = input();
        assert!(input.on_window_event(&world, &moved()));

        assert!(input.on_window_event(
            &world,
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: winit::event::ElementState::Pressed,
                button: MouseButton::Left,
            }
        ));

        assert_eq!(
            events(&world, &input)[2],
            InputEvent::Mouse(MouseEvent::Button {
                position: CURSOR,
                button: CrateButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            })
        );
        assert_eq!(
            events(&world, &input)[3],
            InputEvent::Pointer(PointerEvent {
                kind: PointerKind::Mouse,
                id: MOUSE_POINTER,
                action: PointerAction::Pressed,
                position: Some(CURSOR),
                modifiers: Modifiers::default(),
            })
        );
        assert!(state(&world, &input, |state| state
            .buttons
            .contains(MouseButtons::PRIMARY)));
        assert!(state(&world, &input, |state| state.pointer_down));
    }

    #[test]
    fn a_button_press_without_a_cursor_falls_back_to_the_origin() {
        let (world, input) = input();

        assert!(input.on_window_event(
            &world,
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: winit::event::ElementState::Released,
                button: MouseButton::Right,
            }
        ));

        assert_eq!(
            events(&world, &input),
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
        assert!(!state(&world, &input, |state| state.pointer_down));
    }

    #[test]
    fn a_wheel_delta_keeps_its_unit() {
        let (world, input) = input();

        assert!(input.on_window_event(
            &world,
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(1.0, -2.0),
                phase: WinitTouchPhase::Moved,
            }
        ));
        assert!(input.on_window_event(
            &world,
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(3.0, 4.0)),
                phase: WinitTouchPhase::Ended,
            }
        ));

        // A wheel is a mouse's own: there is no pointer event beside it, so a
        // device-agnostic consumer is not told the page scrolled.
        assert_eq!(
            events(&world, &input),
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
        let (world, input) = input();

        assert!(input.on_window_event(
            &world,
            &WindowEvent::PinchGesture {
                device_id: DeviceId::dummy(),
                delta: 1.5,
                phase: WinitTouchPhase::Moved,
            }
        ));
        assert!(input.on_window_event(
            &world,
            &WindowEvent::RotationGesture {
                device_id: DeviceId::dummy(),
                delta: -0.25,
                phase: WinitTouchPhase::Moved,
            }
        ));

        assert_eq!(
            events(&world, &input),
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
        let (world, input) = input();
        let force = Force::Calibrated {
            force: 1.0,
            max_possible_force: 2.0,
            altitude_angle: None,
        };

        assert!(input.on_window_event(&world, &touch(WinitTouchPhase::Started, Some(force))));
        // A finger reaches a device-agnostic drag through its pointer event,
        // which is what makes the same behaviour work on a touch screen.
        assert_eq!(
            events(&world, &input),
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
            state(&world, &input, |state| state.touches.clone()),
            [(1, [5.0, 6.0])]
        );
        assert!(state(&world, &input, |state| state.pointer_down));
        assert!(
            state(&world, &input, |state| state.buttons.is_empty()),
            "a touch is not a mouse button"
        );

        assert!(input.on_window_event(&world, &touch(WinitTouchPhase::Moved, None)));
        assert!(input.on_window_event(&world, &touch(WinitTouchPhase::Ended, None)));
        assert_eq!(
            events(&world, &input)[4],
            InputEvent::Touch(TouchEvent {
                id: 1,
                phase: TouchPhase::Ended,
                position: [5.0, 6.0],
                force: None,
            })
        );
        assert!(!state(&world, &input, |state| state.pointer_down));
        assert!(state(&world, &input, |state| state.touches.is_empty()));
    }

    #[test]
    fn a_cancelled_touch_abandons_its_pointer() {
        // The platform taking a touch away ends the drag without a position,
        // so a consumer knows not to treat it as a lift where the finger was.
        let (world, input) = input();

        assert!(input.on_window_event(&world, &touch(WinitTouchPhase::Started, None)));
        assert!(input.on_window_event(&world, &touch(WinitTouchPhase::Cancelled, None)));

        assert_eq!(
            events(&world, &input)[3],
            InputEvent::Pointer(PointerEvent {
                kind: PointerKind::Touch,
                id: 1,
                action: PointerAction::Released { cancelled: true },
                position: None,
                modifiers: Modifiers::default(),
            })
        );
        assert!(!state(&world, &input, |state| state.pointer_down));
    }

    #[test]
    fn ime_preedit_and_commit_become_ime_events() {
        let (world, input) = input();

        assert!(input.on_window_event(
            &world,
            &WindowEvent::Ime(Ime::Preedit("にほん".to_owned(), Some((3, 6))))
        ));
        assert!(input.on_window_event(&world, &WindowEvent::Ime(Ime::Commit("日本".to_owned()))));
        assert!(input.on_window_event(&world, &WindowEvent::Ime(Ime::Disabled)));

        assert_eq!(
            events(&world, &input),
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
        let (world, input) = input();

        assert!(input.on_window_event(&world, &WindowEvent::Focused(false)));

        assert_eq!(events(&world, &input), [InputEvent::FocusChanged(false)]);
        assert!(!state(&world, &input, |state| state.focused));
    }

    #[test]
    fn leaving_the_window_forgets_the_cursor() {
        let (world, input) = input();
        assert!(input.on_window_event(&world, &moved()));

        assert!(input.on_window_event(
            &world,
            &WindowEvent::CursorLeft {
                device_id: DeviceId::dummy(),
            }
        ));

        assert_eq!(
            events(&world, &input)[2],
            InputEvent::Mouse(MouseEvent::Left)
        );
        assert_eq!(
            events(&world, &input)[3],
            InputEvent::Pointer(PointerEvent {
                kind: PointerKind::Mouse,
                id: MOUSE_POINTER,
                action: PointerAction::Left,
                position: None,
                modifiers: Modifiers::default(),
            })
        );
        assert_eq!(state(&world, &input, |state| state.cursor), None);
        assert_eq!(state(&world, &input, |state| state.pointer), None);
    }

    #[test]
    fn a_resize_updates_the_state_without_pushing_an_event() {
        let (world, input) = input();

        assert!(!input.on_window_event(&world, &WindowEvent::Resized(PhysicalSize::new(800, 600))));

        assert!(events(&world, &input).is_empty());
        assert_eq!(state(&world, &input, |state| state.size_px), (800, 600));
    }

    #[test]
    fn a_scale_factor_change_updates_the_state_without_pushing_an_event() {
        // `WindowEvent::ScaleFactorChanged` carries an `InnerSizeWriter` that
        // cannot be built outside winit, so the translation its match arm
        // performs is driven directly.
        let (world, input) = input();

        assert!(!input.on_scale_factor(&world, 1.5));

        assert!(events(&world, &input).is_empty());
        assert_eq!(state(&world, &input, |state| state.scale_factor), 1.5);
    }

    #[test]
    fn an_ignored_event_pushes_nothing() {
        let (world, input) = input();

        assert!(!input.on_window_event(&world, &WindowEvent::RedrawRequested));
        assert!(!input.on_window_event(&world, &WindowEvent::Occluded(true)));

        assert!(events(&world, &input).is_empty());
        assert_eq!(state(&world, &input, |state| state.scale_factor), 1.0);
    }
}
