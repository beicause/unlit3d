//! Input events and the behaviour components that react to them.
//!
//! This module is the platform-independent half of input handling: the event
//! types, the [`InputState`] resource that accumulates them, and the behaviour
//! components something drives once a frame. It depends on neither winit nor
//! egui, so game logic written here stays portable; the translation from a
//! windowing library's events lives behind the corresponding feature module
//! (for example [`crate::winit`] on the winit side).
//!
//! # The frame loop
//!
//! Input flows in three steps, and the caller owns all three:
//!
//! 1. Something feeds events into the [`InputState`] resource, one
//!    [`InputState::push`] per event.
//! 2. [`dispatch_input`] runs the behaviour components for the events that
//!    arrived. It does not apply structural changes; the caller does.
//! 3. [`InputState::clear_events`] drops the frame's events once every
//!    consumer has read them.
//!
//! ```
//! use unlit3d::prelude::*;
//!
//! // What a callback accumulates, in a sibling component: a behaviour is
//! // borrowed while it runs, so it cannot keep state in itself.
//! struct Presses(u32);
//!
//! let mut world = LocalWorld::new();
//! // The events of one frame, accumulated by whatever translates them.
//! let input = world.spawn((Resource, InputState::default()));
//! // A behaviour that reacts to the keyboard.
//! world.spawn((
//!     Presses(0),
//!     OnKey::new(|world, entity, key| {
//!         if key.pressed {
//!             let _ = world.with_mut::<Presses, _>(entity, |presses| presses.0 += 1);
//!         }
//!     }),
//! ));
//!
//! // This frame's events, as a translation layer would feed them in.
//! let _ = world.with_mut::<InputState, _>(input, |state| {
//!     state.push(InputEvent::Key(KeyEvent {
//!         key: Key::W,
//!         pressed: true,
//!         repeat: false,
//!         modifiers: Modifiers::default(),
//!     }));
//! });
//!
//! // End of the frame: deliver the events, land what the callbacks queued,
//! // then forget the events (the state they left behind stays).
//! assert!(dispatch_input(&world));
//! world.apply();
//! let _ = world.with_mut::<InputState, _>(input, InputState::clear_events);
//! assert!(world.get::<InputState>(input).unwrap().events().is_empty());
//! ```
//!
//! # Constraints on a callback
//!
//! A behaviour component is borrowed while it runs, so a callback cannot reach
//! for its own component type — that is a borrow panic, not a compile error.
//! State a callback needs across events belongs in a sibling component. Two
//! different behaviour types on the same entity are fine: they are different
//! cells, so [`OnKey`] and [`OnInput`] never conflict.

use core::ops::Deref;

use bitflags::bitflags;
use unlit_ecs::{Entity, LocalWorld};

#[cfg(feature = "winit")]
pub mod winit;

/// A key, addressed by physical position rather than by the character it
/// happens to produce.
///
/// Physical addressing is what game controls want: the `W` key stays the key
/// above `S` on every keyboard layout, while a layout-addressed key would move
/// with the user's language. A key this enum does not name is [`Key::Other`],
/// carrying the platform's own key code so no key is unreachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// The Escape key.
    Escape,
    /// The space bar.
    Space,
    /// The Enter (or Return) key.
    Enter,
    /// The Tab key.
    Tab,
    /// The Backspace key.
    Backspace,
    /// The Delete (forward delete) key.
    Delete,
    /// The up arrow key.
    ArrowUp,
    /// The down arrow key.
    ArrowDown,
    /// The left arrow key.
    ArrowLeft,
    /// The right arrow key.
    ArrowRight,
    /// The Home key.
    Home,
    /// The End key.
    End,
    /// The Page Up key.
    PageUp,
    /// The Page Down key.
    PageDown,
    /// The `A` key.
    A,
    /// The `B` key.
    B,
    /// The `C` key.
    C,
    /// The `D` key.
    D,
    /// The `E` key.
    E,
    /// The `F` key.
    F,
    /// The `G` key.
    G,
    /// The `H` key.
    H,
    /// The `I` key.
    I,
    /// The `J` key.
    J,
    /// The `K` key.
    K,
    /// The `L` key.
    L,
    /// The `M` key.
    M,
    /// The `N` key.
    N,
    /// The `O` key.
    O,
    /// The `P` key.
    P,
    /// The `Q` key.
    Q,
    /// The `R` key.
    R,
    /// The `S` key.
    S,
    /// The `T` key.
    T,
    /// The `U` key.
    U,
    /// The `V` key.
    V,
    /// The `W` key.
    W,
    /// The `X` key.
    X,
    /// The `Y` key.
    Y,
    /// The `Z` key.
    Z,
    /// The `0` key on the top row.
    Num0,
    /// The `1` key on the top row.
    Num1,
    /// The `2` key on the top row.
    Num2,
    /// The `3` key on the top row.
    Num3,
    /// The `4` key on the top row.
    Num4,
    /// The `5` key on the top row.
    Num5,
    /// The `6` key on the top row.
    Num6,
    /// The `7` key on the top row.
    Num7,
    /// The `8` key on the top row.
    Num8,
    /// The `9` key on the top row.
    Num9,
    /// The `F1` function key.
    F1,
    /// The `F2` function key.
    F2,
    /// The `F3` function key.
    F3,
    /// The `F4` function key.
    F4,
    /// The `F5` function key.
    F5,
    /// The `F6` function key.
    F6,
    /// The `F7` function key.
    F7,
    /// The `F8` function key.
    F8,
    /// The `F9` function key.
    F9,
    /// The `F10` function key.
    F10,
    /// The `F11` function key.
    F11,
    /// The `F12` function key.
    F12,
    /// The `F13` function key.
    F13,
    /// The `F14` function key.
    F14,
    /// The `F15` function key.
    F15,
    /// The `F16` function key.
    F16,
    /// The `F17` function key.
    F17,
    /// The `F18` function key.
    F18,
    /// The `F19` function key.
    F19,
    /// The `F20` function key.
    F20,
    /// The `F21` function key.
    F21,
    /// The `F22` function key.
    F22,
    /// The `F23` function key.
    F23,
    /// The `F24` function key.
    F24,
    /// The `F25` function key.
    F25,
    /// The `F26` function key.
    F26,
    /// The `F27` function key.
    F27,
    /// The `F28` function key.
    F28,
    /// The `F29` function key.
    F29,
    /// The `F30` function key.
    F30,
    /// The `F31` function key.
    F31,
    /// The `F32` function key.
    F32,
    /// The `F33` function key.
    F33,
    /// The `F34` function key.
    F34,
    /// The `F35` function key.
    F35,
    /// A key this type does not name, identified by the platform's own key
    /// code. A translation layer hands the code through unchanged, so a caller
    /// can still react to a key such as a media or numpad key.
    Other(u32),
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// The primary button: usually the left one.
    Primary,
    /// The secondary button: usually the right one.
    Secondary,
    /// The middle button, usually the wheel.
    Middle,
    /// The first side button ("back").
    Back,
    /// The second side button ("forward").
    Forward,
    /// A button this type does not name, identified by the platform's own code.
    Other(u16),
}

