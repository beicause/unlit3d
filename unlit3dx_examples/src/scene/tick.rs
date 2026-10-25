//! The behaviour that ticks a gameplay object.
//!
//! A tick is dispatched as a frame, so every behaviour in the gameplay world is
//! an [`OnFrame`]. An object is the component that carries what it owns plus the
//! behaviour that ticks it, spawned as one bundle, so adding an object to the
//! scene is spawning an entity and never editing a list.
//!
//! Most objects state no order: they tick in the default band, where the
//! dispatch promises nothing about which of them runs first, and none of them
//! reads what another writes. The two that do — the balls, which read the
//! assets built before them, and the panel's publish, which reads what every
//! object wrote — state an order with [`advancing_in`] at both ends, so the
//! dependency holds however the setup spawns them.
//!
//! The behaviour advances that object's own component, looked up on the entity
//! the bundle was spawned on. It holds the component exclusively for as long as
//! it runs, so a tick that reaches for the component it advances a second time
//! finds it borrowed: everything else a tick reads, it reads through the world
//! it is handed.

use unlit_ecs::World;
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};

/// The behaviour that advances the `T` of the entity it is spawned on.
///
/// An object spawns its component and this behaviour as one bundle, so the
/// component is found on the behaviour's own entity rather than by reaching for
/// a well-known one, and every run looks it up afresh: the behaviour outlives
/// no component and borrows none between runs.
pub(super) fn advancing<T: 'static>(
    tick: impl FnMut(&mut T, &World, &mut Frame) + 'static,
) -> OnFrame {
    advance(FrameBehaviourOrder::default(), tick)
}

/// The behaviour that advances the `T` of the entity it is spawned on, at
/// `order`.
///
/// For an object whose tick reads what another object's tick writes: the order
/// is what makes the read see this frame's write, where two ticks in the
/// default band have no order between them.
pub(super) fn advancing_in<T: 'static>(
    order: FrameBehaviourOrder,
    tick: impl FnMut(&mut T, &World, &mut Frame) + 'static,
) -> OnFrame {
    advance(order, tick)
}

/// `advancing` and `advancing_in` with the order as a parameter.
fn advance<T: 'static>(
    order: FrameBehaviourOrder,
    mut tick: impl FnMut(&mut T, &World, &mut Frame) + 'static,
) -> OnFrame {
    OnFrame::with_order(order, move |world, entity, frame| {
        let _ = world
            .root_scope()
            .query_one::<&mut T>(entity)
            .map(|mut component| tick(&mut component, world, frame));
    })
}
