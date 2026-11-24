//! The input a page receives, and the callbacks that watch it.
//!
//! A [`BlitzSource`](crate::blitz::BlitzSource) turns the frame's
//! [`InputEvent`]s into the [`UiEvent`]s Blitz dispatches, in the coordinates a
//! document lays out in, and runs each one through Blitz's event driver. Blitz
//! does what a browser does without a script: hover, focus, scrolling, text
//! editing, and the checked state of every form control.
//!
//! [`OnBlitzEvent`] is the way back out. Blitz has no scripting engine, so the
//! only reaction a document can have to an event besides Blitz's own is one a
//! Rust callback states: it sees every DOM event, along with the page it was
//! dispatched into, and may read and change the document there.
//!
//! # Units
//!
//! The crate reports pointer positions in physical pixels; a document lays out
//! in CSS pixels, so every position here is divided by the scale factor and is
//! already in the space Blitz expects.
//!
//! # Replay
//!
//! A batch is a sequence of changes, so what it is read against is what stood
//! before its first event, not the state it has since folded into. That state
//! is [`InputBaseline`], captured beside the batch when the batch was queued;
//! the translator starts each batch from it and advances event by event. A move
//! that comes before the batch's own release still holds the button, a wheel
//! names the place the pointer stood when it turned, and a press the frame never
//! saw is still held.
//!
//! # Duplicates
//!
//! One physical action arrives more than once. The crate reports a mouse as
//! [`MouseEvent`]s and the same action again as a [`PointerEvent`], and the
//! pointer a document hovers with is fed from the first that exists for the
//! device — the mouse for a mouse, the touch for a finger — so the coarser
//! duplicate neither scrolls the page twice nor leaves a stale hover behind.
//!
//! A dispatch reaches a callback more than once for the same reason: Blitz
//! synthesises the mouse and touch compatibility events a browser would, so a
//! click is reported as `pointerup`, `mouseup` and `click`. A callback that
//! reacts to one of them filters on the name of [`BlitzEvent::data`].
//!
//! # Keys
//!
//! The crate reports a key by its physical position, and a document names a key
//! by the character it types. The two agree for an unshifted US layout and part
//! way for a shifted one — `Shift+1` is `!`, and a letter is upper-cased —
//! but a key that produces something else through the layout reaches the
//! document only through its text, which is what a text field inserts from
//! anyway. The event also carries the physical key as a code and the side of
//! the keyboard it sits on, which is what a shortcut matches against.

use blitz_dom::{BaseDocument, Document, EventDriver, EventHandler, NodeId};
use blitz_traits::SmolStr;
use blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta,
    BlitzWheelEvent, DomEvent, DomEventData, EventState, KeyState, MouseEventButton,
    MouseEventButtons, Point, PointerCoords, PointerDetails, UiEvent,
};
use blitz_traits::shell::ShellProvider as _;
use keyboard_types::{Code, Key as DomKey, Location, Modifiers};
use unlit_ecs::{Entity, Scope};

use crate::input::{
    ImeEvent, ImeKind, InputBaseline, InputEvent, KeyEvent, MouseButton, MouseEvent, PointerAction,
    PointerEvent, PointerKind, TextEvent, TouchEvent, TouchPhase, WheelUnit,
};

use super::document::BlitzShell;
use super::keys::{code_of, key_of, location_of};

/// A callback that runs for every DOM event a page dispatches.
type EventCallback = Box<dyn FnMut(Scope<'_>, Entity, &mut BlitzEvent<'_>)>;

