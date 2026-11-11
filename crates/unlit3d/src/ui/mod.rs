//! An egui UI overlay, drawn as a frame source.
//!
//! [`UiSource`] is a [`FrameSource`] that renders the world's egui interfaces
//! over whatever the frame already holds. The interfaces themselves are
//! [`UiPanel`] behaviour components: the source is only their driver, and it
//! runs whatever panels its own scope carries, in query order.
//!
//! ```rust
//! use unlit3d::prelude::*;
//! use unlit_wgpu::resources::ResourceGraph;
//!
//! let (device, queue) =
//!     wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut world = World::new();
//! let _ctx = spawn_context(
//!     &mut world,
//!     device,
//!     queue,
//!     ResourceGraph::new(),
//!     DeviceCapabilities::default(),
//! );
//!
//! // The source, and one interface held as a behaviour component. More
//! // panels are more entities under the same source, each with its own
//! // sibling state.
//! let ui = world.root_scope_mut().spawn(bundle![Source::new(UiSource::new())]);
//! world.scope_mut(ui).unwrap().spawn(bundle![
//!     UiPanel::new(|_world, _entity, ui| {
//!         ui.label("hello");
//!     }),
//! ]);
//! ```
//!
//! The UI draws with the same built-in lit pipeline as everything else and
//! binds the frame's uniforms rather than records of its own: it synthesises a
//! camera that maps its points onto clip space and states the point size as the
//! view its pass covers, and asks the frame's registry for those records. The
//! registry keys a camera by what it was derived from, so a screen that was not
//! resized reuses the record, the buffer and the bind group of the frame before
//! it.

use core::time::Duration;

use unlit_ecs::{Entity, Scope, World};
use unlit_wgpu::scene::Scene;
use unlit_wgpu::specialize::SpecializedPipeline;
use unlit_wgpu::specialize::SurfaceKey;
use unlit_wgpu::ui::{EguiIntegration, ScreenDescriptor, Source, UiFrame, screen_view, ui_variant};
// `std::time::Instant` panics on `wasm32-unknown-unknown`, where the standard
// library has no clock; `web-time` reads the browser's `Performance.now()`
// there and re-exports `std::time` everywhere else.
use web_time::Instant;

pub use crate::source::{OpenUrl, OpenUrls};

use crate::global_uniforms::{CameraKey, UniformGroups, ViewKey};
use crate::input::InputEvents;
use crate::pass::PassInfo;
use crate::pipeline::{GlobalRecipe, uniform_entries};
use crate::relay::Snapshot;
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};
/// egui, re-exported because a [`UiPanel`] is written against its types.
///
/// A panel's callback takes an `&mut egui::Ui` and draws with egui's own
/// widgets, so a caller needs the crate that defines them. Re-exporting it
/// here keeps the version a panel is written against the same one the source
/// drives, instead of leaving the two to agree by hand.
pub use egui;

pub mod convert;

