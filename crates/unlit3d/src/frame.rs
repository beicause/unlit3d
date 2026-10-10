//! Frame behaviours: what a frame is made of, as components.
//!
//! The other behaviour families react to an event. A frame behaviour *is* the
//! frame: it runs once per dispatch, in an order its components declare, so a
//! windowed application's frame loop is one dispatch plus whatever the
//! behaviours queued. Extending the frame is spawning another behaviour, not
//! editing a function.
//!
//! One family: [`OnFrame`] is the frame itself, dispatched by whatever owns
//! the world — a windowed application's host world, or a `Scene` advancing the
//! world it drives. The order behaviours run in is the application's to define:
//! unlit3d declares no order values of its own, so a frame is extended by
//! spawning behaviours, never by editing a list.
//!
//! Like every other behaviour, a frame behaviour runs with `&World`, so a
//! structural change goes through [`World::queue`] and lands when the driver
//! applies after the dispatch. A behaviour that needs the rest of the frame
//! skipped writes a flag its siblings read; nothing is skipped for it.

pub(crate) use unlit_ecs::{Entity, World};

/// One frame's context: what changes from frame to frame and is not world
/// state.
///
/// A frame behaviour receives it as its event, so it can tell how long the
/// frame was, which one it is, and what size the content is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The seconds since the previous frame.
    pub delta_time: f32,
    /// The frame's index, counting from zero.
    pub index: u32,
    /// The size the content is drawn at, in pixels.
    pub size: (u32, u32),
}

/// When a frame behaviour runs, relative to the others.
///
/// Lower runs first. Deliberately has no `Default`: a behaviour states its
/// ordering intent, and a silent default is what an explicit order exists to
/// remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameBehaviourOrder(pub i32);

/// A callback that runs once for the entity it is mounted on.
pub type FrameCallback = Box<dyn FnMut(&World, Entity, &mut Frame)>;

/// A component that runs once per frame.
///
/// Implemented by the `frame_behaviour!` macro for each family; a caller adds a
/// behaviour by spawning one of the family's components, not by implementing
/// this itself.
pub trait FrameBehaviour: 'static {
    /// The order this behaviour runs in.
    fn order(&self) -> FrameBehaviourOrder;

    /// Run this behaviour for `entity` with `frame`.
    fn run(&mut self, world: &World, entity: Entity, frame: &mut Frame);

    /// When this behaviour was mounted, which breaks ties between equal
    /// orders.
    fn mount_index(&self) -> u64;

    /// Record when this behaviour was mounted.
    ///
    /// Written by [`WorldFrameExt::spawn_frame_behaviour`]; a behaviour spawned
    /// without that helper keeps the default of zero.
    fn set_mount_index(&mut self, index: u64);
}

/// The state every frame behaviour family shares: its callback, its order and
/// when it was mounted.
pub(crate) struct BehaviourSlot {
    callback: FrameCallback,
    order: FrameBehaviourOrder,
    mount_index: u64,
}

impl BehaviourSlot {
    fn new(order: FrameBehaviourOrder, callback: FrameCallback) -> Self {
        Self {
            callback,
            order,
            mount_index: 0,
        }
    }

    fn order(&self) -> FrameBehaviourOrder {
        self.order
    }

    fn mount_index(&self) -> u64 {
        self.mount_index
    }

    fn set_mount_index(&mut self, index: u64) {
        self.mount_index = index;
    }

    fn run(&mut self, world: &World, entity: Entity, frame: &mut Frame) {
        (self.callback)(world, entity, frame);
    }
}

/// Declares one frame behaviour family.
macro_rules! frame_behaviour {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        pub struct $name($crate::frame::BehaviourSlot);

        impl $name {
            #[doc = concat!("Wrap `f` as an [`", stringify!($name), "`].")]
            ///
            /// `order` places it among the world's other frame behaviours:
            /// unlit3d supplies no order values, so the caller's are the only
            /// ones that exist.
            pub fn new(
                order: $crate::frame::FrameBehaviourOrder,
                f: impl FnMut(
                    &$crate::frame::World,
                    $crate::frame::Entity,
                    &mut $crate::frame::Frame,
                ) + 'static,
            ) -> Self {
                Self($crate::frame::BehaviourSlot::new(order, Box::new(f)))
            }

            /// The order this behaviour runs in.
            pub fn order(&self) -> $crate::frame::FrameBehaviourOrder {
                self.0.order()
            }

            /// Run this behaviour for `entity` with `frame`.
            pub fn run(
                &mut self,
                world: &$crate::frame::World,
                entity: $crate::frame::Entity,
                frame: &mut $crate::frame::Frame,
            ) {
                self.0.run(world, entity, frame);
            }
        }

        impl $crate::frame::FrameBehaviour for $name {
            fn order(&self) -> $crate::frame::FrameBehaviourOrder {
                self.0.order()
            }

            fn run(
                &mut self,
                world: &$crate::frame::World,
                entity: $crate::frame::Entity,
                frame: &mut $crate::frame::Frame,
            ) {
                self.0.run(world, entity, frame);
            }

            fn mount_index(&self) -> u64 {
                self.0.mount_index()
            }

            fn set_mount_index(&mut self, index: u64) {
                self.0.set_mount_index(index);
            }
        }
    };
}

frame_behaviour!(
    OnFrame,
    "Runs once per frame of a windowed application.\n\nDispatch it with [`dispatch_frame`]."
);

/// The next mount index to hand out.
///
/// Kept in the world so it is monotonic across the world's life rather than per
/// driver: two drivers sharing a world must not hand out the same indices, or
/// the tie-break would not be total.
#[derive(Debug, Default)]
struct FrameMounts(u64);