/// A callback run for every DOM event a page dispatches.
///
/// Blitz has no scripting engine: this is how a document's events reach Rust
/// code. Spawn one under the [`BlitzSource`](crate::blitz::BlitzSource) that
/// draws the page, beside the [`BlitzDocument`](super::BlitzDocument); the
/// source runs every callback its scope carries, in query order, for every event
/// the document dispatches.
///
/// # What a callback may do
///
/// It is handed a [`BlitzEvent`], which reads the event and the node it targets
/// and reaches the document, so a click can read a form field or change the
/// page. It may also read the world through its [`Scope`] and queue structural
/// changes for the frame loop to apply.
///
/// It must not reach for its own [`OnBlitzEvent`], nor for the page's
/// [`BlitzDocument`](super::BlitzDocument): both are borrowed while it runs, and
/// asking for either is a borrow panic rather than a compile error.
///
/// # What it is called for
///
/// Every event the document dispatches, which for one physical action is more
/// than one: Blitz reports a click as pointer, mouse and click events, and a key
/// press as a key event and the text it produced. A callback that reacts once
/// filters on the name of [`BlitzEvent::data`].
pub struct OnBlitzEvent(pub EventCallback);

impl OnBlitzEvent {
    /// Wrap `f` as an [`OnBlitzEvent`].
    pub fn new(f: impl FnMut(Scope<'_>, Entity, &mut BlitzEvent<'_>) + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Run this callback for `entity`, on `event`.
    pub(crate) fn run(&mut self, scope: Scope<'_>, entity: Entity, event: &mut BlitzEvent<'_>) {
        (self.0)(scope, entity, event)
    }
}

/// One DOM event, with the page it was dispatched into.
///
/// A view of the dispatch rather than a value of its own: it lives for the call
/// it was handed to, and the document behind it is the one the page is laid out
/// from.
pub struct BlitzEvent<'a> {
    event: &'a DomEvent,
    chain: &'a [NodeId],
    document: &'a mut BaseDocument,
    state: &'a mut EventState,
}

impl BlitzEvent<'_> {
    /// The event's payload.
    ///
    /// Its name — `click`, `input`, `keydown` — is
    /// [`DomEventData::name`], and that is what a callback filters on.
    ///
    /// A pointer action is reported as a pointer event and again as the mouse or
    /// touch event a browser would synthesise for it, so this is also how a
    /// callback tells the two apart.
    pub fn data(&self) -> &DomEventData {
        &self.event.data
    }

    /// The node the event was dispatched to.
    pub fn target(&self) -> NodeId {
        self.event.target
    }

    /// The target and the ancestors the event bubbles through, target first.
    ///
    /// Every callback of the page is run once for the whole chain, rather than
    /// once per node: an event is delivered to all of them together.
    pub fn chain(&self) -> &[NodeId] {
        self.chain
    }

    /// The page the event was dispatched into, to read or change.
    ///
    /// A change is recorded by the document and laid out once the event has been
    /// dispatched, so a callback may change the page it is reacting to without
    /// the dispatch reading a half-changed document.
    pub fn document_mut(&mut self) -> &mut BaseDocument {
        self.document
    }

    /// Stop Blitz's own reaction to the event.
    ///
    /// A cancelled click does not check the box it landed on, and a cancelled
    /// key press does not insert its character or scroll a text field.
    ///
    /// Every callback of the page is run for every event whether or not one of
    /// them cancelled it: an event reaches the whole page at once rather than
    /// stopping at the first callback that reacted to it.
    pub fn prevent_default(&mut self) {
        self.state.prevent_default();
    }

    /// Ask for another frame.
    pub fn request_redraw(&mut self) {
        self.state.request_redraw();
    }
}

/// Runs a page's callbacks as Blitz dispatches its events.
pub(crate) struct BlitzHandler<'a> {
    /// The scope of the source that draws the page, whose callbacks are run.
    scope: Scope<'a>,
    /// What the page asks of the frame, for a callback that asks for one.
    shell: &'a BlitzShell,
}

impl<'a> BlitzHandler<'a> {
    /// Run the callbacks `scope` carries for the events `page` dispatches.
    pub(crate) fn new(scope: Scope<'a>, shell: &'a BlitzShell) -> Self {
        Self { scope, shell }
    }