/// A callback that may read and write the world, and receives the entity it
/// runs for together with the UI surface its interface is built in.
type PanelCallback = Box<dyn FnMut(Scope<'_>, Entity, &mut egui::Ui)>;

/// A UI panel: one interface, held as a behaviour component.
///
/// A panel is the interface itself — the closure egui runs to build it — and
/// not a field on the source, so an interface lives in the world like any
/// other behaviour: multiple panels are multiple entities, each free to carry
/// its own sibling state, and a third party can define its own panel component
/// for a [`UiSource`] to drive.
///
/// # What a panel must not assume
///
/// A panel is **not** run exactly once per frame. egui's `Context::run_ui`
/// calls its closure again whenever a pass asks for a discard to redo a
/// multi-pass layout, so a panel may run several times in one frame. Anything
/// that must advance once per frame belongs in a **sibling component**, which
/// is also where a panel's own state belongs: a behaviour component is
/// borrowed while it runs, so a panel must not reach for its own
/// [`UiPanel`] — that is a borrow panic, not a compile error.
///
/// A panel may otherwise read and write the world freely through the
/// [`Scope`] it is handed: `get`, `query` and structural changes queued
/// for the frame loop to apply are all available while it runs. The scope is the
/// one of the source that draws the panel, so a panel sees the subtree that
/// source owns and the shared resources, not every entity in the world.
pub struct UiPanel(pub PanelCallback);

impl UiPanel {
    /// Wrap `f` as a [`UiPanel`].
    pub fn new(f: impl FnMut(Scope<'_>, Entity, &mut egui::Ui) + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Run this panel for `entity`, building its interface in `ui`.
    pub fn run(&mut self, scope: Scope<'_>, entity: Entity, ui: &mut egui::Ui) {
        (self.0)(scope, entity, ui)
    }
}

/// The GPU state of a [`UiSource`], built for one [`SurfaceKey`].
///
/// One entry per target the source has been asked to draw into: a source may be
/// recorded by several passes, and the passes of one frame need not share a
/// key — a multisampled offscreen pass and a single-sampled swap chain differ in
/// sample count. Each entry has uniforms and an integration of its own, because
/// the integration holds the pipeline specialized for that target and the
/// texture atlases uploaded for it.
struct Gpu {
    /// How the global bind group is built for the pipeline this target is
    /// specialized for.
    recipe: GlobalRecipe,
    /// The global bind groups the integration's draws are recorded with, one
    /// per [`UniformsKey`](crate::global_uniforms::UniformsKey) the frame
    /// draws the target through.
    ///
    /// The UI has no camera entity: it synthesises a
    /// [`Camera`](unlit_wgpu::globals::Camera) mapping its points onto clip
    /// space and states the point size as the [`View`](unlit_wgpu::globals::View) its
    /// pass covers, so the frame's uniform registry holds those records rather
    /// than the source. A target that grows or shrinks asks for another key,
    /// and the groups of the keys it no longer draws are pruned each frame.
    groups: UniformGroups,
    /// Uploads egui's textures and geometry and records its draws.
    integration: EguiIntegration,
}

/// One target's [`Gpu`] state.
///
/// Entries are kept for the world's life: a key changes with a target's format
/// or sample count, not with its size, so a windowed frame loop settles on one
/// or two of them and never rebuilds as the passes alternate.
struct Target {
    /// The target's specialization key.
    key: SurfaceKey,
    /// The state built for it.
    gpu: Gpu,
}

/// A frame source that draws the world's egui interfaces as an overlay.
///
/// The source owns egui's [`Context`](egui::Context) — its font atlas and
/// memory survive across frames — and drives every [`UiPanel`] in the world.
/// Spawn it like any other source ([`Source::new`](crate::source::Source::new));
/// it declares
/// [`FrameSourceOrder::UI`], so it records after the meshes.
///
/// Nothing is built until the first frame: the source's pipeline is
/// specialized on the frame's target, which is only known in
/// [`FrameSource::build_scene`]. The pipeline and its bind groups are built
/// then, and rebuilt when the target's [`SurfaceKey`] changes.
///
/// The UI binds uniforms of the frame's own registry: its projection maps
/// logical points onto clip space rather than world space, so it names a camera
/// of its own — the screen mapping — and a view of its own, and the frame keeps
/// one record and one buffer per distinct pair rather than one per source.
pub struct UiSource {
    /// egui's state — fonts, memory, style — kept across frames.
    ctx: egui::Context,
    /// When the source was created, the origin of egui's frame clock.
    start: Instant,
    /// The GPU state, one entry per target the source has drawn into.
    targets: Vec<Target>,
    /// The events translated for the frame being laid out, reclaimed from egui
    /// once its pass is over so the next frame reuses the allocation.
    event_scratch: Vec<egui::Event>,
    /// The slots frames published by other worlds are drawn from, still drawn
    /// until a newer frame arrives.
    pub frames: ExternalFrames,
    /// The size and density this source draws at, published for the worlds
    /// laying frames out.
    screen: Snapshot<UiTarget>,
    /// The URLs every frame drawn this frame asked to open, gathered before
    /// they are published together, reused across frames.
    url_scratch: Vec<OpenUrl>,
}

impl UiSource {
    /// Create a UI source.
    ///
    /// No GPU state is built here: the pipeline is specialized on the frame's
    /// target, which is not known until the first frame. Everything is built
    /// then, on the device the frame's [`RenderContext`] names.
    pub fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            start: Instant::now(),
            targets: Vec::new(),
            event_scratch: Vec::new(),
            frames: ExternalFrames::new(),
            screen: Snapshot::new(),
            url_scratch: Vec::new(),
        }
    }

    /// Publish this source's target for the worlds laying frames out.
    ///
    /// This is the seam for a UI that lives in another world: a panel can only
    /// be built where its egui context runs, so the world holding it lays the
    /// frame out against this target and sends it here, where it is drawn.
    ///
    /// The source is also the only party that knows the target it draws into:
    /// its real pixel size, its density and the device's texture limit. A
    /// frame laid out at one size and drawn at another would have its clip
    /// rectangles computed in the wrong pixels, so the laying-out side reads
    /// the descriptor back from here rather than deriving one of its own. It
    /// is published on every frame drawn, so a resize or a density change
    /// reaches the layout one frame later rather than never.
    pub fn publish_target(&mut self, screen: Snapshot<UiTarget>) -> &mut Self {
        self.screen = screen;
        self
    }

    /// egui's context, for reading its style or memory.
    ///
    /// Restyling the UI goes through here rather than through a new source:
    /// the context is the source's state, and replacing the source would throw
    /// away the font atlas with it.
    pub fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// egui's context, for restyling it or setting its zoom factor.
    pub fn context_mut(&mut self) -> &mut egui::Context {
        &mut self.ctx
    }

    /// Run every entity that carries a [`UiPanel`], in query order.
    ///
    /// The query is rebuilt from scratch on every pass egui runs, which is what
    /// a multi-pass layout needs.
    fn run_panels(&self, scope: &Scope<'_>, input: egui::RawInput) -> egui::FullOutput {
        let root = *scope;
        self.ctx.run_ui(input, |ui| {
            for (ui_panel, entity) in root.query::<(&mut UiPanel, Entity)>().iter() {
                ui_panel.run(root, entity, ui);
            }
        })
    }

    /// The index of the entry for `surface`, building it on first use.
    ///
    /// A source may be recorded by several passes, and the passes of one frame
    /// need not share a key — a multisampled offscreen pass and a single-
    /// sampled swap chain differ in sample count — so the state is kept per key
    /// rather than rebuilt when the key changes: alternating passes would
    /// otherwise specialize the pipeline, and drop the material table with it,
    /// twice per frame.
    fn target_index(&mut self, device: &wgpu::Device, surface: SurfaceKey) -> usize {
        if let Some(index) = self.targets.iter().position(|target| target.key == surface) {
            return index;
        }
        // A UI drawn into an sRGB target encodes its own output in gamma space,
        // so the fragment converts to linear; the same call the winit path
        // makes for its swap chain.
        let variant = ui_variant(surface.color_format.has_srgb_suffix(), surface);
        let pipeline = SpecializedPipeline::create(device, variant);
        // The UI binds no array of its own: its group is the frame's three
        // uniforms alone, so the recipe reads them out of the registry and
        // nothing else.
        let layout = pipeline.descriptor().bind_group_layouts(device).global;
        let recipe = GlobalRecipe::new(move |device: &wgpu::Device, graph, uniforms| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("unlit3d::ui::globals"),
                layout: &layout,
                entries: &uniform_entries(graph, uniforms),
            })
        });
        self.targets.push(Target {
            key: surface,
            gpu: Gpu {
                recipe,
                groups: UniformGroups::new(),
                integration: EguiIntegration::new(device, pipeline),
            },
        });
        self.targets.len() - 1
    }
}

