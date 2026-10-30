//! Input events and the behaviour components that react to them.
//!
//! This module is the platform-independent half of input handling: the event
//! types, the [`InputState`] each world keeps, the [`InputWriter`] that feeds
//! them, and the behaviour components something drives once a frame. It
//! depends on neither winit nor egui, so game logic written here stays
//! portable; the translation from a windowing library's events lives behind the
//! corresponding feature module (for example [`crate::winit`] on the winit
//! side).
//!
//! # The frame loop
//!
//! Input is level state plus a per-world buffer, and the caller owns both steps:
//!
//! 1. Something feeds events through the world's [`InputWriter`], one
//!    [`InputWriter::push`] per event. Each one lands in the buffer of every
//!    world that has one.
//! 2. [`dispatch_input`] starts this world's input tick: everything buffered
//!    since the last tick becomes the tick's batch, the batch is folded into
//!    this world's [`InputState`] — modifiers, pointers, buttons, focus, size
//!    — and the behaviour components run for it. It does not apply structural
//!    changes; the caller does.
//!
//! Nothing clears the events afterwards. A world's [`InputEvents`] is the one
//! buffer everything in that world reads — the events of this tick and the
//! state they built — replaced on the next tick, so a slower world still sees
//! every event in order and no two readers of one world can disagree.
//!
//! ```rust
//! use unlit3d::prelude::*;
//!
//! // What a callback accumulates, in a sibling component: a behaviour is
//! // borrowed while it runs, so it cannot keep state in itself.
//! struct Presses(u32);
//!
//! let mut world = World::new();
//! // The handle feeds the events in; the buffer is the world's own, holding
//! // what arrived since its last tick.
//! let input = InputWriter::new();
//! world.root_scope_mut().spawn(InputFeed(input.clone()));
//! // A behaviour that reacts to the keyboard.
//! world.root_scope_mut().spawn(bundle![
//!     Presses(0),
//!     OnKey::new(|world, entity, key| {
//!         if key.pressed {
//!             let _ = world.root_scope().query_one::<&mut Presses>(entity).map(|mut presses| presses.0 += 1);
//!         }
//!     }),
//! ]);
//!
//! // This frame's events, as a translation layer would feed them in.
//! input.push(InputEvent::Key(KeyEvent {
//!     key: Key::W,
//!     pressed: true,
//!     repeat: false,
//!     modifiers: Modifiers::default(),
//! }));
//!
//! // End of the tick: take what arrived as this tick's batch, deliver it,
//! // and land what the callbacks queued. The state the events left stays.
//! assert!(dispatch_input(&world));
//! world.apply().unwrap();
//! // A second dispatch has nothing left to deliver.
//! assert!(!dispatch_input(&world));
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
use std::sync::{Arc, PoisonError, RwLock};

use crate::relay::{Stream, StreamReader};
use bitflags::bitflags;
use unlit_ecs::{Bundle, DynamicBundle, Entity, World, bundle};

use crate::behaviour::behaviour;

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

/// How the input devices are now: the state the events left behind.
///
/// Which modifiers are held, where the pointer is, which buttons are down —
/// things that outlive the event that set them. The events themselves are not
/// here: they belong to the tick of the world that received them, in its
/// [`InputEvents`].
///
/// It lives inside a world's [`InputEvents`], built by that world's own tick,
/// so a behaviour component reads it while it handles an event:
///
/// ```rust
/// use unlit3d::prelude::*;
///
/// // A callback reads the state from the world it was handed, through the
/// // buffer that holds it. Keeping the count in a sibling component, not in
/// // the behaviour, is what makes that borrow legal.
/// struct CtrlPresses(u32);
///
/// let mut world = World::new();
/// let input = InputWriter::new();
/// world.root_scope_mut().spawn(InputFeed(input.clone()));
/// let observer = world.root_scope_mut().spawn(bundle![
///     CtrlPresses(0),
///     OnInput::new(|world, entity, _event| {
///         let ctrl = world
///             .root_scope()
///             .query::<&InputEvents>()
///             .iter()
///             .next()
///             .is_some_and(|input_events| {
///                 input_events
///                     .with_state(|state| state.modifiers.ctrl)
///             });
///         if ctrl {
///             let _ = world.root_scope().query_one::<&mut CtrlPresses>(entity).map(|mut presses| presses.0 += 1);
///         }
///     }),
/// ]);
///
/// input.push(InputEvent::ModifiersChanged(Modifiers { ctrl: true, ..Modifiers::default() }));
/// dispatch_input(&world);
///
/// assert_eq!(world.root_scope().query_one::<&CtrlPresses>(observer).unwrap().0, 1);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct InputState {
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
            modifiers: Modifiers::default(),
            pointers: Vec::new(),
            pointer: None,
            pointer_down: false,
            cursor: None,
            buttons: MouseButtons::empty(),
            touches: Vec::new(),
            // A window starts focused; only a `FocusChanged` event says
            // otherwise.
            focused: true,
            size_px: (0, 0),
            // One physical pixel per logical point until a window says
            // otherwise.
            scale_factor: 1.0,
        }
    }
}

