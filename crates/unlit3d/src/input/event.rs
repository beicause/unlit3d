//! The input event vocabulary: what a translation layer reports, and what a
//! behaviour component receives.
//!
//! Every event is one frame of one device. The state those events add up to is
//! [`InputState`](super::InputState); the buffer they travel through is
//! [`InputEvents`](super::InputEvents).

use core::ops::Deref;

use bitflags::bitflags;

/// A key, addressed by physical position rather than by the character it
/// happens to produce.
///
/// Physical addressing is what game controls want: the `W` key stays the key
/// above `S` on every keyboard layout, while a layout-addressed key would move
/// with the user's language. A key this enum does not name is [`Key::Other`],
/// carrying the platform's own key code so no key is unreachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
    /// bit here; the touches that are down live in
    /// [`InputState::touches`](super::InputState::touches).
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
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct KeyEvent {
    /// Which key.
    pub key: Key,
    /// Whether the key went down (`true`) or came up (`false`).
    #[cfg_attr(feature = "reflect", facet(default = true))]
    pub pressed: bool,
    /// Whether this is an auto-repeat of a key already held.
    #[cfg_attr(feature = "reflect", facet(default))]
    pub repeat: bool,
    /// The modifier state when the event was produced.
    #[cfg_attr(feature = "reflect", facet(default))]
    pub modifiers: Modifiers,
}

/// Which kind of device a [`PointerEvent`] came from.
///
/// A pointer is the device-agnostic reading of "something pointing at a spot on
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
    /// [`InputState::pointers`](super::InputState::pointers).
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
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
        #[cfg_attr(feature = "reflect", facet(default = true))]
        pressed: bool,
        /// The modifier state when the event was produced.
        #[cfg_attr(feature = "reflect", facet(default))]
        modifiers: Modifiers,
    },
    /// The mouse left the window, so there is no position for it any more.
    Left,
    /// The wheel or trackpad scrolled by `delta`, measured in `unit`.
    Wheel {
        /// The scroll amount, in `unit`s.
        delta: [f32; 2],
        /// What `delta` is measured in.
        #[cfg_attr(feature = "reflect", facet(default = WheelUnit::Pixel))]
        unit: WheelUnit,
        /// Where the gesture is in its lifetime.
        #[cfg_attr(feature = "reflect", facet(default = TouchPhase::Moved))]
        phase: TouchPhase,
        /// The modifier state when the event was produced.
        #[cfg_attr(feature = "reflect", facet(default))]
        modifiers: Modifiers,
    },
}

/// A touch point changed.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
#[cfg_attr(feature = "reflect", facet(transparent))]
pub struct TextEvent(pub String);

impl Deref for TextEvent {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Everything a frame of input can produce.
#[derive(Clone, Debug, PartialEq)]
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
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
    /// The window's drawable size changed, in physical pixels.
    Resized {
        /// The new size.
        size_px: (u32, u32),
    },
    /// The window's pixel density changed.
    ScaleFactorChanged {
        /// The new physical pixels per logical point.
        scale_factor: f32,
    },
}