impl Default for UiSource {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSource for UiSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Settle the graph on every way out of this build, the early return
        // below included. The pass runs where this scope ends, once the
        // integration has uploaded and before the scene reads a group.
        let maintain = ctx.maintain_scope(*scope);

        // The pass states the target the frame is specialized for, so nothing
        // has to be read back out of the world: the driver builds a source only
        // for a pass that has attachments.
        let target = info;

        let device = ctx
            .device(scope)
            .expect("the context's device resource exists");
        let queue = ctx
            .queue(scope)
            .expect("the context's queue resource exists");
        let mut input_events_rows = scope.query::<&InputEvents>();

        // Input arrives as world resources: the world's own [`InputEvents`] is
        // both the state and what arrived for this tick. It is shared with
        // everything else in the world — the UI reads this tick's batch, it
        // does not take it. A world without one is not an error: the UI still
        // lays out at the target's own size, unfeed.
        let state = input_events_rows.iter().next().map(|input_events| {
            input_events.with_state(|state| (state.scale_factor, state.size_px, state.focused))
        });
        let (scale_factor, size_px, focused) = match state {
            Some(state) => state,
            None => (1.0, (target.width, target.height), true),
        };
        // A caller that has not learned the window's size yet leaves `size_px`
        // at zero; the target's own pixel size lays the UI out correctly, where
        // a zero-sized screen would lay out nothing.
        let size_px = if size_px == (0, 0) {
            (target.width, target.height)
        } else {
            size_px
        };
        // One physical pixel per logical point until something says otherwise.
        let pixels_per_point = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        // The batch is translated into this source's own list, which the frame
        // takes over and the next frame takes back: egui keeps the raw input
        // it was handed for the length of a pass, so the buffer is reclaimed
        // once the pass is over rather than allocated again.
        let mut event_scratch = core::mem::take(&mut self.event_scratch);
        event_scratch.clear();
        if let Some(input_events) = scope.query::<&InputEvents>().iter().next() {
            input_events.with_batch(|batch| {
                convert::to_egui_events_into(batch, pixels_per_point, &mut event_scratch);
            });
        }
        let screen = ScreenDescriptor {
            size_in_pixels: [size_px.0, size_px.1],
            pixels_per_point,
        };
        let target_screen = UiTarget {
            screen,
            max_texture_side: device.limits().max_texture_dimension_2d,
        };
        // Published before anything is laid out anywhere, and before this
        // frame's own input is built: a world laying a frame out reads it back,
        // so its layout is built against the same target this one draws at.
        self.screen.publish(target_screen);

