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
//! structural change goes through [`unlit_ecs::Scope::queue`]. The dispatch
//! runs one callback at a time and applies what it queued before the next one
//! runs, so a later callback in the same frame sees the change. A behaviour
//! that needs the rest of the frame skipped sets [FrameSkip], which its
//! siblings read; [dispatch_frame] clears it before the first behaviour runs,
//! so every frame starts whole.
//!
//! A component holds a list of callbacks, each with an order of its own, so one
//! entity may carry several frame behaviours and place them independently.
//! They run by order, then by entity, then by their position in the list.

pub(crate) use unlit_ecs::{CallbackList, Entity, World};

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
                .with_mut::<FrameSkip, _>(entity, |skip| skip.0 = true);
        }
    }

    /// Whether the frame has been skipped.
    #[must_use]
    pub fn is_skipped(world: &World) -> bool {
        world
            .root_scope()
            .query::<&FrameSkip>()
            .any(|(_, skip)| skip.0)
    }

    /// Clear the flag, readying the frame for its behaviours.
    fn reset(world: &World) {
        for entity in Self::entities(world) {
            let _ = world
                .root_scope()
                .with_mut::<FrameSkip, _>(entity, |skip| skip.0 = false);
        }
    }

    /// The entities carrying a [FrameSkip], collected before any is borrowed
    /// mutably.
    fn entities(world: &World) -> Vec<Entity> {
        world
            .root_scope()
            .query::<&FrameSkip>()
            .map(|(entity, _)| entity)
            .collect()
    }
}

/// Declares one frame behaviour family.
macro_rules! frame_behaviour {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        pub struct $name {
            /// Every callback, each with the order it runs in.
            pub callbacks: Vec<($crate::frame::FrameBehaviourOrder, $crate::frame::FrameCallback)>,
        }

        impl $name {
            #[doc = concat!(
                "Box `f` as one callback of an [`",
                stringify!($name),
                "`], running at `order`."
            )]
            ///
            /// [`new`](Self::new) takes callbacks in this form, which is what
            /// lets a closure be written without naming the family's frame
            /// type.
            pub fn callback(
                order: $crate::frame::FrameBehaviourOrder,
                f: impl FnMut(
                    &$crate::frame::World,
                    $crate::frame::Entity,
                    &mut $crate::frame::Frame,
                ) + 'static,
            ) -> ($crate::frame::FrameBehaviourOrder, $crate::frame::FrameCallback) {
                (order, Box::new(f))
            }

            #[doc = concat!(
                "Wrap the callbacks `cbs` as an [`",
                stringify!($name),
                "`]."
            )]
            ///
            /// Each carries its own order, which places it among the world's
            /// other frame behaviours: unlit3d supplies no order values, so the
            /// caller's are the only ones that exist. An empty iterator
            /// declares a component that runs nothing yet.
            pub fn new(
                cbs: impl IntoIterator<
                    Item = ($crate::frame::FrameBehaviourOrder, $crate::frame::FrameCallback),
                >,
            ) -> Self {
                Self {
                    callbacks: cbs.into_iter().collect(),
                }
            }

            #[doc = concat!(
                "Add `f` to this [`",
                stringify!($name),
                "`] at run time, after the callbacks already in it."
            )]
            pub fn push(
                &mut self,
                order: $crate::frame::FrameBehaviourOrder,
                f: impl FnMut(
                    &$crate::frame::World,
                    $crate::frame::Entity,
                    &mut $crate::frame::Frame,
                ) + 'static,
            ) {
                self.callbacks.push((order, Box::new(f)));
            }
        }

        impl $crate::frame::CallbackList for $name {
            type Callback = ($crate::frame::FrameBehaviourOrder, $crate::frame::FrameCallback);

            fn callbacks(&self) -> &[Self::Callback] {
                &self.callbacks
            }

            fn callbacks_mut(&mut self) -> &mut Vec<Self::Callback> {
                &mut self.callbacks
            }
        }
    };
}

frame_behaviour!(
    OnFrame,
    "Runs once per frame of a windowed application.\n\nDispatch it with [`dispatch_frame`]."
);

