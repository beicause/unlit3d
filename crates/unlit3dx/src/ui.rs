//! UI panels laid out in one world and drawn in another.
//!
//! A `unlit3d::ui::UiPanel` can only be built where its `egui::Context` runs: the
//! callback is handed a `&mut egui::Ui`, which belongs to the pass building it,
//! so it cannot be carried to another thread. A UI whose panels belong to one
//! world and whose drawing belongs to another therefore crosses the boundary as
//! **data**: the panel's world lays the frame out and tessellates it, and the
//! drawing world uploads and records what comes back.
//!
//! The handover is one way and needs no reply: the panel world writes an
//! [`unlit3d::ui::ExternalFrame`] into a slot of the
//! [`unlit3d::ui::ExternalFrames`] both worlds hold,
//! and the drawing world draws whatever a slot holds. Nothing is ever asked
//! for, so a settled panel publishes nothing and the frame already there keeps
//! being drawn. What comes back is only the target the drawing side draws at,
//! carried by a [`unlit3d::ui::UiScreen`], so a frame is laid out at the size and density it
//! will be drawn at.
//!
//! The two sides hold **separate** contexts. Sharing one would have both worlds
//! writing egui's memory and its font atlas, and egui treats one `run_ui` call
//! as one whole frame, so two worlds running it against one context would
//! interleave two frames into one. Their texture ids both start at zero, which
//! the drawing side handles by keying textures by origin and id: nothing
//! renumbers them.

use core::time::Duration;

use unlit_ecs::World;
use unlit_wgpu::ui::ScreenDescriptor;
use unlit3d::input::{InputEvent, InputEvents};
use unlit3d::ui::{ExternalFrames, UiScreen, UiTarget, egui, lay_out, raw_input};
use web_time::Instant;

/// A world's panel publishing: the context its panels are built in, the slots
/// it writes and the screen it lays out against.
///
/// Spawn it in the world holding the panels. Nothing drives this from outside:
/// the world ticks it, the way it ticks any other behaviour, and each tick that
/// produced a frame publishes it into the slot the drawing side reads. A tick
/// that could not change the picture — a settled panel, no input, the same
/// target — publishes nothing, so an idle UI costs nothing.
///
/// The context, its font atlas and its memory belong to this world and stay
/// there: what crosses to the drawing side is the laid-out frame, which carries
/// no borrow of any of them.
pub struct UiPanelHost {
    /// The context the panels in this world are built in.
    ctx: egui::Context,
    /// When the host was created, the origin of egui's frame clock.
    start: Instant,
    /// The slots every frame is published into.
    frames: ExternalFrames,
    /// The target the drawing side last drew at.
    screen: UiScreen,
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
    /// it.
    laid_out_at: Option<UiTarget>,
    /// When the last frame laid out asked to be laid out again, for a frame
    /// that named a moment rather than asking for the next possible pass.
    repaint_at: Option<Instant>,
}

impl UiPanelHost {
    /// Publish `world`'s panels into `frames`' slot `slot`, against the target
    /// `screen` carries.
    #[must_use]
    pub fn new(frames: ExternalFrames, screen: UiScreen, slot: usize) -> Self {
        Self {
            ctx: egui::Context::default(),
            start: Instant::now(),
            frames,
            screen,
            slot,
            event_scratch: Vec::new(),
            input_scratch: Vec::new(),
            laid_out_at: None,
            repaint_at: None,
        }
    }