        let index = self.target_index(&device, target.surface);

        // The events reached egui's own types while the world's `InputEvents`
        // were read, and they stay in the world afterwards: the UI reads them,
        // it does not consume them, so a game behaviour sees the same tick.
        let input = raw_input(
            target_screen,
            Some(self.start.elapsed().as_secs_f64()),
            focused,
            event_scratch,
        );

        // The URLs every frame asked to open are gathered here and published
        // in one go below, so a published frame's requests join this world's
        // own instead of overwriting them.
        let mut url_scratch = core::mem::take(&mut self.url_scratch);
        url_scratch.clear();
        self.frames.with_frames(|slots| {
            for slot in slots.iter().flatten() {
                url_scratch.extend(open_urls(&slot.platform_output));
            }
        });

        let mut output = self.run_panels(scope, input);
        // The pass is over, so egui no longer needs the list it was handed:
        // taking it back is what turns the next frame's fill into a reuse.
        self.event_scratch = self
            .ctx
            .input_mut(|input| core::mem::take(&mut input.raw.events));

        // A URL egui was asked to open is a request for the host rather than
        // the frame: a panel cannot open a browser tab itself, so the requests
        // of every frame drawn here are published in the world for whoever
        // owns the page to serve. Read before the integration takes the
        // output, because it consumes it.
        url_scratch.extend(open_urls(&output.platform_output));
        publish_open_urls(scope, &mut url_scratch);
        self.url_scratch = url_scratch;