/// Spawning a frame behaviour, which records the order it was mounted in.
pub trait WorldFrameExt {
    /// Spawn `behaviour`, recording it as the next one mounted.
    fn spawn_frame_behaviour<B: FrameBehaviour>(&mut self, behaviour: B) -> Entity;
}

impl WorldFrameExt for World {
    fn spawn_frame_behaviour<B: FrameBehaviour>(&mut self, mut behaviour: B) -> Entity {
        let index = next_mount_index(self);
        behaviour.set_mount_index(index);
        self.spawn((behaviour,))
    }
}

/// Take the next mount index from the world's counter, spawning it if absent.
fn next_mount_index(world: &mut World) -> u64 {
    let counter = world
        .query::<&FrameMounts>()
        .next()
        .map(|(entity, _)| entity);
    let Some(counter) = counter else {
        world.spawn((FrameMounts(1),));
        return 0;
    };
    world
        .with_mut::<FrameMounts, _>(counter, |mounts| {
            let index = mounts.0;
            mounts.0 += 1;
            index
        })
        .expect("the mount counter exists")
}

/// Run every [`OnFrame`] in `world`, in resolved order, then apply what they
/// queued.
pub fn dispatch_frame(world: &mut World, frame: &mut Frame) {
    dispatch::<OnFrame>(world, frame);
    world.apply();
}

/// Run every `B` in `world`, in the order their components declare.
///
/// Sorted by `(order, mount_index)`, so equal orders keep their mount order. A
/// callback must not change the frame's behaviour set: the list is resolved
/// before the first one runs, and a change landing inside the dispatch would
/// leave it stale. Queue structural work instead; the driver applies it after
/// the dispatch.
fn dispatch<B: FrameBehaviour>(world: &World, frame: &mut Frame) {
    let mut order: Vec<(FrameBehaviourOrder, u64, Entity)> = world
        .query::<&B>()
        .map(|(entity, behaviour)| (behaviour.order(), behaviour.mount_index(), entity))
        .collect();
    order.sort_unstable_by_key(|(order, mount_index, _)| (*order, *mount_index));
    for (_, _, entity) in order {
        world
            .with_mut::<B, _>(entity, |behaviour| behaviour.run(world, entity, frame))
            .expect("the behaviour entity exists");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn frame() -> Frame {
        Frame {
            delta_time: 0.0,
            index: 0,
            size: (0, 0),
        }
    }

    /// The order the tests place their behaviours at; unlit3d defines none.
    const UPDATE: FrameBehaviourOrder = FrameBehaviourOrder(0);
    /// A later order, for the tests that need one after `UPDATE`.
    const END: FrameBehaviourOrder = FrameBehaviourOrder(100);

    #[test]
    fn behaviours_run_in_their_declared_order() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let later = {
            let log = log.clone();
            OnFrame::new(FrameBehaviourOrder(10), move |_world, _entity, _frame| {
                log.borrow_mut().push(2)
            })
        };
        let earlier = {
            let log = log.clone();
            OnFrame::new(FrameBehaviourOrder(5), move |_world, _entity, _frame| {
                log.borrow_mut().push(1)
            })
        };
        world.spawn_frame_behaviour(later);
        world.spawn_frame_behaviour(earlier);
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2]);
    }

    #[test]
    fn equal_orders_keep_their_mount_order() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        for value in [1, 2, 3] {
            let log = log.clone();
            world.spawn_frame_behaviour(OnFrame::new(UPDATE, move |_world, _entity, _frame| {
                log.borrow_mut().push(value);
            }));
        }
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2, 3]);
    }

    #[test]
    fn a_behaviour_receives_the_frame() {
        let mut world = World::new();
        let seen: Rc<RefCell<Option<Frame>>> = Rc::new(RefCell::new(None));
        {
            let seen = seen.clone();
            world.spawn_frame_behaviour(OnFrame::new(UPDATE, move |_world, _entity, frame| {
                *seen.borrow_mut() = Some(*frame);
            }));
        }
        let mut frame = Frame {
            delta_time: 0.5,
            index: 7,
            size: (320, 240),
        };
        dispatch_frame(&mut world, &mut frame);
        assert_eq!(
            *seen.borrow(),
            Some(Frame {
                delta_time: 0.5,
                index: 7,
                size: (320, 240)
            })
        );
    }

    #[test]
    fn a_frame_behaviour_may_advance_the_index() {
        let mut world = World::new();
        world.spawn_frame_behaviour(OnFrame::new(UPDATE, |_world, _entity, frame| {
            frame.index += 1;
        }));
        let mut frame = frame();
        dispatch_frame(&mut world, &mut frame);
        assert_eq!(frame.index, 1);
    }

    #[test]
    fn dispatch_applies_what_a_behaviour_queued() {
        struct Marker;
        let mut world = World::new();
        world.spawn_frame_behaviour(OnFrame::new(UPDATE, |world, _entity, _frame| {
            let _ = world.queue().spawn((Marker,));
        }));
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(world.query::<&Marker>().count(), 1);
    }

    /// A later order runs after an earlier one, however the two were mounted.
    #[test]
    fn a_later_order_runs_after_an_earlier_one() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let log = log.clone();
            world.spawn_frame_behaviour(OnFrame::new(END, move |_world, _entity, _frame| {
                log.borrow_mut().push("end");
            }));
        }
        {
            let log = log.clone();
            world.spawn_frame_behaviour(OnFrame::new(UPDATE, move |_world, _entity, _frame| {
                log.borrow_mut().push("update");
            }));
        }
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec!["update", "end"]);
    }
}