    /// Dispatch one event to the page's callbacks.
    pub(crate) fn dispatch(&mut self, document: &mut BaseDocument, event: &UiEvent) {
        let mut driver = EventDriver::new(
            document,
            Self {
                scope: self.scope,
                shell: self.shell,
            },
        );
        driver.handle_ui_event(event.clone());
    }
}

impl EventHandler for BlitzHandler<'_> {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        doc: &mut dyn Document,
        state: &mut EventState,
    ) {
        {
            let mut document = doc.inner_mut();
            for (callback, entity) in self.scope.query::<(&mut OnBlitzEvent, Entity)>().iter() {
                let mut view = BlitzEvent {
                    event: &*event,
                    chain,
                    document: &mut document,
                    state: &mut *state,
                };
                callback.run(self.scope, entity, &mut view);
            }
        }
        if event.request_redraw || state.redraw_is_requested() {
            self.shell.request_redraw();
        }
    }
}

/// Translates a frame's input into the events a document dispatches.
///
/// Kept between frames: the pointer's last position, the buttons and modifiers
/// held, and the buffer the events are collected in all carry over, so a click
/// whose press and release fall in different frames still names the button that
/// is down.
///
/// What is held is held per pointer: the mouse's buttons, whether a pen is
/// pressed, and the fingers down are three separate states, because a page
/// reads a pointer event's buttons as that pointer's own and a device lifting
/// says nothing about the others.
#[derive(Default)]
pub(crate) struct Translator {
    /// Physical pixels per CSS pixel.
    scale: f32,
    /// The pointer's last position, in physical pixels.
    position: [f32; 2],
    /// The modifier keys held.
    modifiers: crate::input::Modifiers,
    /// The mouse buttons held.
    buttons: MouseEventButtons,
    /// Whether a pointer fed from [`Self::pointer`] is pressed.
    ///
    /// A pen and an unnamed device are their own pointer: their press is not a
    /// mouse press, so a mouse released while the pen stays down does not lift
    /// the pen, and a pen hovering over the page holds nothing while the mouse's
    /// button is down.
    pointer_down: bool,
    /// The fingers down, oldest first.
    ///
    /// A finger is one of several, so a page is told which contact is the
    /// primary one and holds a button until the last of them lifts.
    contacts: Vec<u64>,
    /// The translated events, kept between frames.
    pending: Vec<UiEvent>,
}

/// What one frame states to a [`Translator`].
#[derive(Clone, Copy)]
pub(crate) struct Frame<'a> {
    /// The frame's events, in the order they arrived.
    pub(crate) batch: &'a [InputEvent],
    /// Physical pixels per CSS pixel.
    pub(crate) scale: f32,
    /// The state that stood when the batch was queued: what its replay starts
    /// from.
    pub(crate) start: &'a InputBaseline,
    /// How far the document is scrolled, which turns a client position into the
    /// page position a document hovers with.
    pub(crate) scroll: [f64; 2],
    /// The page's own size, in CSS pixels, which is what a wheel step stated in
    /// pages scrolls by.
    pub(crate) viewport: [f32; 2],
}

impl Translator {
    /// Translate the frame's events, returning them in dispatch order.
    ///
    /// Called for every frame, not only for the ones that dispatch something: a
    /// page hovers with the pointer's position and reports the modifiers that
    /// are held even when nothing happened to it, and the scale factor a
    /// position is divided by changes without an event of its own.
    pub(crate) fn translate(&mut self, frame: Frame<'_>) -> &[UiEvent] {
        self.scale = if frame.scale > 0.0 { frame.scale } else { 1.0 };
        self.replay_from(frame.start);
        self.pending.clear();
        for event in frame.batch {
            self.event(event, frame);
        }
        &self.pending
    }

