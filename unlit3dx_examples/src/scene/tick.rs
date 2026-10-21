//! The tick a scene object runs on.
//!
//! Every object in the gameplay world advances once per gameplay tick, in
//! the order it declares. An object is a component plus the behaviour that
//! ticks it, so adding one to the scene is spawning an entity and never
//! editing a list.

use unlit_ecs::World;
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};

/// A scene object that advances once per gameplay tick.
///
/// `tick` runs with the world its entity lives in, so an object reaches its
/// neighbours through [`unlit_ecs::Scope::query_one`]. It must not touch a
/// component of its own type: the driver already holds that one.
pub trait Tick {
    /// Advance the object by one tick, whose duration is `frame`'s.
    fn tick(&mut self, world: &World, frame: &Frame);
}

/// The order the floor's tick runs at: before anything that lands on it.
pub const TICK_FLOOR: i32 = 50;

/// The order the shared state's tick runs at: the build lands before the
/// objects that need the slots it hands out.
pub const TICK_SCENE: i32 = 100;

/// The order the shapes' tick runs at: after the slots exist, before the
/// music that answers them.
pub const TICK_BALLS: i32 = 200;

/// The order the audio object's tick runs at: last, once the shapes are out.
pub const TICK_AUDIO: i32 = 300;

/// The behaviour that ticks `T` for the entity it is spawned on, at `order`.
///
/// The gameplay world dispatches its tick as a frame, so an object's tick is
/// an [`OnFrame`] like any other: the object declares where it runs, and the
/// scene never sorts its objects itself.
pub fn tick_behaviour<T: Tick + 'static>(order: i32) -> OnFrame {
    OnFrame::new(FrameBehaviourOrder(order), |world, entity, frame| {
        let _ = world
            .root_scope()
            .query_one::<&mut T>(entity)
            .map(|mut state| state.tick(world, frame));
    })
}