bitflags! {
    /// The mouse buttons currently held down.
    ///
    /// This is the set of buttons a frame was entered with, not the set of
    /// buttons a [`MouseEvent`] mentioned: it answers "is the user still
    /// dragging?" without scanning the frame's events.
    ///
    /// A touch is not a mouse button, so a finger on the screen never sets a
    /// bit here; the touches that are down live in [`InputState::touches`].
    /// The device-agnostic half of a drag — where a pointer is and whether it
    /// is down — is what [`PointerEvent`] carries.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MouseButtons: u8 {
        /// The primary button is held.
        const PRIMARY = 1 << 0;
        /// The secondary button is held.
        const SECONDARY = 1 << 1;
        /// The middle button is held.
        const MIDDLE = 1 << 2;
        /// The back side button is held.
        const BACK = 1 << 3;
        /// The forward side button is held.
        const FORWARD = 1 << 4;
    }
}

impl MouseButtons {
    /// The button closest to `button`, or `None` for [`MouseButton::Other`].
    ///
    /// A code this type does not name has no bit to occupy, so it cannot be
    /// tracked as held. A translation layer should still deliver the
    /// [`MouseEvent::Button`] itself; only the held set ignores it.
    #[must_use]
    pub const fn bit_of(button: MouseButton) -> Option<Self> {
        match button {
            MouseButton::Primary => Some(Self::PRIMARY),
            MouseButton::Secondary => Some(Self::SECONDARY),
            MouseButton::Middle => Some(Self::MIDDLE),
            MouseButton::Back => Some(Self::BACK),
            MouseButton::Forward => Some(Self::FORWARD),
            MouseButton::Other(_) => None,
        }
    }

    /// Every button this set can track, in a stable order.
    const TRACKED: [Self; 5] = [
        Self::PRIMARY,
        Self::SECONDARY,
        Self::MIDDLE,
        Self::BACK,
        Self::FORWARD,
    ];

    /// The buttons this set contains, in the tracked order.
    pub fn held(self) -> impl Iterator<Item = Self> {
        Self::TRACKED
            .into_iter()
            .filter(move |bit| self.contains(*bit))
    }
}

/// The state of the keyboard modifier keys at the moment an event was
/// produced.
///
/// The flags describe what is held, not what the event does with it: a plain
/// key press while Ctrl is down carries `ctrl: true`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// The Alt key (Option on macOS) is held.
    pub alt: bool,
    /// The Control key is held.
    pub ctrl: bool,
    /// The Shift key is held.
    pub shift: bool,
    /// The macOS Command key is held. Always false elsewhere.
    pub mac_cmd: bool,
    /// The key that acts as the platform's command key is held: Ctrl on
    /// Windows and Linux, Command on macOS.
    pub command: bool,
}

/// Where a touch or a wheel gesture is in its lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    /// The gesture started on this event.
    Started,
    /// The gesture continued.
    Moved,
    /// The gesture ended without being cancelled.
    Ended,
    /// The gesture was interrupted, so it should not count as completed.
    Cancelled,
}

/// The unit a wheel delta is measured in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelUnit {
    /// Lines of text, the usual unit of a mouse wheel.
    Line,
    /// Pixels, the usual unit of a trackpad.
    Pixel,
    /// Pages.
    Page,
}