    /// Put the pointer state back to what `start` states, before the batch's
    /// own events move it on.
    ///
    /// This is what makes a replay a replay: the batch says what changed, not
    /// what stood, so reading it against the state it has since folded into
    /// reports every change twice and every earlier position too late.
    fn replay_from(&mut self, start: &InputBaseline) {
        // A window that had no pointer position keeps the last one this
        // translator saw: a document hovers where the pointer last was.
        if let Some(cursor) = start.cursor.or(start.pointer) {
            self.position = cursor;
        }
        self.modifiers = start.modifiers;
        self.buttons = buttons_of(start.buttons);
        // A pen has no button of its own in the baseline: the crate states a
        // pen's press as a pointer contact, and a baseline holding one is a
        // pointer that was already down when the batch was queued.
        self.pointer_down = start
            .pointers
            .iter()
            .any(|contact| matches!(contact.kind, PointerKind::Pen | PointerKind::Other));
        self.contacts.clear();
        // A mouse contact is not a finger: the page's primary pointer is a
        // finger only when the touch screen states one.
        self.contacts
            .extend(start.touches.iter().map(|(id, _)| *id));
    }

    /// Translate one event, appending what it stands for.
    fn event(&mut self, event: &InputEvent, frame: Frame<'_>) {
        match event {
            InputEvent::Mouse(event) => self.mouse(event, frame),
            InputEvent::Pointer(event) => self.pointer(event, frame),
            InputEvent::Touch(event) => self.touch(event, frame),
            InputEvent::Key(event) => self.key(event),
            InputEvent::Text(event) => self.text(event),
            InputEvent::Ime(ImeEvent { kind }) => self.pending.push(UiEvent::Ime(ime_event(kind))),
            InputEvent::ModifiersChanged(modifiers) => self.modifiers = *modifiers,
            // A page has no window of its own: it is laid out at the size the
            // source states, and it holds no focus besides the one its markup
            // gives it, so neither a resize nor the window's focus is an event
            // it can react to.
            InputEvent::FocusChanged(_)
            | InputEvent::Resized { .. }
            | InputEvent::ScaleFactorChanged { .. } => {}
        }
    }

    /// Translate a mouse event.
    fn mouse(&mut self, event: &MouseEvent, frame: Frame<'_>) {
        let scroll = frame.scroll;
        match event {
            MouseEvent::Moved { position } => {
                self.position = *position;
                let coords = self.coords(scroll);
                let pointer = self.pointer_event(
                    BlitzPointerId::Mouse,
                    MouseEventButton::Main,
                    coords,
                    true,
                    self.buttons,
                );
                self.pending.push(UiEvent::PointerMove(pointer));
            }
            MouseEvent::Button {
                position,
                button,
                pressed,
                modifiers,
            } => {
                // A press with no position leaves the last known one: a
                // fabricated corner would misdirect the document's hit test.
                if let Some(position) = *position {
                    self.position = position;
                }
                self.modifiers = *modifiers;
                // A button neither side has a name for is not pressed on the
                // page: the mouse of a platform with more buttons than a
                // document has is not the primary button by default.
                let Some(button) = button_of(*button) else {
                    return;
                };
                self.buttons.set(MouseEventButtons::from(button), *pressed);
                let coords = self.coords(scroll);
                let pointer =
                    self.pointer_event(BlitzPointerId::Mouse, button, coords, true, self.buttons);
                self.pending.push(if *pressed {
                    UiEvent::PointerDown(pointer)
                } else {
                    UiEvent::PointerUp(pointer)
                });
            }
            MouseEvent::Left => {
                let coords = self.coords(scroll);
                let pointer = self.pointer_event(
                    BlitzPointerId::Mouse,
                    MouseEventButton::Main,
                    coords,
                    true,
                    self.buttons,
                );
                self.pending.push(UiEvent::PointerLeave(pointer));
            }
            // The wheel's phase is dropped: a page scrolls by the distance it is
            // given, and Blitz has no momentum to end.
            MouseEvent::Wheel { delta, unit, .. } => {
                let delta = match unit {
                    WheelUnit::Pixel => BlitzWheelDelta::Pixels(
                        f64::from(delta[0]) / f64::from(self.scale),
                        f64::from(delta[1]) / f64::from(self.scale),
                    ),
                    WheelUnit::Line => {
                        BlitzWheelDelta::Lines(f64::from(delta[0]), f64::from(delta[1]))
                    }
                    // A page is a distance Blitz has no unit for, so a step of
                    // one scrolls by the height of the page being scrolled.
                    WheelUnit::Page => BlitzWheelDelta::Pixels(
                        f64::from(delta[0]) * f64::from(frame.viewport[0]),
                        f64::from(delta[1]) * f64::from(frame.viewport[1]),
                    ),
                };
                let coords = self.coords(scroll);
                self.pending.push(UiEvent::Wheel(BlitzWheelEvent {
                    delta,
                    coords,
                    buttons: self.buttons,
                    mods: modifiers_of(self.modifiers),
                    element: Point::default(),
                }));
            }
        }
    }

