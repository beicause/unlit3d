//! UI panels laid out in one world and drawn in another.
//!
//! A `unlit3d::ui::UiPanel` can only be built where its `egui::Context` runs: the
//! callback is handed a `&mut egui::Ui`, which belongs to the pass building it,
//! so it cannot be carried to another thread. A UI whose panels belong to one
//! world and whose drawing belongs to another therefore crosses the boundary as
//! **data**: the panel's world lays the frame out and tessellates it, and the
//! drawing world uploads and records what comes back.
//!
//! The transport is the one every other cross-world exchange uses, a job that
//! answers back through a reply slot. The drawing side holds a `JobSender` for
//! the world holding the panels, so it asks that world, in a job, to lay a frame
//! out; the job runs where the panels are and answers with the frame, which the
//! drawing side collects on a later frame. Each side reaches only what it can
//! name: the drawing side the other world's command queue, and the job the
//! world it runs in. Nothing is shared between the two.
//!
//! The two sides hold **separate** contexts. Sharing one would have both worlds
//! writing egui's memory and its font atlas, and egui treats one `run_ui` call
//! as one whole frame, so two worlds running it against one context would
//! interleave two frames into one. Their texture ids therefore come from
//! different namespaces, which the drawing side renumbers per provider.

use unlit_ecs::World;
use unlit_wgpu::ui::ScreenDescriptor;
use unlit3d::input::{InputEvent, InputEvents};
use unlit3d::ui::{ExternalFrame, ExternalFrames, UiScreen, UiTarget, egui, lay_out, raw_input};
use web_time::Instant;

/// The providing side: a world's panels, laid out for another world's
/// [`unlit3d::ui::UiSource`].
///
/// Spawn it in the world holding the panels. It is inert until the drawing side
/// asks for a frame, and it is asked inside a job that world runs, so it needs
/// no tick of its own and keeps nothing the drawing side can reach.
pub struct UiHost {
    /// The context the panels in this world are built in.
    ctx: egui::Context,
    /// When the host was created, the origin of egui's frame clock.
    start: Instant,
}

impl UiHost {
    /// A host with a fresh context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            start: Instant::now(),
        }
    }

    /// egui's context, for restyling the interfaces this host builds.
    pub fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// Whether the frame just laid out asked for another one.
    ///
    /// A published frame is read after the pass that produced it: a panel that
    /// started an animation or a text field that is being edited is still
    /// changing, and the world laying it out has to keep doing so even without
    /// input. Without this a published frame would freeze the moment the user
    /// stopped moving the mouse.
    pub fn requested_repaint(&self) -> bool {
        self.ctx.has_requested_repaint()
    }

    /// Lay out one frame of this world's panels and tessellate it.
    ///
    /// `target` is the size and density the drawing side will draw at, and
    /// `events` are this tick's, already translated into egui's types. egui's
    /// clock is this host's own, so a panel's animations run on the world that
    /// builds it, however fast that world ticks.
    pub fn lay_out(
        &self,
        world: &World,
        target: UiTarget,
        events: Vec<egui::Event>,
    ) -> ExternalFrame {
        let input = raw_input(
            target,
            Some(self.start.elapsed().as_secs_f64()),
            true,
            events,
        );
        lay_out(world, &self.ctx, input, target.screen)
    }
}

impl Default for UiHost {
    fn default() -> Self {
        Self::new()
    }
}

/// A world's panel publishing: its host, the slots it writes and the screen it
/// lays out against.
///
/// Spawn it in the world holding the panels, beside the [`UiHost`]. Nothing
/// drives this from outside: the world ticks it, the way it ticks any other
/// behaviour, and each tick that produced a frame publishes it into the slot
/// the drawing side reads. A tick that produced nothing to draw — a settled
/// panel with no input — publishes nothing, so an idle UI costs nothing.
pub struct PanelHost {
    /// The host carrying the context the panels are built in.
    host: UiHost,
    /// The slots every frame is published into.
    frames: ExternalFrames,
    /// The target the drawing side last drew at.
    screen: UiScreen,
    /// Which slot this world's frames go in.
    slot: usize,
    /// The events of the frame being laid out, reclaimed from egui once the
    /// pass is over so the next frame reuses the allocation.
    event_scratch: Vec<egui::Event>,
    /// Whether the last frame laid out asked for another.
    repaint: bool,
    /// Whether a frame has ever been laid out: the first one is, whatever the
    /// panel asked for afterwards.
    drawn: bool,
}

impl PanelHost {
    /// Publish `world`'s panels into `frames`' slot `slot`, against the target
    /// `screen` carries.
    #[must_use]
    pub fn new(host: UiHost, frames: ExternalFrames, screen: UiScreen, slot: usize) -> Self {
        Self {
            host,
            frames,
            screen,
            slot,
            event_scratch: Vec::new(),
            repaint: false,
            drawn: false,
        }
    }