    /// Lay out this tick's frame and publish it, if it could differ from the
    /// one the drawing side holds.
    ///
    /// The frame is laid out here rather than where it is drawn because only
    /// this world holds the panels and their context. A tick publishes when
    /// something could have changed the picture: there is no frame yet, the
    /// target moved, input arrived, or the last frame asked to be laid out
    /// again — either at once or once a delay it named has elapsed. Anything
    /// else draws the same picture, so nothing is laid out and the slot keeps
    /// the frame it has.
    ///
    /// The events are this world's own, read from its [`InputEvents`]: a panel
    /// sees exactly what the rest of the world sees on the same tick.
    pub fn publish(&mut self, world: &World) {
        let target = self.screen.current(fallback_target(world));
        // Copied out before anything is laid out: a panel may read its world's
        // input, and the copy is what keeps that read from taking the same lock
        // twice.
        let mut events = core::mem::take(&mut self.input_scratch);
        events.clear();
        if let Some((_, input)) = world.query::<&InputEvents>().next() {
            input.copy_events_into(&mut events);
        }
        let due = self
            .repaint_at
            .is_some_and(|deadline| Instant::now() >= deadline);
        if self.laid_out_at == Some(target) && !due && events.is_empty() {
            self.input_scratch = events;
            return;
        }
        let mut scratch = core::mem::take(&mut self.event_scratch);
        scratch.clear();
        // The events this world received are in its own types; egui wants its
        // own, translated at the target's density so a click lands where the
        // pointer is in logical points.
        unlit3d::ui::convert::to_egui_events_into(
            &events,
            target.screen.pixels_per_point,
            &mut scratch,
        );
        self.input_scratch = events;
        // egui's clock is this host's own, so a panel's animations run on the
        // world that builds it, however fast that world ticks.
        let input = raw_input(
            target,
            Some(self.start.elapsed().as_secs_f64()),
            true,
            scratch,
        );
        let frame = lay_out(world, &self.ctx, input, target.screen);
        // The pass is over, so egui no longer needs the list it was handed:
        // taking it back is what turns the next frame's fill into a reuse.
        self.event_scratch = self
            .ctx
            .input_mut(|input| core::mem::take(&mut input.raw.events));
        // A frame that asked for nothing named no moment, and the picture is
        // settled. One that asked for the next possible pass is answered on
        // the next tick, whenever that is; one that named a delay is answered
        // once it is over. Either way the request lives only in the pass that
        // made it, so keeping the moment is this side's job.
        self.repaint_at = match frame.repaint_delay {
            delay if delay == Duration::MAX => None,
            Duration::ZERO => Some(Instant::now()),
            // A delay the clock cannot place is one that never comes.
            delay => Instant::now().checked_add(delay),
        };
        self.laid_out_at = Some(target);
        self.frames.publish(self.slot, frame);
    }
}

/// The atlas size a frame is laid out for before any device has said what it
/// can hold.
///
/// The fallback is the smallest atlas every backend accepts; the real limit
/// arrives with the first target the drawing side publishes, and only the
/// frames laid out before that one ever see it.
const FALLBACK_TEXTURE_SIDE: u32 = 2048;

