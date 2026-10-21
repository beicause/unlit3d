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
//! structural change goes through [`unlit_ecs::Scope::queue`] and lands when the driver
//! applies after the dispatch. A behaviour that needs the rest of the frame
//! skipped sets [FrameSkip], which its siblings read; [dispatch_frame] clears it
//! before the first behaviour runs, so every frame starts whole.

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

/// How much time has passed, as the frame loop counts it.
///
/// [`spawn_context`](crate::source::spawn_context) puts one in the world's
/// shared subtree, so every source reads the same clock, and
/// [`dispatch_frame`] advances it once per frame, before any behaviour runs.
/// A source that draws with time — an animation, a shader's clock — reads this
/// rather than counting its own builds: a source may be built once per pass,
/// and time must not run faster because a frame has more passes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameClock {
    /// The seconds since the first frame.
    pub time: f32,
    /// The seconds the current frame took.
    pub delta_time: f32,
    /// The current frame's index, counting from zero.
    pub index: u32,
}

impl FrameClock {
    /// Move the clock on to `frame`.
    ///
    /// The clock follows the frame loop's own count rather than counting
    /// builds: `index` and `delta_time` are what the loop states, and `time`
    /// is the sum of the deltas it has stated.
    pub fn advance(&mut self, delta_time: f32, index: u32) {
        self.time += delta_time;
        self.delta_time = delta_time;
        self.index = index;
    }
}

/// When a frame behaviour runs, relative to the others.
///
/// Lower runs first. Deliberately has no `Default`: a behaviour states its
/// ordering intent, and a silent default is what an explicit order exists to
/// remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameBehaviourOrder(pub i32);

/// A callback that runs once for the entity it is spawned on.
pub type FrameCallback = Box<dyn FnMut(&World, Entity, &mut Frame)>;

/// Whether this frame has been short-circuited.
///
/// The frame's behaviours are independent components with no shared stack, so
/// one that cannot carry the frame further sets this and its later siblings
/// read it and return. The order they run in is what makes the reads
/// meaningful, which is why a frame behaviour declares one.
///
/// Spawn one to take part; a world without one has nothing to skip.
#[derive(Debug, Default)]
pub struct FrameSkip(pub bool);

impl FrameSkip {
    /// Mark the frame as skipped.
    ///
    /// A no-op in a world that never spawned a [FrameSkip].
    pub fn skip(world: &World) {
        for entity in Self::entities(world) {
            let _ = world
                .root_scope()
                .query_one::<&mut FrameSkip>(entity)
                .map(|mut skip| skip.0 = true);
        }
    }

    /// Whether the frame has been skipped.
    #[must_use]
    pub fn is_skipped(world: &World) -> bool {
        world
            .root_scope()
            .query::<&FrameSkip>()
            .any(|frame_skip| frame_skip.0)
    }

    /// Clear the flag, readying the frame for its behaviours.
    fn reset(world: &World) {
        for entity in Self::entities(world) {
            let _ = world
                .root_scope()
                .query_one::<&mut FrameSkip>(entity)
                .map(|mut skip| skip.0 = false);
        }
    }

    /// The entities carrying a [FrameSkip], collected before any is borrowed
    /// mutably.
    fn entities(world: &World) -> Vec<Entity> {
        world
            .root_scope()
            .query::<(&FrameSkip, Entity)>()
            .map(|(_frame_skip, entity)| entity)
            .collect()
    }
}

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
}

/// The state every frame behaviour family shares: its callback and its order.
pub(crate) struct BehaviourSlot {
    callback: FrameCallback,
    order: FrameBehaviourOrder,
}

impl BehaviourSlot {
    fn new(order: FrameBehaviourOrder, callback: FrameCallback) -> Self {
        Self { callback, order }
    }

    fn order(&self) -> FrameBehaviourOrder {
        self.order
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
        }
    };
}

frame_behaviour!(
    OnFrame,
    "Runs once per frame of a windowed application.\n\nDispatch it with [`dispatch_frame`]."
);

/// Run every [`OnFrame`] in `world`, in resolved order, then apply what they
/// queued.
///
/// Any [FrameSkip] is cleared first, so a frame that was short-circuited does
/// not stay short-circuited for the next one. Every [`FrameClock`] then moves
/// on to this frame, before the first behaviour reads it.
pub fn dispatch_frame(world: &mut World, frame: &mut Frame) {
    FrameSkip::reset(world);
    world
        .root_scope()
        .query::<&mut FrameClock>()
        .for_each(|mut clock| clock.advance(frame.delta_time, frame.index));
    dispatch::<OnFrame>(world, frame);
    world.apply();
}