    /// egui's context, for restyling the interfaces this host builds.
    pub fn context(&self) -> &egui::Context {
        self.host.context()
    }

    /// Lay out this tick's frame and publish it, if it could differ from the
    /// one the drawing side holds.
    ///
    /// The frame is laid out here rather than where it is drawn because only
    /// this world holds the panels and their context. Input decides whether it
    /// is worth laying one out at all: a settled panel with no events and no
    /// repaint request draws the same picture, so nothing is laid out and the
    /// slot keeps the frame it has.
    pub fn publish(&mut self, world: &World, events: &[InputEvent]) {
        let target = self.screen.current(fallback_target(world));
        if self.drawn && !self.repaint && events.is_empty() {
            return;
        }
        let mut scratch = core::mem::take(&mut self.event_scratch);
        scratch.clear();
        // The events this world received are in its own types; egui wants its
        // own, translated at the target's density so a click lands where the
        // pointer is in logical points.
        unlit3d::ui::convert::to_egui_events_into(
            events,
            target.screen.pixels_per_point,
            &mut scratch,
        );
        let frame = self.host.lay_out(world, target, scratch);
        // The pass is over, so egui no longer needs the list it was handed:
        // taking it back is what turns the next frame's fill into a reuse.
        self.event_scratch = self
            .host
            .context()
            .input_mut(|input| core::mem::take(&mut input.raw.events));
        self.repaint = self.host.requested_repaint();
        self.drawn = true;
        self.frames.publish(self.slot, frame);
    }
}

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
        // Nothing can be uploaded yet, so the smallest sane atlas is what egui
        // should assume; the real limit arrives with the first drawn frame.
        max_texture_side: 2048,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlit3d::ui::UiPanel;

    /// A world holding a host, its slots and the screen the drawing side draws
    /// at.
    fn test_host() -> (World, PanelHost) {
        let world = World::new();
        let frames = ExternalFrames::new();
        let screen = UiScreen::new();
        let target = UiTarget {
            screen: ScreenDescriptor {
                size_in_pixels: [128, 96],
                pixels_per_point: 1.0,
            },
            max_texture_side: 2048,
        };
        screen.publish(target);
        let host = PanelHost::new(UiHost::new(), frames, screen, 0);
        (world, host)
    }

    /// The first tick always lays a frame out, so the drawing side has
    /// something to draw instead of an empty slot.
    #[test]
    fn the_first_tick_publishes_a_frame() {
        let (world, mut host) = test_host();
        assert_eq!(host.frames.len(), 0);

        host.publish(&world, &[]);

        assert_eq!(host.frames.len(), 1);
        host.frames.with_frames(|slots| {
            let frame = slots[0].as_ref().expect("published");
            assert!(
                frame.primitives.is_empty(),
                "a world with no panels has no geometry"
            );
        });
    }

    /// A settled panel with no input publishes nothing: the drawing side keeps
    /// drawing the frame it has.
    #[test]
    fn a_settled_panel_stops_publishing() {
        let (world, mut host) = test_host();

        // egui answers a repaint request with a further pass, so a context
        // still settling asks for more: tick until one stops asking.
        let mut settled = false;
        for _ in 0..8 {
            host.publish(&world, &[]);
            if !host.repaint {
                settled = true;
                break;
            }
        }
        assert!(settled, "the panel stops asking after a few passes");

        let before = host
            .frames
            .with_frames(|slots| slots[0].as_ref().map(|frame| frame.primitives.len()));

        // Nothing changed and nothing arrived, so no frame is laid out and the
        // slot keeps the one it has.
        for _ in 0..4 {
            host.publish(&world, &[]);
        }
        let after = host
            .frames
            .with_frames(|slots| slots[0].as_ref().map(|frame| frame.primitives.len()));
        assert_eq!(before, after);
    }

    /// Input brings a settled panel back: the frame could now differ.
    #[test]
    fn input_publishes_a_new_frame() {
        let (world, mut host) = test_host();
        let mut settled = false;
        for _ in 0..8 {
            host.publish(&world, &[]);
            if !host.repaint {
                settled = true;
                break;
            }
        }
        assert!(settled);

        let events = [InputEvent::ModifiersChanged(
            unlit3d::input::Modifiers::default(),
        )];
        host.publish(&world, &events);
        assert!(host.drawn);
    }

    /// A world with no host still publishes: the slots belong to the world
    /// holding the panels, and a panel-less world publishes empty frames.
    #[test]
    fn texture_ids_are_not_rewritten() {
        let (mut world, mut host) = test_host();
        host.publish(&world, &[]);

        // A frame's ids are the ids its context allocated. The drawing side
        // keys textures by (source, id), so nothing has to move them.
        world.spawn((UiPanel::new(|_, _, ui: &mut egui::Ui| {
            ui.label("hello");
        }),));
        for _ in 0..8 {
            host.publish(&world, &[]);
            if !host.repaint {
                break;
            }
        }
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
