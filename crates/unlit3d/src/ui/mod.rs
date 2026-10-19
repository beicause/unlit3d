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
//! let ui = world.root_view_mut().spawn((Source::new(UiSource::new()),));
//! world.view_mut(ui).unwrap().spawn((
//!     UiPanel::new(|_world, _entity, ui| {
//!         ui.label("hello");
//!     }),
//! ));
//! ```
//!
//! The UI draws with the same built-in unlit pipeline as everything else, but
//! through its own camera uniform: one pass carries one camera value, so the UI
//! cannot borrow the 3D camera's. The source therefore owns a camera and a
//! frame-globals buffer of its own, plus the bind group binding them.

use core::ops::DerefMut;
use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};

use unlit_ecs::{Entity, ScopedWorld, World};
use unlit_wgpu::globals::{Globals, View};
use unlit_wgpu::pipeline::{CAMERA_BINDING, FRAME_BINDING, SpecializedUnlitPipeline};
use unlit_wgpu::resources::{ResHandle, ResourceGraph};
use unlit_wgpu::scene::Scene;
use unlit_wgpu::specialize::SpecializedPipeline;
use unlit_wgpu::specialize::SurfaceKey;
use unlit_wgpu::ui::{EguiIntegration, ScreenDescriptor, Source, UiFrame, screen_view, ui_variant};
// `std::time::Instant` panics on `wasm32-unknown-unknown`, where the standard
// library has no clock; `web-time` reads the browser's `Performance.now()`
// there and re-exports `std::time` everywhere else.
use web_time::Instant;
use zerocopy::IntoBytes;

pub use crate::source::{OpenUrl, OpenUrls};

use crate::input::InputEvents;