/// Run every [`OnFrame`] in `world`, in resolved order.
///
/// Any [FrameSkip] is cleared first, so a frame that was short-circuited does
/// not stay short-circuited for the next one. What a callback queued is applied
/// before the next callback runs, so the frame it is handed is all a callback
/// shares with its siblings.
pub fn dispatch_frame(world: &mut World, frame: &mut Frame) {
    FrameSkip::reset(world);
    let mut schedule: Vec<(FrameBehaviourOrder, Entity, usize)> = Vec::new();
    for (entity, behaviour) in world.root_scope().query::<&OnFrame>() {
        for (index, (order, _)) in behaviour.callbacks.iter().enumerate() {
            schedule.push((*order, entity, index));
        }
    }
    schedule.sort_unstable();
    for (_, entity, index) in schedule {
        world.run_callback::<OnFrame, _>(entity, index, &mut |world, callback| {
            (callback.1)(world, entity, frame);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
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
            OnFrame::new([OnFrame::callback(
                FrameBehaviourOrder(10),
                move |_world, _entity, _frame| log.borrow_mut().push(2),
            )])
        };
        let earlier = {
            let log = log.clone();
            OnFrame::new([OnFrame::callback(
                FrameBehaviourOrder(5),
                move |_world, _entity, _frame| log.borrow_mut().push(1),
            )])
        };
        world.root_scope_mut().spawn((later,));
        world.root_scope_mut().spawn((earlier,));
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2]);
    }

    #[test]
    fn equal_orders_keep_their_spawn_order() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        for value in [1, 2, 3] {
            let log = log.clone();
            world
                .root_scope_mut()
                .spawn((OnFrame::new([OnFrame::callback(
                    UPDATE,
                    move |_world, _entity, _frame| {
                        log.borrow_mut().push(value);
                    },
                )]),));
        }
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2, 3]);
    }

    /// One entity may carry several callbacks, each with an order of its own.
    #[test]
    fn one_entity_runs_every_callback_of_its_component() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let first = log.clone();
        let second = log.clone();
        let third = log.clone();
        let component = OnFrame::new([
            OnFrame::callback(UPDATE, move |_world, _entity, _frame| {
                first.borrow_mut().push(1);
            }),
            OnFrame::callback(END, move |_world, _entity, _frame| {
                second.borrow_mut().push(3);
            }),
            OnFrame::callback(FrameBehaviourOrder(50), move |_world, _entity, _frame| {
                third.borrow_mut().push(2);
            }),
        ]);
        world.root_scope_mut().spawn((component,));
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2, 3]);
    }

    /// A callback added at run time joins the schedule at its own order.
    #[test]
    fn a_pushed_callback_runs_at_its_order() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let first = log.clone();
        let second = log.clone();
        let mut component =
            OnFrame::new([OnFrame::callback(END, move |_world, _entity, _frame| {
                first.borrow_mut().push(2);
            })]);
        component.push(FrameBehaviourOrder(0), move |_world, _entity, _frame| {
            second.borrow_mut().push(1);
        });
        world.root_scope_mut().spawn((component,));
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec![1, 2]);
    }

    #[test]
    fn a_behaviour_receives_the_frame() {
        let mut world = World::new();
        let seen: Rc<RefCell<Option<Frame>>> = Rc::new(RefCell::new(None));
        {
            let seen = seen.clone();
            world
                .root_scope_mut()
                .spawn((OnFrame::new([OnFrame::callback(
                    UPDATE,
                    move |_world, _entity, frame| {
                        *seen.borrow_mut() = Some(*frame);
                    },
                )]),));
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
            .spawn((OnFrame::new([OnFrame::callback(
                UPDATE,
                |_world, _entity, frame| {
                    frame.index += 1;
                },
            )]),));
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
            .spawn((OnFrame::new([OnFrame::callback(
                UPDATE,
                |world, _entity, _frame| {
                    let _ = world.root_scope().queue().spawn((Marker,));
                },
            )]),));
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(world.root_scope().query::<&Marker>().count(), 1);
    }

    /// The queue is applied after each callback, not once at the end, so a
    /// behaviour that runs later in the same frame sees what an earlier one
    /// spawned or despawned.
    #[test]
    fn a_later_behaviour_sees_what_an_earlier_one_queued() {
        struct Marker;
        let mut world = World::new();
        world.root_scope_mut().spawn((OnFrame::new([
            OnFrame::callback(UPDATE, |world, _entity, _frame| {
                let _ = world.root_scope().queue().spawn((Marker,));
            }),
            OnFrame::callback(UPDATE, |world, _entity, _frame| {
                assert_eq!(
                    world.root_scope().query::<&Marker>().count(),
                    1,
                    "the earlier callback's spawn has landed"
                );
            }),
        ]),));
        dispatch_frame(&mut world, &mut frame());
    }

    /// A later order runs after an earlier one, however the two were spawned.
    #[test]
    fn a_later_order_runs_after_an_earlier_one() {
        let mut world = World::new();
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let log = log.clone();
            world
                .root_scope_mut()
                .spawn((OnFrame::new([OnFrame::callback(
                    END,
                    move |_world, _entity, _frame| {
                        log.borrow_mut().push("end");
                    },
                )]),));
        }
        {
            let log = log.clone();
            world
                .root_scope_mut()
                .spawn((OnFrame::new([OnFrame::callback(
                    UPDATE,
                    move |_world, _entity, _frame| {
                        log.borrow_mut().push("update");
                    },
                )]),));
        }
        dispatch_frame(&mut world, &mut frame());
        assert_eq!(*log.borrow(), vec!["update", "end"]);
    }

    #[test]
    fn skip_marks_the_frame_and_is_skipped_reads_it() {
        let mut world = World::new();
        world.root_scope_mut().spawn((FrameSkip::default(),));
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
        world.root_scope_mut().spawn((FrameSkip::default(),));
        {
            let first = first.clone();
            world
                .root_scope_mut()
                .spawn((OnFrame::new([OnFrame::callback(
                    UPDATE,
                    move |world, _entity, _frame| {
                        assert!(!FrameSkip::is_skipped(world), "the flag starts whole");
                        if first.replace(false) {
                            FrameSkip::skip(world);
                        }
                    },
                )]),));
        }
        dispatch_frame(&mut world, &mut frame());
        assert!(FrameSkip::is_skipped(&world));
        dispatch_frame(&mut world, &mut frame());
        assert!(!FrameSkip::is_skipped(&world));
    }
}