        // Every source and this world's own go into one upload: the
        // integration packs one vertex and index buffer per frame, and the two
        // vertex streams meet at a single split point, so uploading them
        // separately would have each call overwrite the last.
        //
        // Each source keeps its own namespace. Two egui contexts both allocate
        // texture ids from zero, so the same id names different atlases in
        // different worlds; the integration keys textures by (source, id) and
        // the ids a frame arrived with are the ids it is drawn by. Nothing
        // rewrites them, so a frame held across many drawn frames is never
        // renumbered twice.
        //
        // One upload carries them all, which means one screen descriptor packs
        // every clip rectangle: a frame laid out at another size adds no
        // geometry, because its rectangles would be computed in the wrong
        // pixels. The world that published it lays a new one out at the size
        // this source now draws at.
        //
        // A published frame's geometry is borrowed from its slot rather than
        // copied — it is drawn again on every frame that brings no newer one —
        // so the gather borrows the slots for as long as the upload takes.
        let own = self.ctx.tessellate(output.shapes, screen.pixels_per_point);
        let own_textures = core::mem::take(&mut output.textures_delta);
        // What a frame contributes in geometry when its slice is left empty.
        const NO_PRIMITIVES: [egui::ClippedPrimitive; 0] = [];
        // The UI draws through a mapping of its own rather than through a
        // camera entity: `screen_view` maps egui's points onto clip space and
        // the pass's view is the same point size, so the frame's registry keys
        // both by the screen size. The frame globals are the frame's own — the
        // UI reads no clock — and were advanced before any source built.
        let points = screen.size_in_points();
        let mut graph_holder = ctx.graph(scope);
        let uniforms = {
            let mut uniforms_holder = ctx.uniforms(scope);
            uniforms_holder.get().uniforms(
                &device,
                graph_holder.get(),
                encoder,
                CameraKey::screen(points[0], points[1]),
                &screen_view(points),
                ViewKey::new(points[0], points[1]),
            )
        };
        // The group binding those records, built the first time this target
        // draws through them. A record that does not change reuses its group
        // from frame to frame; a screen that was resized names another key.
        let global = {
            let recipe = self.targets[index].gpu.recipe.clone();
            let groups = &mut self.targets[index].gpu.groups;
            let group = groups.group(
                graph_holder.get(),
                uniforms.key(),
                uniforms.frame,
                |graph| recipe.insert(&device, graph, &uniforms),
            );
            groups.prune(uniforms.frame);
            group
        };

        let gpu = &mut self.targets[index].gpu;
        self.frames.with_frames(|slots| {
            let mut sources = Vec::with_capacity(slots.len() + 1);
            sources.push(UiFrame {
                source: Source::LOCAL,
                textures: own_textures,
                primitives: &own,
            });
            for (index, slot) in slots.iter_mut().enumerate() {
                let Some(frame) = slot else {
                    continue;
                };
                // A frame laid out at another size is not drawn: its clip
                // rectangles were computed in the wrong pixels, and the world
                // that published it lays a new one out at the size this source
                // now draws at. Its textures still go through, because egui
                // sends a texture on the frame that first writes it and never
                // again — dropping the delta would lose that image for good,
                // leaving every later draw of the slot with nothing to sample.
                let primitives = if frame.screen == screen {
                    &frame.primitives[..]
                } else {
                    &NO_PRIMITIVES[..]
                };
                sources.push(UiFrame {
                    source: Source::published(index),
                    textures: core::mem::take(&mut frame.textures),
                    primitives,
                });
            }
            gpu.integration
                .update_with(graph_holder.get(), &queue, encoder, &mut sources, screen);
            // The deltas were applied and cleared; a frame's geometry stayed
            // where it was, so the slot still holds what it will draw again.
        });

        // The frame's one rebuild pass: the textures the integration just
        // uploaded marked their readers dirty, and the scene assembled below
        // reads the rebuilt groups. The graph borrow is given up first, because
        // the scope takes it again to run the pass.
        drop(graph_holder);
        drop(maintain);
        let mut graph_holder = ctx.graph(scope);
        let graph = graph_holder.get();

        let bind_group = graph.get(&global).expect("the group exists");
        let mut ui_scene = gpu.integration.scene(graph, bind_group);
        scene.draws.append(&mut ui_scene.draws);
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::UI
    }
}

/// The URLs a frame asked the host to open.
///
/// Reading them is all this does; a frame's requests join every other frame's
/// before they reach the world, so one frame's list cannot drop another's.
fn open_urls(output: &egui::PlatformOutput) -> impl Iterator<Item = OpenUrl> + '_ {
    output.commands.iter().filter_map(|command| match command {
        egui::OutputCommand::OpenUrl(open) => Some(OpenUrl {
            url: open.url.clone(),
            new_tab: open.new_tab,
        }),
        // Copying text or an image is the integration's to serve on the
        // platform it runs on; only a URL needs something a frame source is
        // not: a page to open it in.
        _ => None,
    })
}