    /// Translate a pointer event.
    ///
    /// A mouse and a finger already report their actions as [`MouseEvent`]s and
    /// [`TouchEvent`]s, which carry more than a pointer event does; only a pen
    /// and a device the crate cannot name are fed from here.
    ///
    /// Such a pointer is its own device: the button it reports is its own
    /// press, not the mouse's, so a mouse released while the pen stays down
    /// leaves the pen pressed and a pen hovering holds nothing while the
    /// mouse's button is down.
    fn pointer(&mut self, event: &PointerEvent, frame: Frame<'_>) {
        if !matches!(event.kind, PointerKind::Pen | PointerKind::Other) {
            return;
        }
        // A pointer event states where it happened by stating the position it
        // is at; one that names none carries no position to report.
        let Some(position) = event.position else {
            return;
        };
        self.position = position;
        self.modifiers = event.modifiers;
        // The press is taken before the event is described: the page is told
        // about a pointer that already carries the change this event states.
        // Leaving the window is not a release, which is what a drag out of it
        // relies on: the mouse keeps its buttons for the same reason.
        match event.action {
            PointerAction::Pressed => self.pointer_down = true,
            PointerAction::Released { .. } => self.pointer_down = false,
            PointerAction::Moved
            | PointerAction::Left
            | PointerAction::Zoom(_)
            | PointerAction::Rotate(_) => {}
        }
        let coords = self.coords(frame.scroll);
        let buttons = if self.pointer_down {
            MouseEventButtons::Primary
        } else {
            MouseEventButtons::None
        };
        let pointer = self.pointer_event(
            BlitzPointerId::Pen,
            MouseEventButton::Main,
            coords,
            true,
            buttons,
        );
        match event.action {
            PointerAction::Moved => self.pending.push(UiEvent::PointerMove(pointer)),
            PointerAction::Pressed => self.pending.push(UiEvent::PointerDown(pointer)),
            PointerAction::Released { cancelled } => self.pending.push(if cancelled {
                UiEvent::PointerCancel(pointer)
            } else {
                UiEvent::PointerUp(pointer)
            }),
            PointerAction::Left => self.pending.push(UiEvent::PointerLeave(pointer)),
            // A pinch and a rotation are gestures a document has no use for:
            // it scrolls, but it does not zoom or turn.
            PointerAction::Zoom(_) | PointerAction::Rotate(_) => {}
        }
    }

