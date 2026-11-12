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
//!             let _ = world.root_scope().query_one::<&mut Presses>(entity).map(|mut presses| presses.get().0 += 1);
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
use std::sync::{Arc, RwLock};

use crate::relay::{Stream, StreamReader};
use unlit_ecs::sync;
use unlit_ecs::{Bundle, DynamicBundle, Entity, World, bundle};

use crate::behaviour::behaviour;

mod event;
mod state;

pub use event::{
    ImeEvent, ImeKind, InputEvent, Key, KeyEvent, Modifiers, MouseButton, MouseButtons, MouseEvent,
    PointerAction, PointerContact, PointerEvent, PointerKind, TextEvent, TouchEvent, TouchPhase,
    WheelUnit,
};
pub use state::InputState;

#[cfg(feature = "winit")]
pub mod winit;

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
        let state = sync::read(&self.state);
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
        // The batch is out of its lock before the closure runs, so folding
        // the events into the state is the only lock held here.
        self.events.with_batch(|batch| {
            let mut state = sync::write(&self.state);
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
        if let Ok(mut buffer) = world.root_scope().query_one::<&InputEvents>(entity) {
            buffer.get().refresh();
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
mod tests;
