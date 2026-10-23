//! The tick a scene object runs on.
//!
//! Every object in the gameplay world advances once per gameplay tick, at the
//! place in the tick it declares, and reaches the other worlds through the
//! [`Scene`] the tick hands it. An object is a component plus the behaviour
//! that ticks it, so adding one to the scene is spawning an entity and never
//! editing a list.

use unlit_ecs::World;
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};

use super::Scene;

/// A scene object that advances once per gameplay tick.
///
/// `tick` runs with the world its entity lives in and the scene's handle on
/// the worlds beside it, so an object reaches its neighbours through
/// [`unlit_ecs::Scope::query_one`] and the other worlds through `scene`. It
/// must not touch a component of its own type: the driver already holds that
/// one.
pub trait Tick {
    /// Advance the object by one tick, whose duration is `frame`'s.
    fn tick(&mut self, world: &World, frame: &Frame, scene: &Scene);
}

/// Where in a gameplay tick a behaviour runs.
///
/// A tick is dispatched as a frame, so a behaviour is placed by its order.
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

/// The place in [`ScenePhase::Objects`] the assets tick at: the build lands
/// before the objects that need the instances it hands out.
pub const TICK_SCENE: i32 = 0;

/// The place the shapes tick at: after the assets exist, before the music.
pub const TICK_BALLS: i32 = 100;

/// The place the music ticks at: last, once the shapes are out.
pub const TICK_AUDIO: i32 = 200;

/// The behaviour that ticks `T` for the entity it is spawned on, at `order`.
///
/// The tick hands the object the scene, so an object that reaches another
/// world names it instead of keeping a channel of its own. A tick is a frame,
/// so an object's tick is an [`OnFrame`] like any other: the object declares
/// where it runs, and the scene never sorts its objects itself.
pub fn tick_behaviour<T: Tick + 'static>(scene: Scene, order: FrameBehaviourOrder) -> OnFrame {
    OnFrame::new(order, move |world, entity, frame| {
        let _ = world
            .root_scope()
            .query_one::<&mut T>(entity)
            .map(|mut state| state.tick(world, frame, &scene));
    })
}
