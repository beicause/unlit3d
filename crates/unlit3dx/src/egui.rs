//! egui panels laid out in one world and drawn in another.
//!
//! A `unlit3d::egui::EguiPanel` can only be built where its `egui::Context` runs: the
//! callback is handed a `&mut egui::Ui`, which belongs to the pass building it,
//! so it cannot be carried to another thread. An overlay whose panels belong to one
//! world and whose drawing belongs to another therefore crosses the boundary as
//! **data**: the panel's world lays the frame out and tessellates it, and the
//! drawing world uploads and records what comes back.
//!
//! The handover is one way: the panel world sends a job carrying an
//! [`unlit3d::egui::ExternalFrame`] to the world holding the
//! [`unlit3d::egui::EguiSource`], which keeps it in a slot and draws it. A settled
//! panel sends nothing, so the frame already there keeps being drawn. What comes
//! back is the target the drawing side draws at, published from the source into
//! a [`Snapshot`](unlit3d::relay::Snapshot) the panel world subscribes to, so a
//! frame is laid out at the size and density it will be drawn at.
//!
//! No frame is laid out before the drawing side has drawn once: until then
//! there is no target, and a frame laid out at a guessed size would be dropped
//! by the source rather than drawn.
//!
//! The two sides hold **separate** contexts. Sharing one would have both worlds
//! writing egui's memory and its font atlas, and egui treats one `run_ui` call
//! as one whole frame, so two worlds running it against one context would
//! interleave two frames into one. Their texture ids both start at zero, which
//! the drawing side handles by keying textures by origin and id: nothing
//! renumbers them.

use core::time::Duration;
use std::sync::{Arc, Mutex};

use crate::job::{Job, JobSender, SendErrorKind};
use unlit_ecs::World;
use unlit3d::egui::{EguiSource, EguiTarget, ExternalFrame, egui, lay_out, raw_input};
use unlit3d::frame::Frame;
use unlit3d::input::{InputEvent, InputEvents};
use unlit3d::relay::SnapshotReader;
use unlit3d::source::Source;

/// A world's panel publishing: the context its panels are built in, where its
/// frames are sent and the target it lays out against.
///
/// Spawn it in the world holding the panels. Nothing drives this from outside:
/// the world ticks it, the way it ticks any other behaviour, and each tick that
/// produced a frame sends it to the drawing world. A tick that could not change
/// the picture — a settled panel, no input, the same target — sends nothing, so
/// an idle egui costs nothing.
///
/// The context, its font atlas and its memory belong to this world and stay
/// there: what crosses to the drawing side is the laid-out frame, which carries
/// no borrow of any of them.
pub struct EguiPanelHost {
    /// The context the panels in this world are built in.
    ctx: egui::Context,
    /// The frame time this host has been driven through, in seconds.
    ///
    /// egui's animations are paced by it, so a run that steps the same deltas
    /// draws the same picture however long those steps took: an animation is
    /// over after a number of frames rather than after an amount of wall
    /// clock, which is what lets a snapshot of a panel be compared.
    time: f64,
    /// The queue of the world that draws the frames.
    render: JobSender,
    /// The target the drawing side last drew at.
    screen: SnapshotReader<EguiTarget>,
    /// Which slot this world's frames go in.
    slot: usize,
    /// The events of the frame being laid out, reclaimed from egui once the
    /// pass is over so the next frame reuses the allocation.
    event_scratch: Vec<egui::Event>,
    /// The world's events for the frame being laid out, copied out of its
    /// buffer before anything is laid out so the allocation is reused.
    input_scratch: Vec<InputEvent>,
    /// The target the last frame was laid out at, or `None` before there is
    /// one.
    ///
    /// The first tick always lays a frame out, and a tick whose target
    /// differs lays one out again however settled the panel is: the drawing
    /// side drops a frame laid out at another size, and this is what replaces
    /// it. A frame the drawing world's queue refused leaves this behind, so
    /// the next tick sends it again.
    laid_out_at: Option<EguiTarget>,
    /// The moment, in this host's own [time](Self::time), at which the last
    /// frame laid out asked to be laid out again: a frame that named a moment
    /// rather than asking for the next possible pass.
    repaint_at: Option<f64>,
    /// A frame the drawing world's queue refused, kept so its texture deltas
    /// are not lost: the queue holds a bounded number of frames, and a frame
    /// dropped here would take the texture changes it carries — a font atlas
    /// upload, usually — with it, leaving the drawing side to draw the panel
    /// against textures it never received.
    ///
    /// The frame lives behind an [`Arc`] the job takes it out of, so a frame
    /// whose job was refused is still there to be sent again: the job never
    /// ran, so nothing took it, and holding the frame rather than the job is
    /// what lets it be retried without copying it.
    refused: Option<Arc<Mutex<Option<ExternalFrame>>>>,
}