/// Which input-method effect an [`ImeEvent`] carries.
///
/// An input method composes text a keystroke at a time — Japanese kana, for
/// example — so it reports work in progress (a pre-edit) separately from the
/// text it commits. A consumer that only wants finished text listens for
/// [`ImeKind::Commit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeKind {
    /// Work in progress. `text` is what the method currently shows and
    /// `active_range` is the byte range within it that is still undecided.
    Preedit {
        /// The text being composed so far.
        text: String,
        /// The byte range of `text` that is still undecided, if any.
        active_range: Option<core::ops::Range<usize>>,
    },
    /// Finished text, which should be inserted as it is.
    Commit(String),
    /// The method asks for up to `before` bytes before and `after` bytes after
    /// the cursor to be deleted, as part of re-composing.
    DeleteSurrounding {
        /// How many bytes before the cursor to delete.
        before: usize,
        /// How many bytes after the cursor to delete.
        after: usize,
    },
    /// The method was turned on for this input.
    Enabled,
    /// The method was turned off.
    Disabled,
}

/// A key was pressed, released, or auto-repeated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Which key.
    pub key: Key,
    /// Whether the key went down (`true`) or came up (`false`).
    pub pressed: bool,
    /// Whether this is an auto-repeat of a key already held.
    pub repeat: bool,
    /// The modifier state when the event was produced.
    pub modifiers: Modifiers,
}

/// Which kind of device a [`PointerEvent`] came from.
///
/// A pointer is the device-agnostic view of "something pointing at a spot on
/// the window": a mouse, a finger, a trackpad or a stylus. What the device *is*
/// decides how a game should read the event — a mouse has buttons and a wheel,
/// a stylus has pressure and tilt, a finger has neither — so the kind travels
/// with the event rather than being left to the consumer to guess.
///
/// The mouse and touch kinds are the device-specific halves of the same
/// hardware: a mouse also produces [`MouseEvent`]s and a touch also produces
/// [`TouchEvent`]s, so a consumer that needs buttons, wheel steps, pressure or
/// a stable touch id listens for those instead. This is the level for code
/// that only wants where the pointer is and whether it is down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerKind {
    /// A mouse.
    Mouse,
    /// A finger on a touch screen.
    Touch,
    /// A trackpad.
    ///
    /// A trackpad is not a mouse: it reports gestures a mouse has no notion of
    /// and reaches the cursor without one. It is named here because two
    /// platforms report a pinch or a rotation without saying which device
    /// produced it, and the device is a trackpad in all but the rare case of a
    /// gesture-capable mouse — the one distinction the platform does not
    /// disclose, so a consumer that must know has to ask the user or offer
    /// another control.
    Trackpad,
    /// A stylus or other pen-like pointing device.
    Pen,
    /// A pointing device this type does not name.
    ///
    /// The platforms this crate targets report a closed set of pointer kinds,
    /// so this is the last resort rather than the common case; a translation
    /// layer that meets one it cannot classify reports it here instead of
    /// calling it a mouse.
    Other,
}

/// A pointing device moved, went down or up, left the window, or gestured.
///
/// This is the device-agnostic half of pointing input: it says where the
/// pointer is and whether it is down, without naming a mouse button or a touch
/// id. A drag therefore reads the same whether it is done with a mouse or a
/// finger — which is what makes one behaviour serve both — while a consumer
/// that needs the device itself asks for its [`PointerKind`], or listens for
/// [`MouseEvent`]/[`TouchEvent`] instead.
///
/// Positions are in physical pixels with the origin at the window's top-left
/// corner, the same space windowing libraries report in. Scaling to
/// logical points, if a consumer wants them, is the consumer's call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    /// Which device produced the event.
    pub kind: PointerKind,
    /// Which contact of that kind this is.
    ///
    /// A mouse is one pointer and always reports `0`. A touch screen is not:
    /// every finger down at once is its own pointer, carrying the windowing
    /// library's touch id, which is how a consumer tells a two-finger pinch
    /// from a single finger sliding. A [`PointerKind::Trackpad`] gesture is not
    /// a contact at all and likewise reports `0`; a gesture never presses or
    /// releases, so it and a touch cannot be confused in
    /// [`InputState::pointers`].
    ///
    /// The pair of `kind` and `id` identifies a contact: an id is unique among
    /// the pointers of its kind, not across kinds.
    pub id: u64,
    /// What the device did.
    pub action: PointerAction,
    /// The pointer position in physical pixels, or `None` when the action is
    /// one that has no position — a mouse leaving the window, or a touch the
    /// platform cancelled.
    pub position: Option<[f32; 2]>,
    /// The modifier state when the event was produced.
    pub modifiers: Modifiers,
}

/// One pointer that is currently down.
///
/// `InputState::pointers` holds one of these per contact, which is what lets a
/// consumer follow several fingers at once without dropping to the
/// touch-specific [`TouchEvent`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerContact {
    /// Which device the contact is from.
    pub kind: PointerKind,
    /// Which contact of that kind it is, as in [`PointerEvent::id`].
    pub id: u64,
    /// Where the contact is, in physical pixels.
    pub position: [f32; 2],
}

/// What a pointer did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerAction {
    /// The pointer moved.
    Moved,
    /// The pointer went down: a mouse button was pressed, or a finger touched
    /// the screen.
    Pressed,
    /// The pointer came up: the button was released, or the finger lifted.
    ///
    /// `cancelled` distinguishes a lift the device reported from one the
    /// platform took away — a touch the browser turned into a scroll, say. A
    /// cancelled pointer never completed what it started, so a drag should be
    /// abandoned rather than treated as a click.
    Released {
        /// Whether the platform cancelled the gesture instead of the device
        /// finishing it.
        cancelled: bool,
    },
    /// The pointer left the window, so its position is no longer known. Only a
    /// device that hovers — a mouse — can do this.
    Left,
    /// A pinch gesture changed the scale by `delta`.
    Zoom(f32),
    /// A rotation gesture turned by `delta` radians.
    Rotate(f32),
}