#[cfg(test)]
impl InputState {
    /// Apply `event` without a world to hand it to.
    ///
    /// A test about the state machine alone does not need a buffer; feeding
    /// input for real goes through [`InputWriter::push`], which reaches a
    /// world's own [`InputEvents`] instead.
    fn push(&mut self, event: InputEvent) {
        self.record(&event);
    }
}

impl InputState {
    /// Record `event` and update the state it leaves behind.
    ///
    /// This is the whole state machine, because every state change a frame
    /// makes is carried by the event that caused it. It is private: a world's
    /// state is only ever built by its own [`InputEvents::refresh`], from the
    /// events fed to it.
    fn record(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key(event) => self.modifiers = event.modifiers,
            InputEvent::Mouse(event) => self.apply_mouse(*event),
            InputEvent::Pointer(event) => self.apply_pointer(*event),
            InputEvent::Touch(event) => self.apply_touch(event),
            InputEvent::Ime(_) | InputEvent::Text(_) => {}
            InputEvent::ModifiersChanged(modifiers) => self.modifiers = *modifiers,
            InputEvent::FocusChanged(focused) => self.focused = *focused,
            InputEvent::Resized { size_px } => self.size_px = *size_px,
            InputEvent::ScaleFactorChanged { scale_factor } => self.scale_factor = *scale_factor,
        }
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

/// A world's input: the state the events left behind, and this tick's events.
///
/// This is the one input buffer a world has. [`InputWriter`] drops every event
/// into the queue here, and the world's tick — [`dispatch_input`] — moves
/// whatever queued into the batch and folds it into the state. Everything in
/// the world then reads that same batch and that same state, so a world that
/// ticks slower than input arrives still sees every event, in order, and no two
/// readers of one world can disagree or steal from each other.
///
/// The batch is replaced by the next tick, which is what keeps an event from
/// arriving twice. The state outlives it: which modifiers are held, where the
/// pointer is and which buttons are down describe the devices now rather than
/// one tick, and are what a drag asks about on every move.
#[derive(Clone, Debug)]
#[cfg_attr(
    feature = "reflect",
    derive(facet::Facet),
    facet(opaque, proxy = crate::reflect::InputEventsProxy)
)]
pub struct InputEvents {
    /// The events fed since the last refresh, queued for this world alone.
    events: StreamReader<InputEvent>,
    /// The state the events so far left behind.
    state: Arc<RwLock<InputState>>,
}

impl InputEvents {
    /// Run `f` over the state the events so far left behind.
    ///
    /// Reading takes the lock shared, so every reader in the world reads the
    /// same batch at once; only a tick takes it exclusively. A panicking
    /// writer leaves the lock poisoned, and the value is still readable, so
    /// the poison is ignored rather than propagated.
    pub fn with_state<R>(&self, f: impl FnOnce(&InputState) -> R) -> R {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        f(&state)
    }

    /// Run `f` over this tick's events, oldest first.
    ///
    /// Reading does not consume: the whole world reads the same batch, so no
    /// reader can take an event away from the next one. A caller that only
    /// transforms the batch does it here, rather than copying it out first.
    pub fn with_batch<R>(&self, f: impl FnOnce(&[InputEvent]) -> R) -> R {
        self.events.with_batch(f)
    }

    /// Append this tick's events, oldest first, to `out`.
    pub fn copy_events_into(&self, out: &mut Vec<InputEvent>) {
        self.events.copy_into(out);
    }

    /// Start this world's input tick: take everything queued as the batch, and
    /// fold it into the state.
    ///
    /// Whatever the previous tick left is dropped, so an event is delivered on
    /// exactly one tick. [`dispatch_input`] calls this; a world that reads
    /// input without dispatching anything calls it itself, once per tick,
    /// before anything reads the batch.
    pub fn refresh(&self) {
        self.events.pump();
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        self.events.with_batch(|batch| {
            for event in batch {
                state.record(event);
            }
        });
    }
}

/// Feeds a frame's input to every world that reads it.
///
/// One writer is fed by whoever translates the platform's events, and every
/// world that reads input holds a clone: it is `Send + Sync`, so it can also be
/// carried across a thread boundary. Feeding an event drops it into the buffer
/// of every world subscribed through [`subscribe`](Self::subscribe).
///
/// No state is kept here on purpose. Each world derives its own state from the
/// same stream during its own tick, so a world that ticks at a different rate
/// from the one feeding still ends up with the state its own events describe,
/// and there is no shared lock for a reader to wait on.
#[derive(Clone, Debug, Default)]
pub struct InputWriter(Stream<InputEvent>);