impl EguiPanelHost {
    /// Publish `world`'s panels into slot `slot` of the world behind
    /// `render`, against the target `screen` carries.
    #[must_use]
    pub fn new(render: JobSender, screen: SnapshotReader<EguiTarget>, slot: usize) -> Self {
        let ctx = egui::Context::default();
        #[cfg(feature = "sc_fonts")]
        unlit3d::fonts::set_egui_fonts(&ctx);
        Self {
            ctx,
            time: 0.0,
            render,
            screen,
            slot,
            event_scratch: Vec::new(),
            input_scratch: Vec::new(),
            laid_out_at: None,
            repaint_at: None,
            refused: None,
        }
    }

    /// The context the panels in this world are built in, for setting its
    /// style or fonts up before the first frame is laid out.
    #[must_use]
    pub fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// Lay out this tick's frame and send it to the drawing world, if it could
    /// differ from the one that world holds.
    ///
    /// The frame is laid out here rather than where it is drawn because only
    /// this world holds the panels and their context. A tick lays one out when
    /// something could have changed the picture: the drawing side has not been
    /// seen yet, the target moved, input arrived, or the last frame asked to be
    /// laid out again — either at once or once a delay it named has elapsed.
    /// Anything else draws the same picture, so nothing is laid out and
    /// nothing is sent.
    ///
    /// A frame the drawing world's queue refused is sent again on the next
    /// tick. A frame whose queue is closed has nowhere to go and is forgotten:
    /// the drawing world is gone.
    ///
    /// The events are this world's own, read from its [`InputEvents`]: a panel
    /// sees exactly what the rest of the world sees on the same tick.
    ///
    /// `frame` is the frame being stepped, and its duration is what moves this
    /// host's own clock on. egui paces its animations by that clock, so a panel
    /// animated across a run of frames looks the same at the same frame however
    /// long the run took: a capture compared against a stored picture does not
    /// depend on how busy the machine was. A frame that named a moment to be
    /// laid out again is due once the clock has reached it.
    pub fn publish(&mut self, world: &World, frame: &Frame) {
        // The host's clock is the world's, so it moves on whether or not there
        // is a target to lay out against: a frame stepped before the drawing
        // side first drew is time the panel has still lived through.
        self.time += frame.delta_time.as_secs();
        // The drawing side is the only party that knows the frame's target, so
        // there is nothing to lay out against until it has drawn once.
        let Some(target) = self.screen.latest().map(|target| *target) else {
            return;
        };
        // Copied out before anything is laid out: a panel may read its world's
        // input, and the copy is what keeps that read from taking the same lock
        // twice.
        let mut events = core::mem::take(&mut self.input_scratch);
        events.clear();
        let mut active_touches = 0;
        if let Some(mut row) = world
            .root_scope()
            .singleton::<InputEvents>()
            .and_then(|entity| {
                world
                    .root_scope()
                    .try_query_one::<&InputEvents>(entity)
                    .ok()
            })
        {
            let input_events = row.get();
            input_events.copy_events_into(&mut events);
            active_touches = input_events.touch_baseline();
        }
        // A frame the queue refused keeps its place: its texture deltas are
        // incremental state, so dropping it would strand a texture change the
        // drawing side never saw. Retrying it first delivers it ahead of
        // anything laid out this tick. A retry the queue still refuses does
        // not stop a tick that brought new events: the new frame is laid out
        // and the refused deltas ride along with it (see the send below).
        let mut carried: Option<ExternalFrame> = None;
        if let Some(held) = self.refused.take() {
            match self
                .render
                .send(panel_frame_job(self.slot, Arc::clone(&held)))
            {
                Ok(()) => {}
                // Still no room: the refused deltas fold into whatever this
                // tick lays out. Without new events nothing new is laid out,
                // so the frame simply keeps its place.
                Err(error) if error.kind() == SendErrorKind::Full && events.is_empty() => {
                    self.refused = Some(held);
                    self.input_scratch = events;
                    return;
                }
                Err(error) if error.kind() == SendErrorKind::Full => {
                    carried = unlit_ecs::sync::lock(&held).take();
                }
                // The drawing world is gone; the frame has nowhere to go.
                Err(_) => {}
            }
        }
        let due = self
            .repaint_at
            .is_some_and(|deadline| self.time >= deadline);
        if self.laid_out_at == Some(target) && !due && events.is_empty() {
            self.input_scratch = events;
            return;
        }
        let mut scratch = core::mem::take(&mut self.event_scratch);
        scratch.clear();
        // The events this world received are in its own types; egui wants its
        // own, translated at the target's density so a click lands where the
        // pointer is in logical points.
        unlit3d::egui::convert::to_egui_events_into(
            &events,
            target.screen.pixels_per_point,
            active_touches,
            &mut scratch,
        );
        self.input_scratch = events;
        // The clock this host has been driven through, which is the one egui
        // animates against. It is the frames' own time rather than the wall
        // clock: a panel stepped at a rate of its own animates at that rate,
        // and two runs of the same frames draw the same picture.
        let input = raw_input(target, Some(self.time), true, scratch);
        let laid_out = lay_out(world, &self.ctx, input, target.screen);
        // The pass is over, so egui no longer needs the list it was handed:
        // taking it back is what turns the next frame's fill into a reuse.
        self.event_scratch = self
            .ctx
            .input_mut(|input| core::mem::take(&mut input.raw.events));
        // A frame that asked for nothing named no moment, and the picture is
        // settled. One that asked for the next possible pass is answered on
        // the next tick, whenever that is; one that named a delay is answered
        // once this host's clock has reached it. Either way the request lives
        // only in the pass that made it, so keeping the moment is this side's
        // job.
        self.repaint_at = match laid_out.repaint_delay {
            delay if delay == Duration::MAX => None,
            Duration::ZERO => Some(self.time),
            // A delay the clock cannot place is one that never comes.
            delay => {
                let deadline = self.time + delay.as_secs_f64();
                deadline.is_finite().then_some(deadline)
            }
        };
        // A refused frame from an earlier tick rides here: its texture deltas
        // are incremental state the drawing side never applied, so they are
        // folded into this frame's — the older ones first, the order the
        // drawing side would have applied them in — while the geometry is the
        // new frame's alone, an older frame never having been drawn. The
        // fields are taken one by one: the frame drops its texture deltas, so
        // it cannot be partially moved or unwrapped.
        let mut laid_out = laid_out;
        if let Some(mut refused) = carried {
            let mut textures = core::mem::take(&mut refused.textures);
            textures.append(core::mem::take(&mut laid_out.textures));
            refused.textures = textures;
            refused.primitives = core::mem::take(&mut laid_out.primitives);
            // The refused frame's own requests were made first, and a request
            // only exists for the pass that made it: replacing the output
            // whole would drop them, so they lead the new frame's.
            let mut commands = core::mem::take(&mut refused.platform_output.commands);
            commands.append(&mut laid_out.platform_output.commands);
            refused.platform_output = core::mem::take(&mut laid_out.platform_output);
            refused.platform_output.commands = commands;
            refused.repaint_delay = laid_out.repaint_delay;
            refused.screen = laid_out.screen;
            laid_out = refused;
        }
        let held = Arc::new(Mutex::new(Some(laid_out)));
        match self
            .render
            .send(panel_frame_job(self.slot, Arc::clone(&held)))
        {
            Ok(()) => self.laid_out_at = Some(target),
            // The queue is full: nobody took the frame. Keeping it is what
            // keeps its texture deltas alive — a dropped frame would leave
            // the drawing side missing a texture change, such as a font
            // atlas upload. The refused frame is sent again on the next
            // tick, before anything new is laid out.
            Err(error) if error.kind() == SendErrorKind::Full => self.refused = Some(held),
            // The drawing world is gone; there is nothing left to draw.
            Err(_) => self.laid_out_at = Some(target),
        }
    }
}