use crate::source::{FrameSource, FrameSourceOrder, RenderContext, frame_target};
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
type PanelCallback = Box<dyn FnMut(ScopedWorld<'_>, Entity, &mut egui::Ui)>;

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
/// [`ScopedWorld`] it is handed: `get`, `query` and structural changes queued
/// for the frame loop to apply are all available while it runs. The view is the
/// one of the source that draws the panel, so a panel sees the subtree that
/// source owns and the shared resources, not every entity in the world.
pub struct UiPanel(pub PanelCallback);

impl UiPanel {
    /// Wrap `f` as a [`UiPanel`].
    pub fn new(f: impl FnMut(ScopedWorld<'_>, Entity, &mut egui::Ui) + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Run this panel for `entity`, building its interface in `ui`.
    pub fn run(&mut self, view: ScopedWorld<'_>, entity: Entity, ui: &mut egui::Ui) {
        (self.0)(view, entity, ui)
    }
}

/// The GPU state of a [`UiSource`], built for one [`SurfaceKey`].
///
/// The uniform buffers outlive a target change — nothing about them depends on
/// the target — so only the integration (which holds the specialized pipeline)
/// is rebuilt in place when the target's key changes.
struct Gpu {
    /// The camera uniform `screen_view` is written into.
    camera: wgpu::Buffer,
    /// The frame-globals uniform.
    globals: wgpu::Buffer,
    /// Graph node of the bind group binding the two uniforms.
    ///
    /// Holding it is what keeps the two uniforms alive: the group depends on
    /// them, and dropping the whole [`Gpu`] gives up the group with it.
    global_group: ResHandle<wgpu::BindGroup>,
    /// Uploads egui's textures and geometry and records its draws.
    integration: EguiIntegration,
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
/// [`FrameSource::build_scene`]. The uniforms, pipeline and bind group are
/// built then, and rebuilt when the target's [`SurfaceKey`] changes.
///
/// The UI has its own camera and globals uniforms rather than reusing the 3D
/// ones: a single render pass can carry only one camera value, and the UI's
/// projection maps logical points onto clip space while the 3D camera maps
/// world space. Both buffers are owned by this source and registered in the
/// frame's resource graph.
pub struct UiSource {
    /// egui's state — fonts, memory, style — kept across frames.
    ctx: egui::Context,
    /// When the source was created, the origin of egui's frame clock.
    start: Instant,
    /// This frame's UI draws, reused across frames.
    scene: Scene,
    /// The GPU state, built on the first frame that knows its target.
    gpu: Option<Gpu>,
    /// The events translated for the frame being laid out, reclaimed from egui
    /// once its pass is over so the next frame reuses the allocation.
    event_scratch: Vec<egui::Event>,
    /// The target the current pipeline is specialized for.
    surface: Option<SurfaceKey>,
    /// The slots other worlds publish their frames into, still drawn until a
    /// newer frame arrives.
    external: ExternalFrames,
    /// The size and density this source draws at, published for the worlds
    /// laying frames out.
    screen: UiScreen,
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
            scene: Scene::new(),
            gpu: None,
            event_scratch: Vec::new(),
            surface: None,
            external: ExternalFrames::new(),
            screen: UiScreen::new(),
            url_scratch: Vec::new(),
        }
    }

    /// Draw the frames other worlds publish, as well as this world's own panels.
    ///
    /// This is the seam for a UI that lives in another world: a panel can only
    /// be built where its egui context runs, so the world holding it lays the
    /// frame out and publishes it into a slot of `frames`, and the source
    /// draws whatever is there. Slots are drawn in order, under this world's
    /// own panels, and a slot's frame is drawn again until a newer one
    /// replaces it.
    ///
    /// `screen` is the size and density the frame is drawn at, published by
    /// this source for the worlds that lay frames out: a frame laid out at one
    /// size and drawn at another would have its clip rectangles computed in
    /// the wrong pixels, so the laying-out side reads the descriptor back from
    /// here rather than deriving one of its own.
    pub fn draw_published(&mut self, frames: ExternalFrames, screen: UiScreen) -> &mut Self {
        self.external = frames;
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

    /// The resource graph `ctx` addresses in `world`.
    ///
    /// An associated function rather than a method, matching
    /// [`MeshSource::graph`](crate::mesh_source::MeshSource::graph), so the borrow it takes
    /// is visibly disjoint from the `&mut self` fields a caller splits
    /// alongside it.
    ///
    /// # Panics
    ///
    /// If the context's graph resource is gone.
    fn graph<'w>(
        view: &'w ScopedWorld<'w>,
        ctx: RenderContext,
    ) -> impl DerefMut<Target = ResourceGraph> + 'w {
        view.get_mut::<ResourceGraph>(ctx.graph)
            .expect("the context's resource graph exists")
    }

    /// Run every entity that carries a [`UiPanel`], in query order.
    ///
    /// The query is rebuilt from scratch on every pass egui runs, which is what
    /// a multi-pass layout needs.
    fn run_panels(&self, view: &ScopedWorld<'_>, input: egui::RawInput) -> egui::FullOutput {
        let root = *view;
        self.ctx.run_ui(input, |ui| {
            for (entity, mut panel) in root.query::<&mut UiPanel>() {
                panel.run(root, entity, ui);
            }
        })
    }

    /// Build the UI's pipeline and bind group for `surface`, or rebuild the
    /// target-dependent part in place.
    ///
    /// The uniforms are created once; a later target only changes the pipeline,
    /// so its bind group is replaced behind the same graph node and the node's
    /// id stays valid for the [`EguiIntegration`] built around it.
    fn build_gpu(&mut self, device: &wgpu::Device, graph: &mut ResourceGraph, surface: SurfaceKey) {
        // A UI drawn into an sRGB target encodes its own output in gamma space,
        // so the fragment converts to linear; the same call the winit path
        // makes for its swap chain.
        let variant = ui_variant(device, surface.color_format.is_srgb(), surface);
        let pipeline = SpecializedPipeline::create(device, variant);

        match &mut self.gpu {
            Some(gpu) => {
                let group = global_group(device, &pipeline, &gpu.camera, &gpu.globals);
                graph
                    .replace(&gpu.global_group, group)
                    .expect("the global group is registered in the graph");
                // Same uniforms, same group node, same integration state; the
                // new pipeline is all that actually changed.
                gpu.integration.retarget(pipeline);
            }
            None => {
                let camera = uniform_buffer(
                    device,
                    "unlit3d::ui::camera",
                    <View as const_shader_layout::ShaderLayout>::SIZE.get(),
                );
                let globals = uniform_buffer(
                    device,
                    "unlit3d::ui::globals",
                    <Globals as const_shader_layout::ShaderLayout>::SIZE.get(),
                );
                let camera_id = graph.insert(camera.clone(), None);
                let globals_id = graph.insert(globals.clone(), None);
                // The two uniforms are written into every frame rather than
                // replaced, so the group has nothing to rebuild from: the only
                // change that reaches it is the pipeline swap above, which
                // replaces it outright.
                let group = global_group(device, &pipeline, &camera, &globals);
                let group_id = graph.insert(group, None);
                graph.add_dependency(&group_id, &camera_id);
                graph.add_dependency(&group_id, &globals_id);
                self.gpu = Some(Gpu {
                    camera,
                    globals,
                    global_group: group_id.clone(),
                    integration: EguiIntegration::new(device, group_id, pipeline),
                });
            }
        }
        self.surface = Some(surface);
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
        view: &ScopedWorld<'_>,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Settle the graph on every way out of this build, the early return
        // below included. The pass runs where this scope ends, once the
        // integration has uploaded and before the scene reads a group.
        let maintain = ctx.maintain_scope(*view);
        // Cleared on every path: a frame that records this source must never
        // replay the previous frame's UI.
        self.scene.clear();

        // The target the frame is specialized for, written by the frame loop
        // before it renders. Without it the pipeline cannot be specialized, and
        // a stale one is worse than none.
        let Some(target) = frame_target(view) else {
            log::warn!(
                "UiSource::build_scene: no FrameTarget in the world, so the \
                 frame's render target is unknown and no UI is drawn; write \
                 one by binding a render target before rendering"
            );
            return;
        };

        let device = view
            .get::<wgpu::Device>(ctx.device)
            .expect("the context's device resource exists")
            .clone();
        let queue = view
            .get::<wgpu::Queue>(ctx.queue)
            .expect("the context's queue resource exists")
            .clone();

        // Input arrives as world resources: the world's own [`InputEvents`] is
        // both the state and what arrived for this tick. It is shared with
        // everything else in the world — the UI reads this tick's batch, it
        // does not take it. A world without one is not an error: the UI still
        // lays out at the target's own size, unfeed.
        let state = view.query::<&InputEvents>().next().map(|(_, events)| {
            events.with_state(|state| (state.scale_factor, state.size_px, state.focused))
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
        if let Some((_, events)) = view.query::<&InputEvents>().next() {
            events.with_batch(|batch| {
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

        let mut graph = Self::graph(view, ctx);
        if self.gpu.is_none() || self.surface != Some(target.surface) {
            self.build_gpu(&device, &mut graph, target.surface);
        }

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
        self.external.with_frames(|slots| {
            for slot in slots.iter().flatten() {
                url_scratch.extend(open_urls(&slot.platform_output));
            }
        });

        let mut output = self.run_panels(view, input);
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
        publish_open_urls(view, &mut url_scratch);
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
        // every clip rectangle: a frame laid out at another size is dropped
        // instead, because its rectangles would be computed in the wrong
        // pixels. The world that published it lays a new one out at the size
        // this source now draws at.
        //
        // A published frame's geometry is borrowed from its slot rather than
        // copied — it is drawn again on every frame that brings no newer one —
        // so the gather runs under the slots' lock and the upload happens
        // inside it.
        let own = self.ctx.tessellate(output.shapes, screen.pixels_per_point);
        let own_textures = core::mem::take(&mut output.textures_delta);
        let gpu = self.gpu.as_mut().expect("built above");
        self.external.with_frames(|slots| {
            let mut sources = Vec::with_capacity(slots.len() + 1);
            sources.push(UiFrame {
                source: Source::LOCAL,
                textures: own_textures,
                primitives: &own,
            });
            for (index, slot) in slots.iter_mut().enumerate() {
                // A frame laid out at another size is dropped rather than
                // drawn: its clip rectangles were computed in the wrong
                // pixels, and the world that published it lays a new one out
                // at the size this source now draws at.
                if slot.as_ref().is_some_and(|frame| frame.screen != screen) {
                    *slot = None;
                }
                let Some(frame) = slot else {
                    continue;
                };
                sources.push(UiFrame {
                    source: Source::published(index),
                    textures: core::mem::take(&mut frame.textures),
                    primitives: &frame.primitives,
                });
            }
            gpu.integration
                .update_with(&mut graph, &queue, encoder, &mut sources, screen);
            // The deltas were applied and cleared; a frame's geometry stayed
            // where it was, so the slot still holds what it will draw again.
        });

        // The UI's own camera: `screen_view` maps egui's points onto clip
        // space, and its frame globals stay at their defaults — nothing the UI
        // draws reads the frame clock.
        queue.write_buffer(
            &gpu.camera,
            0,
            screen_view(screen.size_in_points()).as_bytes(),
        );
        queue.write_buffer(&gpu.globals, 0, Globals::default().as_bytes());

        // The frame's one rebuild pass: the uniform writes and the textures the
        // integration just uploaded marked their readers dirty, and the scene
        // assembled below reads the rebuilt groups. The graph borrow is given
        // up first, because the scope takes it again to run the pass.
        drop(graph);
        drop(maintain);
        let graph = Self::graph(view, ctx);

        let mut ui_scene = gpu.integration.scene(&graph);
        self.scene.extend(&mut ui_scene);
        self.surface = Some(target.surface);
    }

    fn scene(&self) -> &Scene {
        &self.scene
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
fn publish_open_urls(view: &ScopedWorld<'_>, urls: &mut Vec<OpenUrl>) {
    let Some(entity) = view.query::<&OpenUrls>().next().map(|(e, _)| e) else {
        urls.clear();
        return;
    };
    let _ = view.with_mut::<OpenUrls, _>(entity, |slot| slot.replace(urls.drain(..)));
}

/// A uniform buffer of `size` bytes, written through the queue.
fn uniform_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// The UI's global bind group: its camera and frame-globals uniforms.
///
/// Built from the pipeline's own global layout, so the group and the pipeline
/// it feeds are always specialized for the same target.
fn global_group(
    device: &wgpu::Device,
    pipeline: &SpecializedUnlitPipeline,
    camera: &wgpu::Buffer,
    globals: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("unlit3d::ui::globals"),
        layout: &pipeline.descriptor().bind_group_layouts(device).global,
        entries: &[
            wgpu::BindGroupEntry {
                binding: CAMERA_BINDING,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: FRAME_BINDING,
                resource: globals.as_entire_binding(),
            },
        ],
    })
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
    /// laying-out side reads [`UiScreen`] rather than deriving one, and this
    /// is that descriptor travelling with the frame it describes.
    pub screen: ScreenDescriptor,
}

impl Drop for ExternalFrame {
    fn drop(&mut self) {
        // egui panics when a texture delta is dropped unapplied, and a frame
        // that crosses worlds can legitimately be dropped: a publisher that
        // replaced an older frame, or a drawing side that dropped one laid out
        // at another target. What it carries was never uploaded, so the
        // deltas are simply forgotten.
        self.textures.clear();
    }
}

/// The slots worlds publish frames into, drawn by a [`UiSource`] elsewhere.
///
/// Each slot holds the newest frame its world published. A world holds a clone
/// and writes its own slot when it has a new frame; the drawing side holds the
/// same map and draws every slot's frame. Nothing is asked for in return: a
/// slot that has not been written since the last frame draws the frame it
/// already holds, so a settled UI costs nothing on the wire.
///
/// The slots are numbered rather than named so a world needs no identity to
/// publish under: whoever mounts the publishing side picks the slot and hands
/// the number to the world that lays the frames out.
#[derive(Clone, Default)]
pub struct ExternalFrames(Arc<Mutex<Vec<Option<ExternalFrame>>>>);

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
    /// write a slot number before the drawing side has looked at any.
    ///
    /// The displaced frame is dropped here, and a frame that is dropped
    /// without being drawn clears its own texture deltas.
    pub fn publish(&self, slot: usize, frame: ExternalFrame) {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slots.len() <= slot {
            slots.resize_with(slot + 1, || None);
        }
        slots[slot] = Some(frame);
    }

    /// Run `f` over every slot's frame, in slot order.
    ///
    /// The frames stay where they are: drawing one does not consume it, so a
    /// slot published once is drawn until it is replaced.
    pub fn with_frames<R>(&self, f: impl FnOnce(&mut [Option<ExternalFrame>]) -> R) -> R {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut slots)
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

/// The target frames are laid out against, published by the drawing side.
///
/// The drawing side publishes it on every frame it draws, and the world laying
/// frames out reads it back, so a resize or a density change reaches the
/// layout one frame later rather than never.
#[derive(Clone, Default)]
pub struct UiScreen(Arc<Mutex<Option<UiTarget>>>);

impl UiScreen {
    /// A screen nothing has been drawn at yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the target the next frames are drawn at.
    pub fn publish(&self, target: UiTarget) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(target);
    }

    /// The target to lay a frame out against, with `fallback` if nothing was
    /// ever published.
    ///
    /// A world that lays frames out before anything has been drawn has no
    /// target to match yet, so it lays out at the size it was given and the
    /// drawing side picks that frame up only once the sizes agree.
    #[must_use]
    pub fn current(&self, fallback: UiTarget) -> UiTarget {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or(fallback)
    }
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
/// out. Read it from [`UiScreen`] to work at the size the drawing side is
/// about to draw at.
pub fn lay_out(
    world: &World,
    ctx: &egui::Context,
    input: egui::RawInput,
    screen: ScreenDescriptor,
) -> ExternalFrame {
    let view = world.root_view();
    let mut output = ctx.run_ui(input, |ui| {
        for (entity, mut panel) in view.query::<&mut UiPanel>() {
            panel.run(view, entity, ui);
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
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::rc::Rc;
    use unlit_wgpu::capabilities::DeviceCapabilities;

    use crate::input::{InputEvent, InputWriter, Modifiers, MouseButton, MouseEvent, TextEvent};
    use crate::source::{FrameTarget, set_frame_target, spawn_context};

    /// How many times a panel's closure ran, in a sibling component: a panel
    /// cannot keep state in itself.
    struct Runs(u32);

    /// The frame target the tests draw into.
    ///
    /// Nothing here records a pass, so only the target's key and size matter —
    /// no attachment has to exist behind them.
    /// The target a frame is laid out against in these tests: the same size
    /// the source was built for, so its clip rectangles are packed in pixels
    /// that agree.
    fn test_ui_target() -> UiTarget {
        UiTarget {
            screen: ScreenDescriptor {
                size_in_pixels: [128, 96],
                pixels_per_point: 1.0,
            },
            max_texture_side: 2048,
        }
    }

    fn test_target() -> FrameTarget {
        FrameTarget {
            surface: SurfaceKey {
                color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth_stencil_format: None,
                sample_count: 1,
            },
            width: 128,
            height: 96,
        }
    }

    /// A world holding the frame's context, and that context.
    fn test_world() -> (World, RenderContext) {
        let mut world = World::new();
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let ctx = spawn_context(
            &mut world,
            device,
            queue,
            ResourceGraph::new(),
            DeviceCapabilities::default(),
        );
        (world, ctx)
    }

    /// An encoder to record the frame's uploads into.
    fn encoder(world: &World, ctx: RenderContext) -> wgpu::CommandEncoder {
        world
            .root_view()
            .get::<wgpu::Device>(ctx.device)
            .expect("the context's device")
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("test::encoder"),
            })
    }

    /// Every panel in the world runs, once, and a panel may read and write a
    /// sibling component while it does.
    #[test]
    fn each_panel_runs_once_and_may_write_a_sibling() {
        let mut world = World::new();
        let runs = Rc::new(Cell::new(0u32));
        for _ in 0..2 {
            let runs = Rc::clone(&runs);
            world.root_view_mut().spawn((
                Runs(0),
                UiPanel::new(move |world, entity, ui| {
                    runs.set(runs.get() + 1);
                    // Read the sibling, then write it back advanced by one.
                    let seen = world.get::<Runs>(entity).expect("the sibling exists").0;
                    let _ = world.with_mut::<Runs, _>(entity, |runs| runs.0 = seen + 1);
                    ui.label("panel");
                }),
            ));
        }

        let source = UiSource::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(128.0, 96.0),
            )),
            ..Default::default()
        };
        // egui panics if a texture delta is dropped unapplied; a real frame
        // hands it to the integration, and this test discards it explicitly.
        let mut output = source.run_panels(&world.root_view(), input);
        output.textures_delta.clear();

        let panels: Vec<Entity> = world
            .root_view()
            .query::<&UiPanel>()
            .map(|(entity, _)| entity)
            .collect();
        assert_eq!(panels.len(), 2, "both panels are driven by one query");
        for entity in panels {
            assert_eq!(
                world
                    .root_view()
                    .get::<Runs>(entity)
                    .expect("the sibling")
                    .0,
                1,
                "the panel ran once and wrote its sibling"
            );
        }
        assert_eq!(runs.get(), 2, "one closure call per panel");
    }

    /// A panel is driven by the source: the source builds its pipeline and
    /// turns what the panel painted into draws.
    #[test]
    fn a_panel_is_driven_by_the_source() {
        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world.root_view(), test_target()));
        world
            .root_view_mut()
            .spawn((UiPanel::new(|_world, _entity, ui| {
                // A plain rectangle, so the frame's contents do not depend on the
                // font atlas being ready on the first pass.
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::Pos2::new(8.0, 8.0),
                        egui::Vec2::new(64.0, 32.0),
                    ),
                    0.0,
                    egui::Color32::from_rgb(0, 255, 0),
                );
            }),));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world.root_view(), ctx, &mut encoder);

        assert!(
            !source.scene().is_empty(),
            "the panel's rectangle became a draw"
        );
    }

    /// Without a frame target the source warns and draws nothing, rather than
    /// specializing on a target it cannot know.
    #[test]
    fn a_missing_frame_target_draws_nothing() {
        let (mut world, ctx) = test_world();
        world
            .root_view_mut()
            .spawn((UiPanel::new(|_world, _entity, ui| {
                ui.label("not drawn");
            }),));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world.root_view(), ctx, &mut encoder);

        assert!(source.scene().is_empty());
        assert!(source.gpu.is_none(), "no target was specialized for");
    }

    /// The source is constructed with no arguments and records over the meshes.
    #[test]
    fn a_fresh_source_declares_the_overlay_order() {
        let source = UiSource::new();
        assert_eq!(source.order(), FrameSourceOrder::UI);
    }

    /// A panel's queued spawn lands once the frame loop applies the queue.
    ///
    /// A panel gets only a shared world, so a structural change is queued —
    /// the same rule every other behaviour component follows.
    #[test]
    fn a_panel_may_queue_a_spawn() {
        /// The entity a panel asks for.
        struct Spawned;

        let world = World::new();
        let source = UiSource::new();
        let world = Rc::new(RefCell::new(world));
        {
            let world = Rc::clone(&world);
            world
                .borrow_mut()
                .root_view_mut()
                .spawn((UiPanel::new(move |world, _entity, ui| {
                    if world.query::<&Spawned>().next().is_none() {
                        world.queue().spawn((Spawned,));
                    }
                    ui.label("once");
                }),));
        }

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(128.0, 96.0),
            )),
            ..Default::default()
        };
        // The panel only queues; nothing exists until the caller applies.
        let mut output = {
            let world = world.borrow();
            source.run_panels(&world.root_view(), input.clone())
        };
        output.textures_delta.clear();
        assert_eq!(
            world.borrow().root_view().query::<&Spawned>().count(),
            0,
            "a queued spawn has not happened yet"
        );

        world.borrow_mut().apply();
        assert_eq!(
            world.borrow().root_view().query::<&Spawned>().count(),
            1,
            "the queue landed on apply"
        );

        // A second frame sees the entity and does not queue another.
        let mut output = source.run_panels(&world.borrow().root_view(), input);
        output.textures_delta.clear();
        world.borrow_mut().apply();
        assert_eq!(
            world.borrow().root_view().query::<&Spawned>().count(),
            1,
            "the panel re-ran and found what it asked for"
        );
    }

    /// Every pass of a multi-pass layout drives every panel.
    ///
    /// egui calls the closure again when a pass asks for a discard, so the
    /// query must be rebuilt per pass rather than iterated once: collecting
    /// the entities up front would make the second pass see nothing.
    #[test]
    fn every_pass_drives_every_panel() {
        let mut world = World::new();
        let runs = Rc::new(Cell::new(0u32));
        for _ in 0..2 {
            let runs = Rc::clone(&runs);
            world
                .root_view_mut()
                .spawn((UiPanel::new(move |_world, _entity, _ui| {
                    runs.set(runs.get() + 1);
                }),));
        }

        let source = UiSource::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(128.0, 96.0),
            )),
            ..Default::default()
        };
        // Run the driving closure twice by hand, as a discard does, and count
        // what each pass saw.
        let mut seen = Vec::new();
        for _ in 0..2 {
            let mut output = source.run_panels(&world.root_view(), input.clone());
            output.textures_delta.clear();
            seen.push(runs.get());
        }

        assert_eq!(
            seen,
            vec![2, 4],
            "each pass drove both panels again, so the query was rebuilt"
        );
    }

    /// A caller can select a subset of the panels by filtering.
    ///
    /// Panels are ordinary components, so a second kind of panel is a second
    /// component and a caller can be selective without the source knowing
    /// either type. A query filter fetches no column, so selecting costs no
    /// borrow.
    #[test]
    fn a_filter_selects_which_panels_a_caller_collects() {
        use unlit_ecs::With;

        /// Marks a panel as belonging to one layer.
        struct Foreground;

        let mut world = World::new();
        world
            .root_view_mut()
            .spawn((Foreground, UiPanel::new(|_world, _entity, _ui| {})));
        world
            .root_view_mut()
            .spawn((UiPanel::new(|_world, _entity, _ui| {}),));

        let foreground: Vec<Entity> = world
            .root_view()
            .query_filtered::<Entity, With<Foreground>>()
            .map(|(entity, _)| entity)
            .collect();
        assert_eq!(foreground.len(), 1, "the filter picked one of two panels");

        let all: Vec<Entity> = world
            .root_view()
            .query::<&UiPanel>()
            .map(|(e, _)| e)
            .collect();
        assert_eq!(all.len(), 2, "the unfiltered query sees both");
        assert!(
            all.contains(&foreground[0]),
            "the filtered panel is one of them"
        );
    }

    /// The frame's events reach the panels.
    ///
    /// A panel that records the events egui gave it proves the world's
    /// `InputEvents` were read and translated, not merely looked up.
    #[test]
    fn the_frame_events_reach_the_panels() {
        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world.root_view(), test_target()));

        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&seen);
        world
            .root_view_mut()
            .spawn((UiPanel::new(move |_world, _entity, ui| {
                // egui's own input state is what a widget reads; the raw events are
                // not exposed to a panel, so this asserts on the effect instead: a
                // typed character must land in the text a focused field sees.
                ui.text_edit_singleline(&mut String::new());
                let pressed = ui.input(|input| {
                    input
                        .events
                        .iter()
                        .filter_map(|event| match event {
                            egui::Event::Text(text) => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                });
                recorded.borrow_mut().extend(pressed);
            }),));

        // The pointer and a typed key arrive in the world's own event types.
        let input = InputWriter::new();
        let events = input.subscribe();
        world.root_view_mut().spawn((input.clone(), events.clone()));
        input.push(InputEvent::Resized { size_px: (128, 96) });
        events.refresh();

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        // egui only lays a text field out once it knows its fonts, so the
        // first frame establishes them; the text is then this tick's batch.
        source.build_scene(&world.root_view(), ctx, &mut encoder);
        input.push(InputEvent::Text(TextEvent("hi".to_string())));
        events.refresh();
        source.build_scene(&world.root_view(), ctx, &mut encoder);

        assert_eq!(
            &*seen.borrow(),
            &["hi".to_string()],
            "the frame's text event reached the panel's egui input, once"
        );
    }

    /// A mouse click becomes a click a panel can see.
    ///
    /// The whole chain is exercised: the world's event, its translation, and
    /// egui's own hit testing on a widget the panel painted.
    #[test]
    fn a_mouse_click_reaches_a_widget() {
        /// Where the button is, in points.
        fn button() -> egui::Rect {
            egui::Rect::from_min_size(egui::Pos2::new(8.0, 8.0), egui::Vec2::new(48.0, 24.0))
        }
        let clicked = Rc::new(Cell::new(false));

        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world.root_view(), test_target()));
        let flag = Rc::clone(&clicked);
        world
            .root_view_mut()
            .spawn((UiPanel::new(move |_world, _entity, ui| {
                // A real widget, so the click has to come through egui's own hit
                // testing rather than a hand-rolled rectangle test.
                let response = ui.allocate_rect(button(), egui::Sense::click());
                if response.clicked() {
                    flag.set(true);
                }
            }),));

        let input = InputWriter::new();
        let events = input.subscribe();
        world.root_view_mut().spawn((input.clone(), events.clone()));
        input.push(InputEvent::Resized { size_px: (128, 96) });
        events.refresh();

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        // The first frame lays the widget out and knows its fonts; nothing is
        // clicked yet because no input has arrived.
        source.build_scene(&world.root_view(), ctx, &mut encoder);
        assert!(!clicked.get(), "nothing is clicked before any input");

        // The pointer arrives over the button and presses and releases there,
        // in *physical* pixels at a density of one, so points and pixels
        // coincide. A click is complete within the frame egui sees it, which
        // is how a fast click looks to a frame loop.
        let centre = [button().center().x, button().center().y];
        for event in [
            MouseEvent::Moved { position: centre },
            MouseEvent::Button {
                position: centre,
                button: MouseButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            },
            MouseEvent::Button {
                position: centre,
                button: MouseButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            },
        ] {
            input.push(InputEvent::Mouse(event));
        }
        // The tick's batch: what the pointer did since the last frame.
        events.refresh();
        source.build_scene(&world.root_view(), ctx, &mut encoder);

        assert!(
            clicked.get(),
            "a press and release inside a widget must click it"
        );
    }

    /// A published frame is drawn on every frame it is the newest one, not
    /// just the frame it arrived: a settled UI is neither laid out nor
    /// published again, so the drawing side has to keep what it was given.
    #[test]
    fn a_published_frame_is_drawn_until_a_newer_one_arrives() {
        let (world, ctx) = test_world();
        assert!(set_frame_target(&world.root_view(), test_target()));
        // Laid out in a context of its own, the way a publishing world would:
        // its texture ids start where every context's do.
        let mut other = World::new();
        other
            .root_view_mut()
            .spawn((UiPanel::new(|_world, _entity, ui| {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                    0.0,
                    egui::Color32::RED,
                );
            }),));
        let target = test_ui_target();
        let frame = lay_out(
            &other,
            &egui::Context::default(),
            raw_input(target, Some(0.0), true, Vec::new()),
            target.screen,
        );

        let mut source = UiSource::new();
        source.draw_published(ExternalFrames::new(), UiScreen::new());
        source.external.publish(0, frame);
        let mut encoder = encoder(&world, ctx);

        source.build_scene(&world.root_view(), ctx, &mut encoder);
        assert!(
            !source.scene().is_empty(),
            "the published frame is drawn when it arrives"
        );

        // The publisher has nothing new, and the frame it published must not
        // have been emptied by drawing it.
        source.build_scene(&world.root_view(), ctx, &mut encoder);
        assert!(
            !source.scene().is_empty(),
            "the same frame is drawn again while no newer one arrives"
        );
    }
    /// Two contexts allocate texture ids from zero, so the same id names
    /// different images in different worlds. Keys carry the origin, so a
    /// publisher's atlas and the drawing world's own do not collide.
    #[test]
    fn two_contexts_keep_their_textures_apart() {
        let (world, _ctx) = test_world();
        assert!(set_frame_target(&world.root_view(), test_target()));
        let target = test_ui_target();

        // A separate world, with a context whose ids start at zero like any
        // other, paints the frame that gets published.
        let mut other = World::new();
        other
            .root_view_mut()
            .spawn((UiPanel::new(|_world, _entity, ui| {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                    0.0,
                    egui::Color32::RED,
                );
            }),));
        let frame = lay_out(
            &other,
            &egui::Context::default(),
            raw_input(target, Some(0.0), true, Vec::new()),
            target.screen,
        );
        let ids = frame
            .primitives
            .iter()
            .filter_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!ids.is_empty(), "the frame names a texture");

        // The ids are whatever its context allocated: publishing rewrites
        // nothing, because the drawing side keys by (source, id).
        let mut source = UiSource::new();
        source.draw_published(ExternalFrames::new(), UiScreen::new());
        source.external.publish(0, frame);
        let drawn = source.external.with_frames(|slots| {
            slots[0]
                .as_ref()
                .expect("published")
                .primitives
                .iter()
                .filter_map(|primitive| match &primitive.primitive {
                    egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(ids, drawn, "publishing leaves the ids alone");
    }
    /// Without an event source the UI still draws, with an idle frame.
    #[test]
    fn a_world_without_input_state_draws_an_idle_frame() {
        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world.root_view(), test_target()));
        world
            .root_view_mut()
            .spawn((UiPanel::new(|_world, _entity, ui| {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                    0.0,
                    egui::Color32::RED,
                );
            }),));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world.root_view(), ctx, &mut encoder);
        assert!(
            !source.scene().is_empty(),
            "a panel draws with no input in the world"
        );
    }
}