    /// Translate a touch event.
    ///
    /// A screen can be touched with more than one finger, and the fingers are
    /// independent contacts: the page holds the primary button while any of them
    /// is down, and names the oldest one as the primary contact, which is the
    /// contact that drags and hovers. The pointer a page reports a position with
    /// is the one of the contact the event came from, so lifting a second finger
    /// leaves the first where it was instead of jumping to the second.
    ///
    /// The contacts are advanced before the event is described, so the pointer
    /// the page is told about carries what this event leaves behind: the finger
    /// that presses is one of the fingers down, and the finger that lifts is
    /// not. The buttons of the mouse are not touched: a finger and a mouse are
    /// different pointers, and one lifting says nothing about the other.
    fn touch(&mut self, event: &TouchEvent, frame: Frame<'_>) {
        match event.phase {
            TouchPhase::Started => {
                if !self.contacts.contains(&event.id) {
                    self.contacts.push(event.id);
                }
            }
            TouchPhase::Moved => {}
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if let Some(index) = self.contacts.iter().position(|id| *id == event.id) {
                    self.contacts.remove(index);
                }
            }
        }
        let primary = self.contacts.first() == Some(&event.id);
        let buttons = if self.contacts.contains(&event.id) {
            MouseEventButtons::Primary
        } else {
            MouseEventButtons::None
        };
        self.position = event.position;
        let coords = self.coords(frame.scroll);
        let mut pointer = self.pointer_event(
            BlitzPointerId::Finger(event.id),
            MouseEventButton::Main,
            coords,
            primary,
            buttons,
        );
        pointer.details.pressure = f64::from(event.force.unwrap_or(0.0));
        match event.phase {
            TouchPhase::Started => self.pending.push(UiEvent::PointerDown(pointer)),
            TouchPhase::Moved => self.pending.push(UiEvent::PointerMove(pointer)),
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.pending
                    .push(if matches!(event.phase, TouchPhase::Cancelled) {
                        UiEvent::PointerCancel(pointer)
                    } else {
                        UiEvent::PointerUp(pointer)
                    });
            }
        }
    }

    /// Translate a key event.
    ///
    /// A character key arrives again as the [`InputEvent::Text`] it produced,
    /// and that is the event a text field inserts from, so a character is only
    /// dispatched as a key while a command modifier is held: that is the
    /// shortcut a document reacts to, and the platform produces no text for it.
    fn key(&mut self, event: &KeyEvent) {
        self.modifiers = event.modifiers;
        let key = key_of(event.key, event.modifiers.shift);
        // A character that is typed without a command modifier arrives again as
        // the text it produced, and the text is what a field inserts. The press
        // alone is dropped; the release still goes out, so a page that watches
        // for one sees a key it saw pressed.
        if event.pressed && !event.modifiers.command && matches!(key, DomKey::Character(_)) {
            return;
        }
        self.pending.push(self.key_event(
            code_of(event.key),
            location_of(event.location),
            key,
            event.pressed,
            event.repeat,
        ));
    }

    /// Translate the text a key produced, or a paste, which inserts the same
    /// way.
    fn text(&mut self, event: &TextEvent) {
        // Text carries no physical key of its own: it is what a key produced,
        // not which key produced it.
        let mut key = self.key_event(
            Code::Unidentified,
            Location::Standard,
            DomKey::Character(event.0.clone()),
            true,
            false,
        );
        if let UiEvent::KeyDown(key) = &mut key {
            key.text = Some(SmolStr::new(event.0.as_str()));
        }
        self.pending.push(key);
    }

    /// Wrap `key` as the event a document dispatches for it.
    ///
    /// A page reads the physical key as a code and the side of the keyboard it
    /// is on, which the input layer already states, so both are carried across
    /// rather than left blank.
    fn key_event(
        &self,
        code: Code,
        location: Location,
        key: DomKey,
        pressed: bool,
        repeat: bool,
    ) -> UiEvent {
        let event = BlitzKeyEvent {
            key,
            code,
            location,
            modifiers: modifiers_of(self.modifiers),
            is_auto_repeating: repeat,
            is_composing: false,
            state: if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            text: None,
        };
        if pressed {
            UiEvent::KeyDown(event)
        } else {
            UiEvent::KeyUp(event)
        }
    }

    /// The pointer position, in the page coordinates a dispatch hovers with.
    fn coords(&self, scroll: [f64; 2]) -> PointerCoords {
        let x = self.position[0] / self.scale;
        let y = self.position[1] / self.scale;
        PointerCoords {
            page_x: x + scroll[0] as f32,
            page_y: y + scroll[1] as f32,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        }
    }

    /// The pointer event a document dispatches for `id`.
    ///
    /// `is_primary` says whether this is the contact the page drags and
    /// hovers with, which for a mouse and a pen is always this one and for a
    /// finger is the oldest one still down. `buttons` is what the pointer named
    /// by `id` holds, not what every device together holds: a page dragging with
    /// a finger reads this to decide whether the drag continues, and the mouse's
    /// button being down says nothing about a pen or a finger.
    fn pointer_event(
        &self,
        id: BlitzPointerId,
        button: MouseEventButton,
        coords: PointerCoords,
        is_primary: bool,
        buttons: MouseEventButtons,
    ) -> BlitzPointerEvent {
        BlitzPointerEvent {
            id,
            is_primary,
            coords,
            button,
            buttons,
            mods: modifiers_of(self.modifiers),
            details: PointerDetails::default(),
            element: Point::default(),
            // The contacts a drag is built from are tracked by the driver that
            // needs them; a dispatch here states one action at a time, so the
            // list it carries is empty.
            active_pointers: Default::default(),
        }
    }
}