/// Publish the URLs egui was asked to open as an [`OpenUrls`] resource.
///
/// The resource is looked up by type, so a world gets one from `spawn_context`
/// — the frame's own setup — and a caller never has to create it. A world
/// without one is not an error: the UI still draws, and the request is simply
/// dropped. The queue is replaced rather than appended to, so a request that
/// a previous frame left behind cannot be served twice; `urls` is emptied
/// either way, so its allocation is reused by the next frame.
fn publish_open_urls(scope: &Scope<'_>, urls: &mut Vec<OpenUrl>) {
    let Some(entity) = ({
        let mut open_urls_rows = scope.query::<(&OpenUrls, Entity)>();
        open_urls_rows.iter().next().map(|(_open_urls, e)| e)
    }) else {
        urls.clear();
        return;
    };
    let _ = scope.query_one::<&mut OpenUrls>(entity).map(|mut slot| {
        let slot = slot.get();
        slot.replace(urls.drain(..))
    });
}

/// A frame of UI that was laid out somewhere other than the world drawing it.
///
/// A [`UiPanel`] callback takes a `&mut egui::Ui`, which belongs to the egui
/// pass running it, so a panel can only be built where its own context lives.
/// A UI that lives in one world and is drawn in another therefore splits the
/// work: the world holding the panels lays them out and tessellates them —
/// [`lay_out`] does that — and the world holding the [`UiSource`] uploads and
/// draws what comes back.
///
/// Everything here is plain owned data, which is what lets it cross the
/// boundary: it carries neither egui's context nor any borrow of it.
pub struct ExternalFrame {
    /// The frame's texture updates, keyed by the context that laid it out.
    pub textures: egui::TexturesDelta,
    /// The frame's tessellated geometry.
    pub primitives: Vec<egui::ClippedPrimitive>,
    /// What the frame asked the platform to do, such as a URL to open.
    pub platform_output: egui::PlatformOutput,
    /// How long after the pass the frame asked to be laid out again, where
    /// [Duration::MAX] is a frame that asked for nothing.
    ///
    /// A panel running an animation or a field being edited keeps asking, and
    /// the delay is how egui paces those requests: zero asks for the next
    /// possible pass, a finite non-zero value names a moment, and the largest
    /// value means the picture is settled. It travels with the frame because
    /// the pass that produced it is the only chance to read it — egui clears
    /// the request as the next pass begins — so the world laying frames out
    /// turns it into a deadline of its own.
    pub repaint_delay: Duration,
    /// The size and density the frame was laid out at.
    ///
    /// The drawing side packs this frame's clip rectangles into pixels with
    /// it, so it has to be the descriptor the frame was built against: the
    /// laying-out side reads the target the drawing side publishes rather than
    /// deriving one, and this is that descriptor travelling with the frame it
    /// describes.
    pub screen: ScreenDescriptor,
}

impl Drop for ExternalFrame {
    fn drop(&mut self) {
        // egui panics when a texture delta is dropped unapplied, and a frame
        // that crosses worlds can legitimately be dropped: a publisher that
        // replaced an older frame, or a drawing side that dropped the frame of
        // a slot whose publisher is gone. What it carries was never uploaded,
        // so the deltas are simply forgotten.
        self.textures.clear();
    }
}

/// The slots frames are published into, drawn by a [`UiSource`].
///
/// Each slot holds the newest frame published to it. The frames belong to the
/// source's own world: a job sent to that world writes a slot, and this map is
/// what keeps it across frames. A slot that has not been written since the
/// last frame draws the frame it already holds, so a settled UI costs nothing
/// on the wire.
///
/// The slots are numbered rather than named so a publisher needs no identity:
/// whoever mounts the publishing side picks the slot and hands the number to
/// the world that lays the frames out.
#[derive(Default)]
pub struct ExternalFrames(Vec<Option<ExternalFrame>>);