/// The mouse moved, changed button, left the window, or scrolled.
///
/// Positions are in physical pixels with the origin at the window's top-left
/// corner, the same space windowing libraries report in. A mouse also produces
/// [`PointerEvent`]s, so a consumer that only wants "where and whether down"
/// does not have to know about buttons; this type is for the button and wheel
/// detail that only a mouse has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseEvent {
    /// The mouse moved to `position`.
    Moved {
        /// The new mouse position.
        position: [f32; 2],
    },
    /// A mouse button changed state.
    Button {
        /// The mouse position when the button changed.
        position: [f32; 2],
        /// Which button.
        button: MouseButton,
        /// Whether the button went down (`true`) or came up (`false`).
        pressed: bool,
        /// The modifier state when the event was produced.
        modifiers: Modifiers,
    },
    /// The mouse left the window, so there is no position for it any more.
    Left,
    /// The wheel or trackpad scrolled by `delta`, measured in `unit`.
    Wheel {
        /// The scroll amount, in `unit`s.
        delta: [f32; 2],
        /// What `delta` is measured in.
        unit: WheelUnit,
        /// Where the gesture is in its lifetime.
        phase: TouchPhase,
        /// The modifier state when the event was produced.
        modifiers: Modifiers,
    },
}

/// A touch point changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchEvent {
    /// Identifies the touch across its lifetime. A device numbers its
    /// simultaneous touches, so an id is only unique among touches that are
    /// active at the same time.
    pub id: u64,
    /// Where the touch is in its lifetime.
    pub phase: TouchPhase,
    /// The touch position, in physical pixels from the window's top-left.
    pub position: [f32; 2],
    /// The pressure the device reported, if it reports any.
    pub force: Option<f32>,
}

/// An input method changed its composition state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImeEvent {
    /// What the input method did.
    pub kind: ImeKind,
}

/// Text a key or an input method produced, ready to be inserted.
///
/// This is the same string a [`KeyEvent`] may carry alongside its key: a key
/// translates to the character the user's layout produces there, while the
/// key tells game controls which physical key moved. A consumer that inserts
/// text wants this event, not the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEvent(pub String);

impl Deref for TextEvent {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Everything a frame of input can produce.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// A key changed state.
    Key(KeyEvent),
    /// The mouse moved, changed button, left, or scrolled.
    Mouse(MouseEvent),
    /// A pointing device moved, went down or up, left, or gestured.
    Pointer(PointerEvent),
    /// A touch point changed.
    Touch(TouchEvent),
    /// Text was produced.
    Text(TextEvent),
    /// An input method changed its composition state.
    Ime(ImeEvent),
    /// The modifier state changed without a key event reporting it.
    ModifiersChanged(Modifiers),
    /// The window gained or lost focus.
    FocusChanged(bool),
}

/// The frame's input: the events that arrived since they were last cleared,
/// plus the state they left behind.
///
/// The two halves have different lifetimes. Events belong to one frame and are
/// cleared once every consumer has read them. The state fields describe how
/// the input devices are now — which modifiers are held, where the pointer is,
/// which buttons are down — and outlive the events that set them.
///
/// It lives in the world as a resource entity, so a behaviour component can
/// read the state while it handles an event:
///
/// ```
/// use unlit3d::prelude::*;
///
/// // A callback reads the resource through the handle it captured. Keeping
/// // the count in a sibling component, not in the behaviour, is what makes
/// // that borrow legal.
/// struct CtrlPresses(u32);
///
/// let mut world = LocalWorld::new();
/// let input = world.spawn((Resource, InputState::default()));
/// let observer = world.spawn((
///     CtrlPresses(0),
///     OnInput::new(move |world, entity, _event| {
///         let ctrl = world.get::<InputState>(input).unwrap().modifiers.ctrl;
///         if ctrl {
///             let _ = world.with_mut::<CtrlPresses, _>(entity, |presses| presses.0 += 1);
///         }
///     }),
/// ));
///
/// let _ = world.with_mut::<InputState, _>(input, |state| {
///     state.push(InputEvent::ModifiersChanged(Modifiers { ctrl: true, ..Modifiers::default() }));
/// });
/// dispatch_input(&world);
///
/// assert_eq!(world.get::<CtrlPresses>(observer).unwrap().0, 1);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct InputState {
    events: Vec<InputEvent>,
    /// The modifier keys currently held.
    pub modifiers: Modifiers,
    /// Every pointer currently down, in the order they went down.
    ///
    /// A mouse contributes at most one entry and a touch screen one per finger,
    /// so this is the state that tells a one-finger drag from a two-finger
    /// gesture. [`Self::pointer`] is the position of the most recent contact
    /// and is what a single-pointer drag reads.
    pub pointers: Vec<PointerContact>,
    /// Where the pointer is in physical pixels, or `None` while it is outside
    /// the window.
    ///
    /// A pointer is a mouse or a finger, so this is set by either and is what
    /// device-agnostic code reads. With several fingers down it follows the
    /// most recent contact, which is the one a drag is about.
    pub pointer: Option<[f32; 2]>,
    /// Whether any pointer is down: a mouse button is held, a finger is on the
    /// screen, or several fingers are.
    ///
    /// This is [`Self::pointers`] being non-empty, kept as a field because a
    /// drag asks the question on every move. Read [`Self::pointers`] instead to
    /// tell how many pointers there are.
    pub pointer_down: bool,
    /// The mouse position in physical pixels, or `None` while the mouse is
    /// outside the window. A finger does not move the mouse.
    pub cursor: Option<[f32; 2]>,
    /// The mouse buttons currently held.
    pub buttons: MouseButtons,
    /// The touches currently active, as `(touch id, position)` pairs.
    pub touches: Vec<(u64, [f32; 2])>,
    /// Whether the window has focus. Input without focus is usually stale.
    pub focused: bool,
    /// The window's size in physical pixels.
    pub size_px: (u32, u32),
    /// The window's physical pixels per logical point.
    pub scale_factor: f32,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            modifiers: Modifiers::default(),
            pointers: Vec::new(),
            pointer: None,
            pointer_down: false,
            cursor: None,
            buttons: MouseButtons::empty(),
            touches: Vec::new(),
            // A window starts focused, and a caller that knows better seeds it
            // through `set_focused` or a `FocusChanged` event.
            focused: true,
            size_px: (0, 0),
            // One physical pixel per logical point until a window says
            // otherwise.
            scale_factor: 1.0,
        }
    }
}