/// The buttons a document reports as held.
fn buttons_of(buttons: crate::input::MouseButtons) -> MouseEventButtons {
    let mut event = MouseEventButtons::None;
    for (button, flag) in [
        (
            crate::input::MouseButtons::PRIMARY,
            MouseEventButtons::Primary,
        ),
        (
            crate::input::MouseButtons::SECONDARY,
            MouseEventButtons::Secondary,
        ),
        (
            crate::input::MouseButtons::MIDDLE,
            MouseEventButtons::Auxiliary,
        ),
        (crate::input::MouseButtons::BACK, MouseEventButtons::Fourth),
        (
            crate::input::MouseButtons::FORWARD,
            MouseEventButtons::Fifth,
        ),
    ] {
        event.set(flag, buttons.contains(button));
    }
    event
}

/// The button a document reports a crate button as.
///
/// `None` for a button the document has no name for, which it therefore never
/// sees pressed.
fn button_of(button: MouseButton) -> Option<MouseEventButton> {
    Some(match button {
        MouseButton::Primary => MouseEventButton::Main,
        MouseButton::Secondary => MouseEventButton::Secondary,
        MouseButton::Middle => MouseEventButton::Auxiliary,
        MouseButton::Back => MouseEventButton::Fourth,
        MouseButton::Forward => MouseEventButton::Fifth,
        MouseButton::Other(_) => return None,
    })
}

/// The modifier keys a document reports as held.
fn modifiers_of(modifiers: crate::input::Modifiers) -> Modifiers {
    let mut event = Modifiers::empty();
    event.set(Modifiers::ALT, modifiers.alt);
    event.set(Modifiers::CONTROL, modifiers.ctrl);
    event.set(Modifiers::SHIFT, modifiers.shift);
    event.set(Modifiers::META, modifiers.mac_cmd);
    event
}

/// The input-method event a document dispatches for the crate's.
fn ime_event(kind: &ImeKind) -> BlitzImeEvent {
    match kind {
        ImeKind::Preedit { text, active_range } => {
            BlitzImeEvent::Preedit(text.clone(), active_range.clone().map(|r| (r.start, r.end)))
        }
        ImeKind::Commit(text) => BlitzImeEvent::Commit(text.clone()),
        ImeKind::DeleteSurrounding { before, after } => BlitzImeEvent::DeleteSurrounding {
            before_bytes: *before,
            after_bytes: *after,
        },
        ImeKind::Enabled => BlitzImeEvent::Enabled,
        ImeKind::Disabled => BlitzImeEvent::Disabled,
    }
}
