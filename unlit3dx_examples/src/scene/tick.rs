//! The behaviour that ticks a gameplay object.
//!
//! A tick is dispatched as a frame, so every behaviour in the gameplay world is
//! an [`OnFrame`]. An object is the component that carries what it owns plus the
//! behaviour that ticks it, spawned as one bundle, so adding an object to the
//! scene is spawning an entity and never editing a list.
//!
//! Nothing here states an order. The frame runs the behaviours that state none
//! in the order they were spawned, so this world ticks in the order its setup
//! spawns its objects: the assets before the objects that read them, and the
//! panel's publish after everything that could change what it shows. A
//! behaviour that has to run before another whatever that order becomes is the
//! one that states an order, and none of these does.
//!
//! The behaviour advances that object's own component, looked up on the entity
//! the bundle was spawned on. It holds the component exclusively for as long as
//! it runs, so a tick that reaches for the component it advances a second time
//! finds it borrowed: everything else a tick reads, it reads through the world
//! it is handed.

use unlit_ecs::World;
use unlit3d::frame::{Frame, OnFrame};

/// The behaviour that advances the `T` of the entity it is spawned on.
///
/// An object spawns its component and this behaviour as one bundle, so the
/// component is found on the behaviour's own entity rather than by reaching for
/// a well-known one, and every run looks it up afresh: the behaviour outlives
/// no component and borrows none between runs.
pub(super) fn advancing<T: 'static>(
    mut tick: impl FnMut(&mut T, &World, &mut Frame) + 'static,
) -> OnFrame {
    OnFrame::new(move |world, entity, frame| {
        let _ = world
            .root_scope()
            .query_one::<&mut T>(entity)
            .map(|mut component| tick(&mut component, world, frame));
    })
}