impl InputState {
    /// The events that have arrived since they were last cleared.
    #[must_use]
    pub fn events(&self) -> &[InputEvent] {
        &self.events
    }

    /// Record `event` and update the state it leaves behind.
    ///
    /// This is the entry point a translation layer feeds; it is also the whole
    /// state machine, because every state change a frame makes is carried by
    /// the event that caused it.
    pub fn push(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key(event) => self.modifiers = event.modifiers,
            InputEvent::Mouse(event) => self.apply_mouse(event),
            InputEvent::Pointer(event) => self.apply_pointer(event),
            InputEvent::Touch(event) => self.apply_touch(&event),
            InputEvent::Ime(_) | InputEvent::Text(_) => {}
            InputEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            InputEvent::FocusChanged(focused) => self.focused = focused,
        }
        self.events.push(event);
    }

    /// Drop the frame's events, keeping the state they left behind.
    ///
    /// Call this once every consumer of the frame has read the events: the
    /// dispatcher and any UI source share the one list.
    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    /// Seed the window size, for a caller that learns it outside an event.
    pub fn set_size_px(&mut self, width: u32, height: u32) {
        self.size_px = (width, height);
    }

    /// Seed the pixel density, for a caller that learns it outside an event.
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        self.scale_factor = scale_factor;
    }

    /// Move the pointer without an event, for a caller that tracks it itself.
    ///
    /// Only the hover position is set: which pointers are down is a fact only
    /// the events carry, so a caller that synthesises a move cannot invent a
    /// press through this.
    pub fn set_pointer(&mut self, position: Option<[f32; 2]>) {
        self.pointer = position;
    }

    /// Update the state a mouse event leaves behind.
    fn apply_mouse(&mut self, event: MouseEvent) {
        match event {
            MouseEvent::Moved { position } => self.cursor = Some(position),
            MouseEvent::Button {
                position,
                button,
                pressed,
                ..
            } => {
                self.cursor = Some(position);
                if let Some(bit) = MouseButtons::bit_of(button) {
                    self.buttons.set(bit, pressed);
                }
            }
            MouseEvent::Left => self.cursor = None,
            MouseEvent::Wheel { .. } => {}
        }
    }

    /// Update the state a pointer event leaves behind.
    ///
    /// A pointer's down state is device-agnostic, so it is tracked here for a
    /// mouse and a finger alike; which *button* is held is mouse-only state and
    /// is left to [`Self::apply_mouse`].
    fn apply_pointer(&mut self, event: PointerEvent) {
        if let Some(position) = event.position {
            self.pointer = Some(position);
        }
        // A press or a move may in principle arrive without a position, and a
        // contact has to be somewhere: the last known pointer position is then
        // the best answer. A translation layer sends a move before a press, so
        // a device's first contact has one even then.
        let position = event.position.or(self.pointer);
        match event.action {
            PointerAction::Pressed => {
                // A second finger presses its own contact without disturbing
                // the first: this is what makes the two distinguishable, and
                // why a single down flag would be wrong.
                match self.contact_mut(event.kind, event.id) {
                    Some(contact) => contact.position = position.unwrap_or(contact.position),
                    None => {
                        if let Some(position) = position {
                            self.pointers.push(PointerContact {
                                kind: event.kind,
                                id: event.id,
                                position,
                            });
                        }
                    }
                }
            }
            PointerAction::Released { .. } => {
                self.pointers
                    .retain(|contact| (contact.kind, contact.id) != (event.kind, event.id));
            }
            PointerAction::Left => self.pointer = None,
            PointerAction::Moved => {
                // A move only updates a contact that is down; a hover is the
                // pointer position above and nothing more.
                if let (Some(contact), Some(position)) =
                    (self.contact_mut(event.kind, event.id), position)
                {
                    contact.position = position;
                }
            }
            PointerAction::Zoom(_) | PointerAction::Rotate(_) => {}
        }
        self.pointer_down = !self.pointers.is_empty();
    }

    /// The down contact `kind` and `id` name, if it is one.
    fn contact_mut(&mut self, kind: PointerKind, id: u64) -> Option<&mut PointerContact> {
        self.pointers
            .iter_mut()
            .find(|contact| (contact.kind, contact.id) == (kind, id))
    }

    /// Update the state a touch event leaves behind.
    ///
    /// The active touches are the state a consumer needs to tell one finger
    /// from another; the device-agnostic down flag is set by the
    /// [`PointerEvent`] the same touch produces.
    fn apply_touch(&mut self, event: &TouchEvent) {
        match event.phase {
            TouchPhase::Started | TouchPhase::Moved => {
                match self.touches.iter_mut().find(|(id, _)| *id == event.id) {
                    Some((_, position)) => *position = event.position,
                    None => self.touches.push((event.id, event.position)),
                }
            }
            // A finished touch is no longer active, so it stops being state.
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.touches.retain(|(id, _)| *id != event.id);
            }
        }
    }
}