/// Run every `B` in `world`, in the order their components declare.
///
/// Sorted by `(order, entity)`, so the entity breaks a tie between equal
/// orders. A callback must not change the frame's behaviour set: the list is
/// resolved before the first one runs, and a change landing inside the dispatch
/// would leave it stale. Queue structural work instead; the driver applies it
/// after the dispatch.
fn dispatch<B: FrameBehaviour>(world: &World, frame: &mut Frame) {
    let mut order: Vec<(FrameBehaviourOrder, Entity)> = world
        .root_scope()
        .query::<(&B, Entity)>()
        .map(|(b, entity)| (b.order(), entity))
        .collect();
    order.sort_unstable_by_key(|(order, entity)| (*order, *entity));
    for (_, entity) in order {
        world
            .root_scope()
            .query_one::<&mut B>(entity)
            .map(|mut behaviour| behaviour.run(world, entity, frame))
            .expect("the behaviour entity exists");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;
    use unlit_ecs::bundle;

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
        world.root_scope_mut().spawn(bundle![later]);
        world.root_scope_mut().spawn(bundle![earlier]);
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2]);
    }

    #[test]
    fn equal_orders_keep_their_spawn_order() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        for value in [1, 2, 3] {
            let log = log.clone();
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                UPDATE,
                move |_world, _entity, _frame| {
                    log.borrow_mut().push(value);
                }
            ),]);
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
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                UPDATE,
                move |_world, _entity, frame| {
                    *seen.borrow_mut() = Some(*frame);
                }
            ),]);
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
        world
            .root_scope_mut()
            .spawn(bundle![OnFrame::new(UPDATE, |_world, _entity, frame| {
                frame.index += 1;
            }),]);
        let mut frame = frame();
        dispatch_frame(&mut world, &mut frame);
        assert_eq!(frame.index, 1);
    }

    #[test]
    fn dispatch_applies_what_a_behaviour_queued() {
        struct Marker;
        let mut world = World::new();
        world
            .root_scope_mut()
            .spawn(bundle![OnFrame::new(UPDATE, |world, _entity, _frame| {
                let _ = world.root_scope().queue().spawn(bundle![Marker]);
            }),]);
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(world.root_scope().query::<&Marker>().count(), 1);
    }

    /// A later order runs after an earlier one, however the two were spawned.
    #[test]
    fn a_later_order_runs_after_an_earlier_one() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let log = log.clone();
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                END,
                move |_world, _entity, _frame| {
                    log.borrow_mut().push("end");
                }
            ),]);
        }
        {
            let log = log.clone();
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                UPDATE,
                move |_world, _entity, _frame| {
                    log.borrow_mut().push("update");
                }
            ),]);
        }
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec!["update", "end"]);
    }

    #[test]
    fn skip_marks_the_frame_and_is_skipped_reads_it() {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![FrameSkip::default()]);
        assert!(!FrameSkip::is_skipped(&world));
        FrameSkip::skip(&world);
        assert!(FrameSkip::is_skipped(&world));
    }

    #[test]
    fn skip_is_a_no_op_without_a_flag_component() {
        let world = World::new();
        FrameSkip::skip(&world);
        assert!(!FrameSkip::is_skipped(&world));
    }

    /// A frame that short-circuited does not leave the next one short-circuited:
    /// the dispatch clears the flag before the first behaviour runs.
    #[test]
    fn dispatch_clears_a_skip_left_by_the_previous_frame() {
        let mut world = World::new();
        let first = Rc::new(Cell::new(true));
        world.root_scope_mut().spawn(bundle![FrameSkip::default()]);
        {
            let first = first.clone();
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                UPDATE,
                move |world, _entity, _frame| {
                    assert!(!FrameSkip::is_skipped(world), "the flag starts whole");
                    if first.replace(false) {
                        FrameSkip::skip(world);
                    }
                }
            ),]);
        }
        dispatch_frame(&mut world, &mut frame());
        assert!(FrameSkip::is_skipped(&world));
        dispatch_frame(&mut world, &mut frame());
        assert!(!FrameSkip::is_skipped(&world));
    }

    /// The clock follows the frame the loop states, so a frame that takes
    /// longer moves the clock further, and a frame index the loop skips over is
    /// not counted twice.
    #[test]
    fn the_clock_follows_the_frame_it_is_dispatched_with() {
        let mut world = World::new();
        let clock = world.root_scope_mut().spawn(bundle![FrameClock::default()]);
        let seen: Rc<RefCell<Vec<FrameClock>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let seen = seen.clone();
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                UPDATE,
                move |world, _entity, _frame| {
                    let clock = *world
                        .root_scope()
                        .query_one::<&FrameClock>(clock)
                        .expect("the clock is spawned");
                    seen.borrow_mut().push(clock);
                }
            ),]);
        }

        let mut frame = Frame {
            delta_time: 0.25,
            index: 3,
            size: (0, 0),
        };
        dispatch_frame(&mut world, &mut frame);
        frame.delta_time = 0.5;
        frame.index = 4;
        dispatch_frame(&mut world, &mut frame);

        let seen = seen.borrow();
        assert_eq!(
            seen[0],
            FrameClock {
                time: 0.25,
                delta_time: 0.25,
                index: 3,
            },
            "the first frame's clock starts from zero"
        );
        assert_eq!(
            seen[1],
            FrameClock {
                time: 0.75,
                delta_time: 0.5,
                index: 4,
            },
            "time accumulates the deltas the loop states"
        );
    }

    /// A world with no clock still dispatches: the clock is the application's to
    /// spawn, and nothing in the dispatch requires one.
    #[test]
    fn dispatch_without_a_clock_still_runs_every_behaviour() {
        let mut world = World::new();
        let ran = Rc::new(Cell::new(0));
        {
            let ran = ran.clone();
            world.root_scope_mut().spawn(bundle![OnFrame::new(
                UPDATE,
                move |_world, _entity, _frame| {
                    ran.set(ran.get() + 1);
                }
            ),]);
        }
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(ran.get(), 1);
    }
}