impl InputWriter {
    /// A writer nothing is subscribed to yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event.
    pub fn push(&self, event: InputEvent) {
        self.push_all([event]);
    }

    /// Feed several events, in the order given.
    ///
    /// One lock is held for the batch, so a device event and the
    /// device-agnostic one describing the same action reach every world
    /// together.
    pub fn push_all(&self, events: impl IntoIterator<Item = InputEvent>) {
        // A world that dropped its buffer is pruned the next time input
        // arrives, rather than by whoever happens to notice.
        self.0.publish_all(events);
    }

    /// A buffer of this writer's future events, for one world.
    ///
    /// The buffer lives as long as the component holding it: a world that drops
    /// it stops being fed on the next push.
    pub fn subscribe(&self) -> InputEvents {
        InputEvents {
            events: self.0.subscribe(),
            state: Arc::new(RwLock::new(InputState::default())),
        }
    }
}

/// One world's input feed: the writer it is fed through, and the buffer that
/// receives what the writer publishes.
///
/// Spawning it gives a world both halves at once, so the caller keeps feeding
/// through its own handle while the world's tick reads the events out of the
/// buffer beside it. The buffer holds the events the world has not read yet,
/// which is what lets a world that dispatches its own key behaviours subscribe
/// to the same writer without sharing a list with anyone.
pub struct InputFeed(pub InputWriter);

impl Bundle for InputFeed {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.0.clone(), self.0.subscribe()]
    }
}

/// Start `world`'s input tick: everything queued becomes the tick's batch.
///
/// [`dispatch_input`] calls this, so a world that dispatches needs nothing
/// else. A world that reads input without dispatching — one where a
/// [`FrameSource`](crate::source::FrameSource) or a panel is the only reader —
/// calls this once per tick instead, before anything reads the batch.
///
/// Calling it in the same tick as [`dispatch_input`] is harmless but wasteful:
/// the refresh has already happened, and the dispatch finds nothing new.
///
/// Returns whether the world has an input buffer at all.
pub fn refresh_input(world: &World) -> bool {
    let entities: Vec<_> = world
        .root_scope()
        .query::<(&InputEvents, Entity)>()
        .iter()
        .map(|(_input_events, entity)| entity)
        .collect();
    let present = !entities.is_empty();
    for entity in entities {
        if let Ok(buffer) = world.root_scope().query_one::<&InputEvents>(entity) {
            buffer.refresh();
        }
    }
    present
}

/// Give `world` an input buffer of its own.
///
/// The buffer is subscribed from the world's [`InputWriter`], and a world that
/// already has one is left alone, so a buffer someone else made keeps its
/// events. Returns whether a buffer was added: a world with no writer has
/// nothing to subscribe to.
///
/// A world that never ticks keeps every event it is fed until it does, which
/// is why [`distribute_input`] subscribes the nested worlds, and why a world
/// spawned outside that hierarchy — one a runner started as a sibling —
/// subscribes itself from its own setup.
pub fn subscribe_input(world: &mut World) -> bool {
    if world
        .root_scope()
        .query::<&InputEvents>()
        .iter()
        .next()
        .is_some()
    {
        return false;
    }
    let events = {
        let mut writers = world.root_scope().query::<&InputWriter>();
        writers
            .iter()
            .next()
            .map(|input_writer| input_writer.subscribe())
    };
    let Some(events) = events else {
        return false;
    };
    // The buffer belongs to the shared subtree: a source reads the world's
    // input from its own scope, and a sibling's component is out of reach.
    let shared = world.shared();
    world
        .scope_mut(shared)
        .expect("the shared scope exists")
        .spawn(bundle![events]);
    true
}

/// Give every nested world the root world's input writer, and a buffer of
/// its own.
///
/// Input arrives in one world — the host's, where the window callbacks run —
/// while the worlds that read it are nested inside that one. A nested world
/// cannot see its parent's components, so the writer is copied down, and
/// subscribing to it here is what gives the nested world the state it reads:
/// a world with no buffer has no input at all. A writer or buffer already
/// there is left alone, so one put there deliberately is not replaced.
///
/// Returns whether anything was added. Recurses, so a world nested more than
/// one level deep is reached too.
pub fn distribute_input(world: &World) -> bool {
    let mut input_writer_rows = world.root_scope().query::<&InputWriter>();

    let Some(writer) = input_writer_rows.iter().next().cloned() else {
        return false;
    };
    distribute_into(world, &writer)
}