/// A callback that may read and write the world, and receives the entity it
/// runs for together with one event.
type EventCallback<E> = Box<dyn FnMut(&LocalWorld, Entity, &E)>;

/// Declares one behaviour component over an event type.
///
/// Every behaviour is the same shape [`unlit_ecs`]' own behaviour components
/// use: a public boxed closure, a `new` that boxes a caller's closure, and a
/// `run` that calls it. Events are passed by reference, so the world can hold
/// one copy of an event that several behaviours read.
///
/// A `run` takes the world and the entity the behaviour sits on, so the
/// callback can tell which entity it is acting for — one closure may be
/// mounted on many.
macro_rules! behaviour {
    ($name:ident, $event:ty, $doc:expr) => {
        #[doc = $doc]
        pub struct $name(pub EventCallback<$event>);

        impl $name {
            #[doc = concat!("Wrap `f` as an [`", stringify!($name), "`].")]
            pub fn new(f: impl FnMut(&LocalWorld, Entity, &$event) + 'static) -> Self {
                Self(Box::new(f))
            }

            #[doc = concat!("Run this behaviour for `entity` with `event`.")]
            pub fn run(&mut self, world: &LocalWorld, entity: Entity, event: &$event) {
                (self.0)(world, entity, event);
            }
        }
    };
}

behaviour!(
    OnInput,
    InputEvent,
    "Runs for every input event, whatever its category.\n\n\
     This is the catch-all: a behaviour that wants the whole stream — an input\n\
     log, a replay recorder, a handler that cares about ordering across\n\
     categories — mounts this one instead of the per-category components."
);
behaviour!(OnKey, KeyEvent, "Runs for every [`KeyEvent`].");
behaviour!(OnMouse, MouseEvent, "Runs for every [`MouseEvent`].");
behaviour!(
    OnPointer,
    PointerEvent,
    "Runs for every [`PointerEvent`], whatever device produced it.\n\n\
     This is the device-agnostic pointing behaviour: a drag written against it\n\
     works with a mouse, a finger and a stylus alike. A consumer that needs the\n\
     device's own detail — mouse buttons and wheel steps, or a touch's id and\n\
     pressure — mounts [`OnMouse`] or [`OnTouch`] beside it."
);
behaviour!(OnTouch, TouchEvent, "Runs for every [`TouchEvent`].");
behaviour!(OnText, TextEvent, "Runs for every [`TextEvent`].");
behaviour!(OnIme, ImeEvent, "Runs for every [`ImeEvent`].");

