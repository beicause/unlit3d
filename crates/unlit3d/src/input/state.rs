//! The state a world's input events add up to.
//!
//! [`InputState`] is level state rather than per-tick data: which modifiers are
//! held, where the pointer is, which buttons and fingers are down. It is built
//! by folding the events of each tick into it, and it outlives the tick that
//! built it.

use super::event::{
    InputEvent, Modifiers, MouseButtons, MouseEvent, PointerAction, PointerContact, PointerEvent,
    PointerKind, TouchEvent, TouchPhase,
};

/// How the input devices are now: the state the events left behind.
///
/// Which modifiers are held, where the pointer is, which buttons are down —
/// things that outlive the event that set them. The events themselves are not
/// here: they belong to the tick of the world that received them, in its
/// [`InputEvents`](super::InputEvents).
///
/// It lives inside a world's [`InputEvents`](super::InputEvents), built by that world's own tick,
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
///             let _ = world.root_scope().query_one::<&mut CtrlPresses>(entity).map(|mut presses| presses.get().0 += 1);
///         }
///     }),
/// ]);
///
/// input.push(InputEvent::ModifiersChanged(Modifiers { ctrl: true, ..Modifiers::default() }));
/// dispatch_input(&world);
///
/// assert_eq!(world.root_scope().query_one::<&CtrlPresses>(observer).unwrap().get().0, 1);
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
    /// world's own [`InputEvents`](super::InputEvents) instead.
    pub(super) fn push(&mut self, event: InputEvent) {
        self.record(&event);
    }
}

impl InputState {
    /// Record `event` and update the state it leaves behind.
    ///
    /// This is the whole state machine, because every state change a frame
    /// makes is carried by the event that caused it. It is private: a world's
    /// state is only ever built by its own [`InputEvents::refresh`](super::InputEvents::refresh), from the
    /// events fed to it.
    pub(super) fn record(&mut self, event: &InputEvent) {
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