/// The recursion behind [`distribute_input`].
fn distribute_into(world: &World, writer: &InputWriter) -> bool {
    let mut changed = false;
    // Whether each child already reads input, asked and answered before any
    // child is written: the read borrows the child, and the write below needs
    // it back exclusively.
    let children: Vec<(Entity, bool)> = {
        let mut nested_worlds = world.root_scope().query::<(&World, Entity)>();
        nested_worlds
            .iter()
            .map(|(child, entity)| {
                let has = child
                    .root_scope()
                    .query::<&InputWriter>()
                    .iter()
                    .next()
                    .is_some();
                (entity, has)
            })
            .collect()
    };
    for (entity, has) in children {
        let nested = world
            .root_scope()
            .query_one::<&mut World>(entity)
            .map(|mut child| {
                let child = child.get();
                if !has {
                    child.root_scope_mut().spawn(bundle![writer.clone()]);
                }
                // A buffer subscribed here is what the child's own tick reads.
                let subscribed = subscribe_input(child);
                (subscribed, distribute_into(child, writer))
            });
        if let Ok((subscribed, nested)) = nested {
            changed |= !has || subscribed || nested;
        }
    }
    changed
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

/// Run the world's behaviour components once for each event it has not read.
///
/// The events come from this world's [`InputEvents`] and are consumed by the
/// dispatch, so a world that ticks slower than input arrives still reads all
/// of it, and a world that dispatches twice in a frame delivers the second
/// time only what arrived in between. With no queue in the world this does
/// nothing and returns `false`: a world that never asked for input has
/// nothing to deliver, and that is not an error.
///
/// Returns whether any event was delivered, so a caller can skip the
/// [`World::apply`] that would otherwise follow an empty dispatch.
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
/// The events are taken out of the queue before any callback runs, so a
/// callback may freely read and write the [`InputState`] — a common need,
/// since the resource is where "is Ctrl still held?" lives — and may even
/// feed input, which lands in the next dispatch.
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
/// Structural changes queue through [`unlit_ecs::Scope::queue`] and land when the
/// caller applies them. The caller applies **once**, after the whole dispatch,
/// not between callbacks: the behaviour components are iterated directly, so
/// the world is borrowed for the duration and cannot be applied to. Typically
/// that looks like:
///
/// ```rust
/// # use unlit3d::prelude::*;
/// # let mut world = World::new();
/// if dispatch_input(&world) {
///     world.apply().unwrap();
/// }
/// ```
pub fn dispatch_input(world: &World) -> bool {
    // The tick starts by moving everything queued into the batch, before any
    // behaviour runs: a callback may feed input itself, and that belongs to
    // the next tick rather than to this dispatch.
    if !refresh_input(world) {
        return false;
    }
    let mut input_events_rows = world.root_scope().query::<&InputEvents>();

    // The batch is copied out while the component is borrowed, so a callback
    // may read the world — including this component — freely.
    let Some(events) = input_events_rows
        .iter()
        .next()
        .map(|input_events| input_events.with_batch(|batch| batch.to_vec()))
    else {
        return false;
    };

    let delivered = !events.is_empty();

    for event in &events {
        match event {
            InputEvent::Key(event) => {
                for (on_key, entity) in world.root_scope().query::<(&mut OnKey, Entity)>().iter() {
                    on_key.run(world, entity, event);
                }
            }
            InputEvent::Mouse(event) => {
                for (on_mouse, entity) in
                    world.root_scope().query::<(&mut OnMouse, Entity)>().iter()
                {
                    on_mouse.run(world, entity, event);
                }
            }
            InputEvent::Pointer(event) => {
                for (on_pointer, entity) in world
                    .root_scope()
                    .query::<(&mut OnPointer, Entity)>()
                    .iter()
                {
                    on_pointer.run(world, entity, event);
                }
            }
            InputEvent::Touch(event) => {
                for (on_touch, entity) in
                    world.root_scope().query::<(&mut OnTouch, Entity)>().iter()
                {
                    on_touch.run(world, entity, event);
                }
            }
            InputEvent::Text(event) => {
                for (on_text, entity) in world.root_scope().query::<(&mut OnText, Entity)>().iter()
                {
                    on_text.run(world, entity, event);
                }
            }
            InputEvent::Ime(event) => {
                for (on_ime, entity) in world.root_scope().query::<(&mut OnIme, Entity)>().iter() {
                    on_ime.run(world, entity, event);
                }
            }
            // These carry no per-category payload to route, so only the
            // catch-all hears them.
            InputEvent::ModifiersChanged(_)
            | InputEvent::FocusChanged(_)
            | InputEvent::Resized { .. }
            | InputEvent::ScaleFactorChanged { .. } => {}
        }

        for (on_input, entity) in world.root_scope().query::<(&mut OnInput, Entity)>().iter() {
            on_input.run(world, entity, event);
        }
    }

    delivered
}

#[cfg(test)]
mod tests {
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
                    .is_some_and(|input_events| {
                        input_events.with_state(|state| state.modifiers.ctrl)
                    });
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
}