/// The target to lay a frame out against before the drawing side has drawn one.
///
/// A world that publishes a frame before anything is on screen has no target to
/// match yet. It lays the frame out at the size the frame loop reported, from
/// the world's own `InputEvents`; the sizes agree on the first drawn frame, and
/// from then on the published descriptor is what both sides use.
fn fallback_target(world: &World) -> UiTarget {
    let state = world
        .query::<&InputEvents>()
        .next()
        .map(|(_, events)| events.with_state(|state| (state.scale_factor, state.size_px)));
    let (scale_factor, size_px) = state.unwrap_or((1.0, (0, 0)));
    let pixels_per_point = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    UiTarget {
        screen: ScreenDescriptor {
            size_in_pixels: [size_px.0, size_px.1],
            pixels_per_point,
        },
        max_texture_side: FALLBACK_TEXTURE_SIDE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use std::rc::Rc;
    use unlit3d::input::{InputWriter, Modifiers, refresh_input};
    use unlit3d::ui::UiPanel;

    /// A world holding a host, its slots and the screen the drawing side draws
    /// at.
    fn test_host() -> (World, UiPanelHost) {
        let world = World::new();
        let frames = ExternalFrames::new();
        let screen = UiScreen::new();
        screen.publish(test_target());
        let host = UiPanelHost::new(frames, screen, 0);
        (world, host)
    }

    /// The target the drawing side draws at in these tests.
    fn test_target() -> UiTarget {
        UiTarget {
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
        world.spawn((UiPanel::new(move |_, _, ui: &mut egui::Ui| {
            counted.set(counted.get() + 1);
            ui.label("hello");
        }),));
        (world, runs)
    }

    /// How many slots hold a frame.
    fn published_slots(host: &UiPanelHost) -> usize {
        host.frames
            .with_frames(|slots| slots.iter().filter(|slot| slot.is_some()).count())
    }

    /// Publish until the panel stops asking to be laid out again.
    fn settle(host: &mut UiPanelHost, world: &World) {
        for _ in 0..8 {
            host.publish(world);
            if host.repaint_at.is_none() {
                return;
            }
        }
        panic!("the panel never settles");
    }

    /// The first tick always lays a frame out, so the drawing side has
    /// something to draw instead of an empty slot.
    #[test]
    fn the_first_tick_publishes_a_frame() {
        let (world, runs) = counting_world();
        let (_, mut host) = test_host();
        assert!(published_slots(&host) == 0, "no frame is published yet");

        host.publish(&world);

        assert_eq!(runs.get(), 1, "the panel ran");
        assert_eq!(published_slots(&host), 1, "the slot holds the frame");
    }

    /// A settled panel with no input publishes nothing: the drawing side keeps
    /// drawing the frame it has.
    #[test]
    fn a_settled_panel_stops_publishing() {
        let (world, runs) = counting_world();
        let (_, mut host) = test_host();
        settle(&mut host, &world);

        let settled = runs.get();
        assert!(settled > 0, "the panel ran while settling");
        for _ in 0..4 {
            host.publish(&world);
        }
        assert_eq!(runs.get(), settled, "nothing was laid out again");
    }

    /// Input brings a settled panel back: the frame could now differ.
    #[test]
    fn input_publishes_a_new_frame() {
        let (mut world, runs) = counting_world();
        let (_, mut host) = test_host();
        let writer = InputWriter::new();
        world.spawn((writer.subscribe(),));
        settle(&mut host, &world);

        let settled = runs.get();
        writer.push(InputEvent::ModifiersChanged(Modifiers::default()));
        assert!(refresh_input(&world), "the world has a buffer");
        host.publish(&world);

        assert!(runs.get() > settled, "the event was laid out");
    }

    /// A socket without input still publishes, and the frame it publishes is
    /// laid out at the fallback target until the drawing side reports one.
    #[test]
    fn a_world_without_input_still_lays_out() {
        let (world, runs) = counting_world();
        let (_, mut host) = test_host();

        host.publish(&world);

        assert_eq!(runs.get(), 1);
        host.frames.with_frames(|slots| {
            let frame = slots[0].as_ref().expect("published");
            assert_eq!(
                frame.screen,
                test_target().screen,
                "laid out at the published target"
            );
        });
    }

    /// A target the frame was not laid out at is laid out again, however
    /// settled the panel is: the drawing side drops a frame that no longer
    /// matches its size, so the slot has to be filled with one that does.
    #[test]
    fn a_new_target_is_laid_out_at_once() {
        let (world, runs) = counting_world();
        let (_, mut host) = test_host();
        settle(&mut host, &world);

        let settled = runs.get();
        let resized = UiTarget {
            screen: ScreenDescriptor {
                size_in_pixels: [256, 192],
                pixels_per_point: 2.0,
            },
            max_texture_side: 2048,
        };
        host.screen.publish(resized);
        host.publish(&world);

        assert!(runs.get() > settled, "the new target was laid out for");
        host.frames.with_frames(|slots| {
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
        world.spawn((UiPanel::new(move |_, _, ui: &mut egui::Ui| {
            counted.set(counted.get() + 1);
            ui.ctx()
                .request_repaint_after(core::time::Duration::from_millis(50));
        }),));
        let (_, mut host) = test_host();

        // egui answers a request for the next possible pass with a second
        // one, so the first passes are immediate; the delay is what the
        // passes after those report.
        let before = Instant::now();
        (0..8)
            .find_map(|_| {
                host.publish(&world);
                host.repaint_at.filter(|at| *at > before)
            })
            .expect("a pass named a moment to be laid out again");

        // Before the deadline nothing is laid out, and the moment is kept
        // rather than dropped by the tick that skipped. The deadline is put
        // out of reach first, so the check does not depend on how long a
        // pass takes.
        host.repaint_at = Some(Instant::now() + Duration::from_secs(60));
        let laid_out = runs.get();
        host.publish(&world);
        assert_eq!(runs.get(), laid_out, "the deadline has not come due");
        assert!(host.repaint_at.is_some(), "the moment is still kept");

        // Stand in for the ticks until the deadline elapses.
        host.repaint_at = Some(Instant::now());
        host.publish(&world);
        assert!(runs.get() > laid_out, "the deadline brought a frame");
    }

    /// A frame's texture ids are the ids its context allocated, and the
    /// drawing side keys textures by origin and id, so nothing moves them.
    #[test]
    fn texture_ids_are_not_rewritten() {
        let (world, _runs) = counting_world();
        let (_, mut host) = test_host();
        settle(&mut host, &world);

        host.frames.with_frames(|slots| {
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
}