/// Hand the frame `frame` holds to the egui source of the world this job runs
/// in.
///
/// The job takes the frame out of the cell as it runs, so a job the queue
/// refused leaves the frame where it was and the same cell can be sent again.
/// It then walks the world's draw sources and publishes into the first
/// [`EguiSource`] it finds, which draws that slot from then on. A world with no
/// source drops the frame, and dropping it clears its texture deltas.
pub fn panel_frame_job(slot: usize, frame: Arc<Mutex<Option<ExternalFrame>>>) -> Job {
    Box::new(move |world: &mut World| {
        let Some(frame) = unlit_ecs::sync::lock(&frame).take() else {
            return;
        };
        for source in world.root_scope().query::<&mut Source>().iter() {
            if let Ok(ui) = source.as_mut::<EguiSource>() {
                ui.frames.publish(slot, frame);
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use std::rc::Rc;
    use unlit_ecs::bundle;
    use unlit_wgpu::egui::ScreenDescriptor;
    use unlit3d::egui::EguiPanel;
    use unlit3d::input::{InputWriter, Modifiers, refresh_input};
    use unlit3d::relay::Snapshot;
    use unlit3d::source::SourceOrder;

    /// The worlds a panel is split across: the one holding it, and the one
    /// that draws what it sends.
    struct TestUi {
        /// The world that draws, holding the source frames are published to.
        render: World,
        /// The queue a sent frame waits in until the test runs it.
        jobs: async_channel::Receiver<Job>,
        /// The drawing world's end of that queue.
        sender: async_channel::Sender<Job>,
        host: EguiPanelHost,
        /// The target the drawing side publishes at.
        target: Snapshot<EguiTarget>,
    }

    impl TestUi {
        fn new() -> Self {
            let test = Self::without_target();
            test.target.publish(test_target());
            test
        }

        /// A drawing world that has not drawn yet, so it has published no
        /// target.
        fn without_target() -> Self {
            Self::with_capacity(8)
        }

        fn with_capacity(capacity: usize) -> Self {
            let mut render = World::new();
            render
                .root_scope_mut()
                .spawn(bundle![Source::new(SourceOrder::EGUI, EguiSource::new())]);
            let (sender, jobs) = async_channel::bounded(capacity);
            let target = Snapshot::new();
            let host = EguiPanelHost::new(JobSender::new(sender.clone()), target.reader(), 0);
            Self {
                render,
                jobs,
                sender,
                host,
                target,
            }
        }

        /// Lay out and send a frame, then run what was sent in the drawing
        /// world, the way a tick there would.
        fn publish(&mut self, world: &World) {
            self.host.publish(world, &frame(1.0 / 60.0));
            self.drain();
        }

        fn drain(&mut self) {
            while let Ok(job) = self.jobs.try_recv() {
                job(&mut self.render);
            }
        }

        fn published_slots(&mut self) -> usize {
            self.frames(|slots| slots.iter().filter(|slot| slot.is_some()).count())
        }

        fn frames<R>(&mut self, f: impl FnOnce(&mut [Option<ExternalFrame>]) -> R) -> R {
            for source in self.render.root_scope().query::<&mut Source>().iter() {
                if let Ok(ui) = source.as_mut::<EguiSource>() {
                    return ui.frames.with_frames(f);
                }
            }
            panic!("the drawing world holds an egui source");
        }
    }

    /// A frame of the duration these tests step at.
    fn frame(delta_time: f32) -> Frame {
        Frame {
            delta_time: unlit3d::time::FrameDelta::from_secs(f64::from(delta_time)),
            index: 0,
        }
    }

    /// The target the drawing side draws at in these tests.
    fn test_target() -> EguiTarget {
        EguiTarget {
            screen: ScreenDescriptor {
                size_in_pixels: [128, 96],
                pixels_per_point: 1.0,
            },
            max_texture_side: 2048,
        }
    }

    /// A world with a panel that counts how often it is laid out.
    ///
    /// The count is what tells a laid-out frame from a skipped tick: the panel
    /// runs once per pass egui makes.
    fn counting_world() -> (World, Rc<Cell<usize>>) {
        let mut world = World::new();
        let runs = Rc::new(Cell::new(0));
        let counted = Rc::clone(&runs);
        world
            .root_scope_mut()
            .spawn(bundle![EguiPanel::new(move |_, _, ui: &mut egui::Ui| {
                counted.set(counted.get() + 1);
                ui.label("hello");
            }),]);
        (world, runs)
    }

    /// Publish until the panel stops asking to be laid out again.
    fn settle(test: &mut TestUi, world: &World) {
        for _ in 0..8 {
            test.publish(world);
            if test.host.repaint_at.is_none() {
                return;
            }
        }
        panic!("the panel never settles");
    }

    /// Nothing is laid out before the drawing side has drawn once: until then
    /// there is no target, and a frame at a guessed size would be dropped.
    #[test]
    fn a_panel_waits_for_the_drawing_sides_target() {
        let (world, runs) = counting_world();
        let mut test = TestUi::without_target();

        test.publish(&world);
        assert_eq!(runs.get(), 0, "nothing was laid out");
        assert_eq!(test.published_slots(), 0, "no frame was sent");

        test.target.publish(test_target());
        test.publish(&world);
        assert_eq!(runs.get(), 1, "the panel ran");
        assert_eq!(test.published_slots(), 1, "the slot holds the frame");
    }

    /// A settled panel with no input sends nothing: the drawing side keeps
    /// drawing the frame it has.
    #[test]
    fn a_settled_panel_stops_sending() {
        let (world, runs) = counting_world();
        let mut test = TestUi::new();
        settle(&mut test, &world);

        let settled = runs.get();
        assert!(settled > 0, "the panel ran while settling");
        for _ in 0..4 {
            test.publish(&world);
        }
        assert_eq!(runs.get(), settled, "nothing was laid out again");
        assert_eq!(test.published_slots(), 1, "the frame is still there");
    }

    /// Input brings a settled panel back: the frame could now differ.
    #[test]
    fn input_sends_a_new_frame() {
        let (mut world, runs) = counting_world();
        let mut test = TestUi::new();
        let writer = InputWriter::new();
        world.root_scope_mut().spawn(writer.subscribe());
        settle(&mut test, &world);

        let settled = runs.get();
        writer.push(InputEvent::ModifiersChanged(Modifiers::default()));
        assert!(refresh_input(&world), "the world has a buffer");
        test.publish(&world);

        assert!(runs.get() > settled, "the event was laid out");
    }

    /// A target the frame was not laid out at is laid out again, however
    /// settled the panel is: the drawing side drops a frame that no longer
    /// matches its size, so the slot has to be filled with one that does.
    #[test]
    fn a_new_target_is_laid_out_at_once() {
        let (world, runs) = counting_world();
        let mut test = TestUi::new();
        settle(&mut test, &world);

        let settled = runs.get();
        let resized = EguiTarget {
            screen: ScreenDescriptor {
                size_in_pixels: [256, 192],
                pixels_per_point: 2.0,
            },
            max_texture_side: 2048,
        };
        test.target.publish(resized);
        test.publish(&world);

        assert!(runs.get() > settled, "the new target was laid out for");
        test.frames(|slots| {
            let frame = slots[0].as_ref().expect("published");
            assert_eq!(
                frame.screen, resized.screen,
                "the frame names the target it was laid out for"
            );
        });
    }

    /// A frame that asked to be laid out again after a delay is laid out once
    /// the delay is over, even though nothing else changed.
    #[test]
    fn a_delayed_repaint_comes_due() {
        let mut world = World::new();
        let runs = Rc::new(Cell::new(0));
        let counted = Rc::clone(&runs);
        world
            .root_scope_mut()
            .spawn(bundle![EguiPanel::new(move |_, _, ui: &mut egui::Ui| {
                counted.set(counted.get() + 1);
                ui.ctx()
                    .request_repaint_after(core::time::Duration::from_millis(50));
            }),]);
        let mut test = TestUi::new();

        // egui answers a request for the next possible pass with a second
        // one, so the first passes are immediate; the delay is what the
        // passes after those report.
        let before = test.host.time;
        (0..8)
            .find_map(|_| {
                test.publish(&world);
                test.host.repaint_at.filter(|at| *at > before)
            })
            .expect("a pass named a moment to be laid out again");

        // Before the deadline nothing is laid out, and the moment is kept
        // rather than dropped by the tick that skipped. The deadline is put
        // out of reach first, so the check does not depend on how long a
        // pass takes.
        test.host.repaint_at = Some(test.host.time + Duration::from_secs(60).as_secs_f64());
        let laid_out = runs.get();
        test.publish(&world);
        assert_eq!(runs.get(), laid_out, "the deadline has not come due");
        assert!(test.host.repaint_at.is_some(), "the moment is still kept");

        // Stand in for the ticks until the deadline elapses.
        test.host.repaint_at = Some(test.host.time);
        test.publish(&world);
        assert!(runs.get() > laid_out, "the deadline brought a frame");
    }

    /// A frame's texture ids are the ids its context allocated, and the
    /// drawing side keys textures by origin and id, so nothing moves them.
    #[test]
    fn texture_ids_are_not_rewritten() {
        let (world, _runs) = counting_world();
        let mut test = TestUi::new();
        settle(&mut test, &world);

        test.frames(|slots| {
            let frame = slots[0].as_ref().expect("published");
            let managed = frame
                .primitives
                .iter()
                .filter_map(|primitive| match &primitive.primitive {
                    egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(
                managed
                    .iter()
                    .any(|id| matches!(id, egui::TextureId::Managed(_))),
                "a label samples the font atlas"
            );
        });
    }

    /// A frame the drawing world's queue refused is sent again rather than
    /// counted as delivered: the drawing side still holds the older one.
    #[test]
    fn a_frame_the_queue_refuses_is_sent_again() {
        let (world, runs) = counting_world();
        let mut test = TestUi::with_capacity(1);
        test.target.publish(test_target());
        test.sender
            .try_send(Box::new(|_: &mut World| {}))
            .expect("the queue has room");

        // The send, without the drain: the queue is still full when it is
        // made, so the frame is refused rather than delivered.
        test.host.publish(&world, &frame(1.0 / 60.0));
        assert_eq!(test.jobs.len(), 1, "the placeholder is still queued");
        assert_eq!(test.published_slots(), 0, "no frame reached the source");

        test.drain();
        let laid_out = runs.get();
        test.publish(&world);
        assert!(runs.get() > laid_out, "the frame was laid out again");
        assert_eq!(test.published_slots(), 1, "the retry reached the source");
    }

    /// A refused frame rides into whatever the next tick lays out — its
    /// requests were made first, so they lead the newer frame's — and the
    /// fold itself survives a second refusal: waiting for room loses
    /// nothing the drawing side never applied.
    #[test]
    fn a_refused_frame_joins_the_next_frame_its_requests_leading() {
        let mut world = World::new();
        // Each laid-out pass asks for its own URL, so the fold's order is
        // readable: the frame that was refused first must ask first.
        let urls = Rc::new(Cell::new(0usize));
        let counted = Rc::clone(&urls);
        world
            .root_scope_mut()
            .spawn(bundle![EguiPanel::new(move |_, _, ui: &mut egui::Ui| {
                let pass = counted.get();
                counted.set(pass + 1);
                ui.label("hello");
                ui.ctx().open_url(egui::OpenUrl {
                    url: format!("frame-{pass}"),
                    new_tab: false,
                });
            }),]);
        let writer = InputWriter::new();
        world.root_scope_mut().spawn(writer.subscribe());
        let mut test = TestUi::with_capacity(1);
        test.target.publish(test_target());

        test.publish(&world);
        assert_eq!(test.published_slots(), 1, "the first frame was delivered");

        // Fill the queue, then bring input: the frame laid out for it is
        // refused. The queue is drained but not refilled, and newer input
        // arrives: the refused frame is refused again, and joins the frame
        // that tick lays out.
        test.sender
            .try_send(Box::new(|_: &mut World| {}))
            .expect("the queue has room");
        writer.push(InputEvent::ModifiersChanged(Modifiers::default()));
        assert!(refresh_input(&world), "the world has a buffer");
        test.host.publish(&world, &frame(1.0 / 60.0));
        assert_eq!(test.jobs.len(), 1, "the placeholder is still queued");

        test.drain();
        test.sender
            .try_send(Box::new(|_: &mut World| {}))
            .expect("the queue has room");
        writer.push(InputEvent::ModifiersChanged(Modifiers::default()));
        assert!(refresh_input(&world), "the world has a buffer");
        test.host.publish(&world, &frame(1.0 / 60.0));
        assert_eq!(test.jobs.len(), 1, "the placeholder is still queued");

        // Room again, and no newer input: what is sent is the fold, the
        // refused frame's requests leading the newer frame's.
        test.drain();
        assert!(refresh_input(&world), "the world has a buffer");
        test.publish(&world);
        assert_eq!(test.published_slots(), 1, "the folded frame was delivered");
        // Every pass that laid a frame out had asked for a URL, and egui makes
        // the pass after such a request due at once, so all four publishes
        // laid a frame out. Only the two refusals' requests are lost, and the
        // delivering pass lays one more frame whose request the fold will
        // carry on the next publish.
        assert_eq!(
            urls.get(),
            4,
            "one layout per publish, none retried by a refusal"
        );
        test.frames(|slots| {
            let frame = slots[0].as_ref().expect("published");
            let urls: Vec<_> = frame
                .platform_output
                .commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::OpenUrl(open) => Some(open.url.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                urls,
                ["frame-1", "frame-2"],
                "the refused frame's requests lead, in the order they were made"
            );
        });
    }
}