/// Run the world's behaviour components once for each event of the frame.
///
/// Returns whether any event was delivered, so a caller can skip the
/// [`LocalWorld::apply`] that would otherwise follow an empty dispatch. With
/// no [`InputState`] resource in the world this does nothing and returns
/// `false`: a world that never asked for input has nothing to deliver, and
/// that is not an error.
///
/// Each event goes to its category's behaviour component and, in addition, to
/// every [`OnInput`]:
///
/// | event | components that receive it |
/// |---|---|
/// | [`InputEvent::Key`] | [`OnKey`], [`OnInput`] |
/// | [`InputEvent::Mouse`] | [`OnMouse`], [`OnInput`] |
/// | [`InputEvent::Pointer`] | [`OnPointer`], [`OnInput`] |
/// | [`InputEvent::Touch`] | [`OnTouch`], [`OnInput`] |
/// | [`InputEvent::Text`] | [`OnText`], [`OnInput`] |
/// | [`InputEvent::Ime`] | [`OnIme`], [`OnInput`] |
/// | [`InputEvent::ModifiersChanged`] / [`InputEvent::FocusChanged`] | [`OnInput`] |
///
/// # What a callback may not do
///
/// The events are copied out of the [`InputState`] before any callback runs, so
/// a callback may freely read and write that resource — a common need, since
/// the resource is where "is Ctrl still held?" lives.
///
/// Two things stay off limits, both consequences of `unlit_ecs` borrowing a
/// component to call it:
///
/// - A callback must not reach for its *own* component type, on any entity.
///   That is a borrow panic; keep per-callback state in a sibling component.
///   [`OnInput`] and [`OnKey`] on one entity do not conflict, because they are
///   different cells.
/// - A callback must not re-enter this function for the same component type.
///
/// Structural changes queue through [`LocalWorld::queue`] and land when the
/// caller applies them. The caller applies **once**, after the whole dispatch,
/// not between callbacks: the behaviour components are iterated directly, so
/// the world is borrowed for the duration and cannot be applied to. Typically
/// that looks like:
///
/// ```
/// # use unlit3d::prelude::*;
/// # let mut world = LocalWorld::new();
/// if dispatch_input(&world) {
///     world.apply();
/// }
/// ```
pub fn dispatch_input(world: &LocalWorld) -> bool {
    let Some(events) = world
        .query::<&InputState>()
        .next()
        .map(|(_, state)| state.events.to_vec())
    else {
        return false;
    };

    let delivered = !events.is_empty();

    for event in &events {
        match event {
            InputEvent::Key(event) => {
                for (entity, mut behaviour) in world.query::<&mut OnKey>() {
                    behaviour.run(world, entity, event);
                }
            }
            InputEvent::Mouse(event) => {
                for (entity, mut behaviour) in world.query::<&mut OnMouse>() {
                    behaviour.run(world, entity, event);
                }
            }
            InputEvent::Pointer(event) => {
                for (entity, mut behaviour) in world.query::<&mut OnPointer>() {
                    behaviour.run(world, entity, event);
                }
            }
            InputEvent::Touch(event) => {
                for (entity, mut behaviour) in world.query::<&mut OnTouch>() {
                    behaviour.run(world, entity, event);
                }
            }
            InputEvent::Text(event) => {
                for (entity, mut behaviour) in world.query::<&mut OnText>() {
                    behaviour.run(world, entity, event);
                }
            }
            InputEvent::Ime(event) => {
                for (entity, mut behaviour) in world.query::<&mut OnIme>() {
                    behaviour.run(world, entity, event);
                }
            }
            // These two carry no per-category payload to route, so only the
            // catch-all hears them.
            InputEvent::ModifiersChanged(_) | InputEvent::FocusChanged(_) => {}
        }

        for (entity, mut behaviour) in world.query::<&mut OnInput>() {
            behaviour.run(world, entity, event);
        }
    }

    delivered
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::rc::Rc;

    use unlit_ecs::{Resource, With};

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
        let mut world = LocalWorld::new();
        let (seen_input, input) = counter();
        let (seen_key, key) = counter();
        let (seen_mouse, mouse) = counter();
        let (seen_pointer, pointer) = counter();
        let (seen_touch, touch) = counter();
        let (seen_text, text) = counter();
        let (seen_ime, ime) = counter();

        world.spawn((OnInput::new(move |_, _, _| {
            seen_input.set(seen_input.get() + 1);
        }),));
        world.spawn((OnKey::new(move |_, _, _| {
            seen_key.set(seen_key.get() + 1);
        }),));
        world.spawn((OnMouse::new(move |_, _, _| {
            seen_mouse.set(seen_mouse.get() + 1);
        }),));
        world.spawn((OnPointer::new(move |_, _, _| {
            seen_pointer.set(seen_pointer.get() + 1);
        }),));
        world.spawn((OnTouch::new(move |_, _, _| {
            seen_touch.set(seen_touch.get() + 1);
        }),));
        world.spawn((OnText::new(move |_, _, _| {
            seen_text.set(seen_text.get() + 1);
        }),));
        world.spawn((OnIme::new(move |_, _, _| {
            seen_ime.set(seen_ime.get() + 1);
        }),));

        let input_entity = world.spawn((Resource, InputState::default()));
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
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            for event in events {
                state.push(event);
            }
        });

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
        let mut world = LocalWorld::new();
        let (keys, key_count) = counter();
        let (all, all_count) = counter();

        world.spawn((
            OnKey::new(move |_, _, _| keys.set(keys.get() + 1)),
            OnInput::new(move |_, _, _| all.set(all.get() + 1)),
        ));
        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
            state.push(mouse_event());
        });

        dispatch_input(&world);

        assert_eq!(key_count.get(), 1);
        assert_eq!(all_count.get(), 2);
    }

    #[test]
    fn one_behaviour_serves_every_entity_carrying_it() {
        let mut world = LocalWorld::new();
        let (count, seen) = counter();
        for _ in 0..3 {
            let count = count.clone();
            world.spawn((OnKey::new(move |_, _, _| count.set(count.get() + 1)),));
        }

        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
        });

        dispatch_input(&world);

        assert_eq!(seen.get(), 3, "one key event per behaviour");
    }

    #[test]
    fn a_callback_sees_the_entity_it_sits_on() {
        // The entity a behaviour is passed is the one the component sits on, so
        // a callback can reach its own sibling components.
        let mut world = LocalWorld::new();
        let (matches, seen) = counter();
        world.spawn((
            7u32,
            OnKey::new(move |world, entity, _| {
                let marker = *world.get::<u32>(entity).unwrap();
                matches.set(marker as usize);
            }),
        ));
        // Another entity of the same kind must not be substituted.
        world.spawn((9u32, OnKey::new(|_, _, _| {})));

        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
        });

        dispatch_input(&world);

        assert_eq!(seen.get(), 7);
    }

    #[test]
    fn a_callback_may_read_and_write_the_input_state() {
        // The events are copied out before the callbacks run, so the resource
        // is not borrowed while they execute.
        let mut world = LocalWorld::new();
        let input_entity = world.spawn((Resource, InputState::default()));
        world.spawn((OnKey::new(move |world, _, _| {
            let held = world
                .get::<InputState>(input_entity)
                .unwrap()
                .modifiers
                .ctrl;
            let _ = world.with_mut::<InputState, _>(input_entity, |state| state.focused = held);
        }),));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(InputEvent::Key(KeyEvent {
                key: Key::C,
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
            }));
        });

        dispatch_input(&world);

        assert!(world.get::<InputState>(input_entity).unwrap().focused);
    }

    #[test]
    fn a_callback_may_filter_its_siblings_without_disturbing_the_dispatch() {
        // `With` borrows no cell, so a callback may enumerate the entities of
        // its own component type even though one of them is borrowed.
        let mut world = LocalWorld::new();
        let (count, seen) = counter();
        world.spawn((OnKey::new(move |world, _, _| {
            let siblings = world.query_filtered::<Entity, With<OnKey>>().count();
            count.set(siblings);
        }),));
        world.spawn((OnKey::new(|_, _, _| {}),));

        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
        });

        dispatch_input(&world);

        assert_eq!(seen.get(), 2);
    }

    #[test]
    fn a_queued_spawn_lands_after_apply() {
        let mut world = LocalWorld::new();
        world.spawn((OnKey::new(|world, _, _| {
            world.queue().spawn((42u32,));
        }),));
        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
        });

        assert!(dispatch_input(&world));
        assert_eq!(world.len(), 2, "queued, not applied");

        world.apply();
        assert_eq!(world.len(), 3);
    }

    #[test]
    fn a_callback_may_queue_its_own_despawn() {
        let mut world = LocalWorld::new();
        let doomed = world.spawn((OnKey::new(|world, entity, _| {
            world.queue().despawn(entity);
        }),));
        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
            state.push(key_event(Key::B));
        });

        dispatch_input(&world);
        world.apply();

        assert!(!world.has::<OnKey>(doomed));
    }

    #[test]
    fn an_empty_event_list_delivers_nothing() {
        let mut world = LocalWorld::new();
        let (count, seen) = counter();
        world.spawn((OnInput::new(move |_, _, _| count.set(count.get() + 1)),));
        world.spawn((Resource, InputState::default()));

        assert!(!dispatch_input(&world));
        assert_eq!(seen.get(), 0);
    }

    #[test]
    fn no_input_state_resource_is_not_an_error() {
        let mut world = LocalWorld::new();
        let (count, seen) = counter();
        world.spawn((OnInput::new(move |_, _, _| count.set(count.get() + 1)),));

        assert!(!dispatch_input(&world));
        assert_eq!(seen.get(), 0);
    }

    #[test]
    fn events_without_behaviours_are_delivered_to_nobody() {
        let mut world = LocalWorld::new();
        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
        });

        // Events did arrive, so the caller still owes an `apply`.
        assert!(dispatch_input(&world));
    }

    #[test]
    fn push_updates_the_state_and_keeps_the_event() {
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

        assert_eq!(state.events().len(), 6);
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
    fn clear_events_keeps_the_state() {
        let mut state = InputState::default();
        state.set_size_px(1280, 720);
        state.set_scale_factor(2.0);
        state.push(InputEvent::ModifiersChanged(Modifiers {
            alt: true,
            ..Modifiers::default()
        }));
        state.push(InputEvent::FocusChanged(false));
        state.push(InputEvent::Mouse(MouseEvent::Moved { position: CURSOR }));
        state.push(InputEvent::Mouse(MouseEvent::Button {
            position: CURSOR,
            button: MouseButton::Secondary,
            pressed: true,
            modifiers: Modifiers::default(),
        }));

        state.clear_events();

        assert!(state.events().is_empty());
        assert!(state.modifiers.alt);
        assert_eq!(state.cursor, Some(CURSOR));
        assert_eq!(state.buttons, MouseButtons::SECONDARY);
        assert!(!state.focused);
        assert_eq!(state.size_px, (1280, 720));
        assert_eq!(state.scale_factor, 2.0);
    }

    #[test]
    fn a_seeded_state_survives_a_dispatch_and_a_clear() {
        // The documented frame loop, end to end: deliver, apply, clear, and
        // the state a later frame reads is still there.
        let mut world = LocalWorld::new();
        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.set_size_px(800, 600);
            state.set_scale_factor(1.5);
            state.push(InputEvent::FocusChanged(false));
            state.push(key_event(Key::S));
        });

        assert!(dispatch_input(&world));
        world.apply();
        let _ = world.with_mut::<InputState, _>(input_entity, InputState::clear_events);

        let state = world.get::<InputState>(input_entity).unwrap();
        assert!(state.events().is_empty());
        assert_eq!(state.size_px, (800, 600));
        assert_eq!(state.scale_factor, 1.5);
        assert!(!state.focused);
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn a_behaviour_cannot_reborrow_its_own_component() {
        // The boundary a callback has to stay inside, pinned here so it cannot
        // regress into a silently different failure.
        let mut world = LocalWorld::new();
        world.spawn((OnKey::new(|world, entity, _| {
            let _ = world.get::<OnKey>(entity);
        }),));
        let input_entity = world.spawn((Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input_entity, |state| {
            state.push(key_event(Key::A));
        });

        dispatch_input(&world);
    }
}
