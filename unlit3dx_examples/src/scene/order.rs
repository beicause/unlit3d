//! Where in a gameplay tick a behaviour runs.
//!
//! A tick is dispatched as a frame, so every behaviour in the gameplay world is
//! an [`OnFrame`] placed by its order. An object is the component that carries
//! what it owns plus the behaviour that ticks it, spawned as one bundle, so
//! adding an object to the scene is spawning an entity and never editing a list.
//!
//! The behaviour advances that object's own component, looked up on the entity
//! the bundle was spawned on. It holds the component exclusively for as long as
//! it runs, so a tick that reaches for the component it advances a second time
//! finds it borrowed: everything else a tick reads, it reads through the world
//! it is handed.

use unlit_ecs::World;
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};

/// Where in a gameplay tick a behaviour runs.
///
/// These are the bands a scene's own frame uses: the tick's input is
/// dispatched before any object reads it, the objects tick in the order they
/// name, and the panel publishes last, once every object has settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ScenePhase {
    /// The tick's input, dispatched before any object ticks, so every object
    /// sees the events of its own tick.
    Input = 10,
    /// The objects' ticks.
    Objects = 100,
    /// The panel's publish, after every object that could change what it
    /// shows has ticked.
    Publish = 400,
}

impl ScenePhase {
    /// The frame behaviour order of `place` inside this phase.
    ///
    /// A phase is a band of orders, wide enough for the behaviours it holds:
    /// an object is placed in the ticking phase by its own place, and nothing
    /// outside that band can land between two objects.
    #[must_use]
    pub const fn at(self, place: i32) -> FrameBehaviourOrder {
        FrameBehaviourOrder(self as i32 + place)
    }
}

/// The behaviour that advances the `T` of the entity it is spawned on.
///
/// An object spawns its component and this behaviour as one bundle, so the
/// component is found on the behaviour's own entity rather than by reaching for
/// a well-known one, and every run looks it up afresh: the behaviour outlives
/// no component and borrows none between runs.
pub(super) fn advancing<T: 'static>(
    place: FrameBehaviourOrder,
    mut tick: impl FnMut(&mut T, &World, &mut Frame) + 'static,
) -> OnFrame {
    OnFrame::new(place, move |world, entity, frame| {
        let _ = world
            .root_scope()
            .query_one::<&mut T>(entity)
            .map(|mut component| tick(&mut component, world, frame));
    })
}

/// The place in [`ScenePhase::Objects`] the assets are collected at: the build
/// lands before the objects that need the instances it hands out.
pub(super) const TICK_SCENE: i32 = 0;

/// The place the balls are advanced at: after the assets exist, before the
/// music.
pub(super) const TICK_BALLS: i32 = 100;

/// The place the music is advanced at: last, once the balls are out.
pub(super) const TICK_AUDIO: i32 = 200;