impl ExternalFrames {
    /// A map with no slots.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish `frame` into `slot`.
    ///
    /// The frame replaces whatever the slot held, which is what the next
    /// frame drawn will show. Slots are grown on demand, so a publisher may
    /// write a slot number the drawing side has never looked at.
    ///
    /// A frame displaced before it was drawn hands its texture updates to its
    /// successor, because egui sends a texture once — on the frame that first
    /// writes it — while every later frame of the same context carries none.
    /// Dropping the displaced frame outright would therefore drop the update
    /// that makes the slot's textures exist, and every draw of the successor
    /// would find no material to bind: a publisher that lays frames out faster
    /// than they are drawn would never put anything on screen. A frame that
    /// *was* drawn has already had its updates taken, so a slot keeping up with
    /// its publisher carries nothing.
    pub fn publish(&mut self, slot: usize, mut frame: ExternalFrame) {
        if self.0.len() <= slot {
            self.0.resize_with(slot + 1, || None);
        }
        if let Some(displaced) = self.0[slot].as_mut() {
            let mut carried = core::mem::take(&mut displaced.textures);
            carried.append(core::mem::take(&mut frame.textures));
            frame.textures = carried;
        }
        self.0[slot] = Some(frame);
    }

    /// Run `f` over every slot's frame, in slot order.
    ///
    /// The frames stay where they are: drawing one does not consume it, so a
    /// slot published once is drawn until it is replaced.
    pub fn with_frames<R>(&mut self, f: impl FnOnce(&mut [Option<ExternalFrame>]) -> R) -> R {
        f(&mut self.0)
    }
}

/// What a frame has to be laid out against, published by the drawing side.
///
/// A frame's clip rectangles are packed into physical pixels while its
/// geometry and projection are in logical points, so the two sides have to
/// agree on one descriptor: the drawing side knows the target's real size, so
/// it writes this and the laying-out side reads it instead of deriving one of
/// its own.
///
/// The texture limit travels with it because it is the same kind of fact: the
/// drawing side holds the device that will upload egui's atlases, and the
/// laying-out side decides how large they may grow. A world laying frames out
/// has no device of its own to ask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiTarget {
    /// The target the frame is drawn into, and the density it is drawn at.
    pub screen: ScreenDescriptor,
    /// The largest texture the drawing side's device can hold, on either side.
    pub max_texture_side: u32,
}

/// The egui input for one frame laid out against `target`.
///
/// Both worlds lay frames out — the drawing side its own panels, every other
/// world the frames it publishes — and egui only produces the same picture for
/// the same input. Building it in one place is what keeps a published frame's
/// capture and the drawing side's own layout agreeing on the screen rectangle,
/// the density and the texture limit.
///
/// `events` is taken by value so a caller can hand over a buffer it keeps
/// across frames; it reaches egui as this frame's events.
pub fn raw_input(
    target: UiTarget,
    time: Option<f64>,
    focused: bool,
    events: Vec<egui::Event>,
) -> egui::RawInput {
    let [width, height] = target.screen.size_in_points();
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(width, height),
        )),
        time,
        focused,
        events,
        max_texture_side: Some(target.max_texture_side as usize),
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&input.viewport_id)
        .expect("the root viewport is always present")
        .native_pixels_per_point = Some(target.screen.pixels_per_point);
    input
}

/// Lay out the world's panels and tessellate them, without drawing them.
///
/// The caller owns `ctx` — its font atlas, memory and style — and keeps it
/// across frames; this runs the world's [`UiPanel`]s against it and returns
/// what a [`UiSource`] would have drawn.
///
/// `screen` is the descriptor the frame is laid out at, which travels with
/// the frame: the drawing side packs its clip rectangles with that descriptor
/// rather than with one of its own, so a frame is drawn exactly as it was laid
/// out. Take it from the target the drawing side publishes to work at the size
/// it is about to draw at.
pub fn lay_out(
    world: &World,
    ctx: &egui::Context,
    input: egui::RawInput,
    screen: ScreenDescriptor,
) -> ExternalFrame {
    let scope = world.root_scope();
    let mut output = ctx.run_ui(input, |ui| {
        for (ui_panel, entity) in scope.query::<(&mut UiPanel, Entity)>().iter() {
            ui_panel.run(scope, entity, ui);
        }
    });
    // Read before the pass's other output is taken apart: the request only
    // exists for this pass, and egui clears it as the next one begins.
    let repaint_delay = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(Duration::MAX, |viewport| viewport.repaint_delay);
    let primitives = ctx.tessellate(output.shapes, screen.pixels_per_point);
    let textures = core::mem::take(&mut output.textures_delta);
    ExternalFrame {
        textures,
        primitives,
        platform_output: output.platform_output,
        repaint_delay,
        screen,
    }
}

#[cfg(test)]
mod tests;
