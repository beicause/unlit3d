//! An egui UI overlay, drawn as a frame source.
//!
//! [`UiSource`] is a [`FrameSource`] that renders the world's egui interfaces
//! over whatever the frame already holds. The interfaces themselves are
//! [`UiPanel`] behaviour components: the source is only their driver, and it
//! runs whatever panels the world carries, in query order.
//!
//! ```
//! use unlit3d::prelude::*;
//! use wgpu_unlit_render::resources::ResourceGraph;
//!
//! let (device, queue) =
//!     wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut world = LocalWorld::new();
//! let _ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
//!
//! // The source, and one interface held as a behaviour component. More
//! // panels are more entities, each with its own sibling state.
//! spawn_source(&mut world, UiSource::new());
//! world.spawn((
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

use unlit_ecs::{Entity, LocalWorld};
use wgpu_unlit_render::globals::{Globals, View};
use wgpu_unlit_render::pipeline::{CAMERA_BINDING, FRAME_BINDING, UnlitPipeline};
use wgpu_unlit_render::resources::{Resource, ResourceGraph, ResourceId};
use wgpu_unlit_render::scene::Scene;
use wgpu_unlit_render::specialize::SurfaceKey;
use wgpu_unlit_render::ui::{
    EguiIntegration, ScreenDescriptor, screen_view, ui_options_for_surface,
};
// `std::time::Instant` panics on `wasm32-unknown-unknown`, where the standard
// library has no clock; `web-time` reads the browser's `Performance.now()`
// there and re-exports `std::time` everywhere else.
use web_time::Instant;
use zerocopy::IntoBytes;

use crate::input::InputState;
pub use crate::source::InputCapture;

use crate::source::{FrameOrder, FrameSource, RenderContext, frame_target};
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
type PanelCallback = Box<dyn FnMut(&LocalWorld, Entity, &mut egui::Ui)>;

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
/// `&LocalWorld` it is handed: `get`, `query` and structural changes queued for
/// the frame loop to apply are all available while it runs.
pub struct UiPanel(pub PanelCallback);

impl UiPanel {
    /// Wrap `f` as a [`UiPanel`].
    pub fn new(f: impl FnMut(&LocalWorld, Entity, &mut egui::Ui) + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Run this panel for `entity`, building its interface in `ui`.
    pub fn run(&mut self, world: &LocalWorld, entity: Entity, ui: &mut egui::Ui) {
        (self.0)(world, entity, ui)
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
    /// The graph node of [`Gpu::camera`], so the source can release it.
    camera_id: ResourceId,
    /// The frame-globals uniform.
    globals: wgpu::Buffer,
    /// The graph node of [`Gpu::globals`], so the source can release it.
    globals_id: ResourceId,
    /// Graph node of the bind group binding the two uniforms.
    global_group: ResourceId,
    /// Uploads egui's textures and geometry and records its draws.
    integration: EguiIntegration,
}

/// A frame source that draws the world's egui interfaces as an overlay.
///
/// The source owns egui's [`Context`](egui::Context) — its font atlas and
/// memory survive across frames — and drives every [`UiPanel`] in the world.
/// Mount it like any other source
/// ([`spawn_source`](crate::source::spawn_source)); it declares
/// [`FrameOrder::OVERLAY`], so it records after the meshes.
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
    /// The target the current pipeline is specialized for.
    surface: Option<SurfaceKey>,
    /// The [`InputState`] resource found on the last frame, if the world has
    /// one.
    input: Option<Entity>,
    /// The frame's GPU context, learned on the first frame. Kept so
    /// [`FrameSource::release`] can reach the resource graph: it is given the
    /// world but no context, and the graph is the context's.
    context: Option<RenderContext>,
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
            surface: None,
            input: None,
            context: None,
        }
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
        world: &'w LocalWorld,
        ctx: RenderContext,
    ) -> impl DerefMut<Target = ResourceGraph> + 'w {
        world
            .get_mut::<ResourceGraph>(ctx.graph)
            .expect("the context's resource graph exists")
    }

    /// Run every entity that carries a [`UiPanel`], in query order.
    ///
    /// The query is rebuilt from scratch on every pass egui runs, which is what
    /// a multi-pass layout needs.
    fn run_panels(&self, world: &LocalWorld, input: egui::RawInput) -> egui::FullOutput {
        self.ctx.run_ui(input, |ui| {
            for (entity, mut panel) in world.query::<&mut UiPanel>() {
                panel.run(world, entity, ui);
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
        let options = ui_options_for_surface(device, surface.color_format.is_srgb(), surface);
        let pipeline = UnlitPipeline::new(device, &options);

        match &mut self.gpu {
            Some(gpu) => {
                let group = global_group(device, &pipeline, &gpu.camera, &gpu.globals);
                graph
                    .replace(gpu.global_group, Resource::BindGroup(group))
                    .expect("the global group is registered in the graph");
                gpu.integration = EguiIntegration::new(device, gpu.global_group, pipeline);
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
                let camera_id = graph
                    .insert_strong(Resource::Buffer(camera.clone()), &[])
                    .expect("a uniform buffer has no dependencies");
                let globals_id = graph
                    .insert_strong(Resource::Buffer(globals.clone()), &[])
                    .expect("a uniform buffer has no dependencies");
                let group = global_group(device, &pipeline, &camera, &globals);
                let group_id = graph
                    .insert_strong(Resource::BindGroup(group), &[camera_id, globals_id])
                    .expect("both uniforms were registered");
                self.gpu = Some(Gpu {
                    camera,
                    camera_id,
                    globals,
                    globals_id,
                    global_group: group_id,
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
        world: &LocalWorld,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Kept so `release` can reach the resource graph it registered in.
        self.context = Some(ctx);

        // Cleared on every path: a frame that records this source must never
        // replay the previous frame's UI.
        self.scene.clear();

        // The target the frame is specialized for, written by the frame loop
        // before it renders. Without it the pipeline cannot be specialized, and
        // a stale one is worse than none.
        let Some(target) = frame_target(world) else {
            log::warn!(
                "UiSource::build_scene: no FrameTarget in the world, so the \
                 frame's render target is unknown and no UI is drawn; write \
                 one by binding a render target before rendering"
            );
            return;
        };

        let device = world
            .get::<wgpu::Device>(ctx.device)
            .expect("the context's device resource exists")
            .clone();
        let queue = world
            .get::<wgpu::Queue>(ctx.queue)
            .expect("the context's queue resource exists")
            .clone();

        // Input arrives as a world resource: the source looks it up by type
        // and remembers the entity. A world without one is not an error — the
        // UI still lays out at the target's own size — but nothing feeds it.
        //
        // The events are translated to egui's own here, while the state is
        // borrowed, rather than cloned out of the world first: the conversion
        // only reads them, so egui's list is the one allocation this makes.
        let input_state = world.query::<&InputState>().next().map(|(entity, state)| {
            let pixels_per_point = if state.scale_factor > 0.0 {
                state.scale_factor
            } else {
                1.0
            };
            (
                entity,
                state.scale_factor,
                state.size_px,
                state.focused,
                convert::to_egui_events(state.events(), pixels_per_point),
            )
        });
        self.input = input_state.as_ref().map(|(entity, ..)| *entity);
        let (scale_factor, size_px, focused, events) = match input_state {
            Some((_, scale_factor, size_px, focused, events)) => {
                (scale_factor, size_px, focused, events)
            }
            None => (1.0, (target.width, target.height), true, Vec::new()),
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
        let screen = ScreenDescriptor {
            size_in_pixels: [size_px.0, size_px.1],
            pixels_per_point,
        };

        let mut graph = Self::graph(world, ctx);
        if self.gpu.is_none() || self.surface != Some(target.surface) {
            self.build_gpu(&device, &mut graph, target.surface);
        }

        // egui lays out in logical points, so the screen rectangle and the
        // projection both take the point size; the clip rectangles the
        // integration derives are scaled to pixels by `pixels_per_point`.
        let points = screen.size_in_points();
        // The events reached egui's own types while `InputState` was borrowed,
        // and they stay in the world afterwards: the UI reads them, it does not
        // consume them, so a game behaviour sees the same frame.
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(points[0], points[1]),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            focused,
            events,
            max_texture_side: Some(device.limits().max_texture_dimension_2d as usize),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&input.viewport_id)
            .expect("the root viewport is always present")
            .native_pixels_per_point = Some(pixels_per_point);

        let output = self.run_panels(world, input);

        let gpu = self.gpu.as_mut().expect("built above");
        gpu.integration
            .update(&mut graph, &queue, encoder, &self.ctx, output, screen);

        // The UI's own camera: `screen_view` maps egui's points onto clip
        // space, and its frame globals stay at their defaults — nothing the UI
        // draws reads the frame clock.
        queue.write_buffer(
            &gpu.camera,
            0,
            screen_view(screen.size_in_points()).as_bytes(),
        );
        queue.write_buffer(&gpu.globals, 0, Globals::default().as_bytes());

        let mut ui_scene = gpu.integration.scene(&graph);
        self.scene.extend(&mut ui_scene);
        self.surface = Some(target.surface);

        // What the UI claimed, read from the context after the frame is laid
        // out, so a game control can decide whether to act on the same frame's
        // input. Written every frame the UI draws.
        publish_capture(world, &self.ctx);
    }

    fn scene(&self) -> &Scene {
        &self.scene
    }

    fn order(&self) -> FrameOrder {
        FrameOrder::OVERLAY
    }

    /// Remove the UI's own graph nodes: its two uniforms and the bind group
    /// binding them.
    ///
    /// The pipeline is a `wgpu` handle the graph reaches only through the bind
    /// group, so dropping it with the source is enough. The bind group is
    /// removed before the buffers it depends on, so it does not outlive them.
    fn release(&mut self, world: &LocalWorld) {
        let Some(gpu) = self.gpu.take() else {
            return;
        };
        let Some(context) = self.context else {
            return;
        };
        let mut graph = world
            .get_mut::<ResourceGraph>(context.graph)
            .expect("the context's resource graph exists");
        graph.remove_drop(gpu.global_group);
        graph.remove_drop(gpu.camera_id);
        graph.remove_drop(gpu.globals_id);
        graph.cleanup_drop();
    }
}

/// Publish what the UI claimed this frame as an [`InputCapture`] resource.
///
/// The resource is looked up by type, so a world gets one from `spawn_context`
/// — the frame's own setup — and a caller never has to create it. A world
/// without one is not an error: the UI still draws, and there is simply
/// nowhere to report what it claimed.
fn publish_capture(world: &LocalWorld, ctx: &egui::Context) {
    let capture = InputCapture {
        pointer: ctx.egui_wants_pointer_input(),
        keyboard: ctx.egui_wants_keyboard_input(),
    };
    let Some(entity) = world.query::<&InputCapture>().next().map(|(e, _)| e) else {
        return;
    };
    let _ = world.with_mut::<InputCapture, _>(entity, |slot| *slot = capture);
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
    pipeline: &UnlitPipeline,
    camera: &wgpu::Buffer,
    globals: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("unlit3d::ui::globals"),
        layout: &pipeline.global_layout,
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::rc::Rc;

    use crate::source::{FrameTarget, set_frame_target, spawn_context};

    /// How many times a panel's closure ran, in a sibling component: a panel
    /// cannot keep state in itself.
    struct Runs(u32);

    /// The frame target the tests draw into.
    ///
    /// Nothing here records a pass, so only the target's key and size matter —
    /// no attachment has to exist behind them.
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
    fn test_world() -> (LocalWorld, RenderContext) {
        let mut world = LocalWorld::new();
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
        (world, ctx)
    }

    /// An encoder to record the frame's uploads into.
    fn encoder(world: &LocalWorld, ctx: RenderContext) -> wgpu::CommandEncoder {
        world
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
        let mut world = LocalWorld::new();
        let runs = Rc::new(Cell::new(0u32));
        for _ in 0..2 {
            let runs = Rc::clone(&runs);
            world.spawn((
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
        let mut output = source.run_panels(&world, input);
        output.textures_delta.clear();

        let panels: Vec<Entity> = world
            .query::<&UiPanel>()
            .map(|(entity, _)| entity)
            .collect();
        assert_eq!(panels.len(), 2, "both panels are driven by one query");
        for entity in panels {
            assert_eq!(
                world.get::<Runs>(entity).expect("the sibling").0,
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
        assert!(set_frame_target(&world, test_target()));
        world.spawn((UiPanel::new(|_world, _entity, ui| {
            // A plain rectangle, so the frame's contents do not depend on the
            // font atlas being ready on the first pass.
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::Pos2::new(8.0, 8.0), egui::Vec2::new(64.0, 32.0)),
                0.0,
                egui::Color32::from_rgb(0, 255, 0),
            );
        }),));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world, ctx, &mut encoder);

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
        world.spawn((UiPanel::new(|_world, _entity, ui| {
            ui.label("not drawn");
        }),));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world, ctx, &mut encoder);

        assert!(source.scene().is_empty());
        assert!(source.gpu.is_none(), "no target was specialized for");
    }

    /// The source is constructed with no arguments and records over the meshes.
    #[test]
    fn a_fresh_source_declares_the_overlay_order() {
        let source = UiSource::new();
        assert_eq!(source.order(), FrameOrder::OVERLAY);
    }

    /// A panel's queued spawn lands once the frame loop applies the queue.
    ///
    /// A panel gets only a shared world, so a structural change is queued —
    /// the same rule every other behaviour component follows.
    #[test]
    fn a_panel_may_queue_a_spawn() {
        /// The entity a panel asks for.
        struct Spawned;

        let world = LocalWorld::new();
        let source = UiSource::new();
        let world = Rc::new(RefCell::new(world));
        {
            let world = Rc::clone(&world);
            world
                .borrow_mut()
                .spawn((UiPanel::new(move |world, _entity, ui| {
                    if world.query::<&Spawned>().next().is_none() {
                        world.queue().spawn((unlit_ecs::Resource, Spawned));
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
            source.run_panels(&world, input.clone())
        };
        output.textures_delta.clear();
        assert_eq!(
            world.borrow().query::<&Spawned>().count(),
            0,
            "a queued spawn has not happened yet"
        );

        world.borrow_mut().apply();
        assert_eq!(
            world.borrow().query::<&Spawned>().count(),
            1,
            "the queue landed on apply"
        );

        // A second frame sees the entity and does not queue another.
        let mut output = source.run_panels(&world.borrow(), input);
        output.textures_delta.clear();
        world.borrow_mut().apply();
        assert_eq!(
            world.borrow().query::<&Spawned>().count(),
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
        let mut world = LocalWorld::new();
        let runs = Rc::new(Cell::new(0u32));
        for _ in 0..2 {
            let runs = Rc::clone(&runs);
            world.spawn((UiPanel::new(move |_world, _entity, _ui| {
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
            let mut output = source.run_panels(&world, input.clone());
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

        let mut world = LocalWorld::new();
        world.spawn((Foreground, UiPanel::new(|_world, _entity, _ui| {})));
        world.spawn((UiPanel::new(|_world, _entity, _ui| {}),));

        let foreground: Vec<Entity> = world
            .query_filtered::<Entity, With<Foreground>>()
            .map(|(entity, _)| entity)
            .collect();
        assert_eq!(foreground.len(), 1, "the filter picked one of two panels");

        let all: Vec<Entity> = world.query::<&UiPanel>().map(|(e, _)| e).collect();
        assert_eq!(all.len(), 2, "the unfiltered query sees both");
        assert!(
            all.contains(&foreground[0]),
            "the filtered panel is one of them"
        );
    }

    /// The frame's events reach the panels.
    ///
    /// A panel that records the events egui gave it proves the `InputState`
    /// resource was read and translated, not merely looked up.
    #[test]
    fn the_frame_events_reach_the_panels() {
        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world, test_target()));

        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&seen);
        world.spawn((UiPanel::new(move |_world, _entity, ui| {
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
        let input = world.spawn((unlit_ecs::Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input, |state| {
            state.set_size_px(128, 96);
            state.push(crate::input::InputEvent::Text(crate::input::TextEvent(
                "hi".to_string(),
            )));
        });

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        // Two frames: egui only lays a text field out once it knows its fonts.
        // The caller clears the events between frames, which is what keeps a
        // frame's text from being delivered twice.
        source.build_scene(&world, ctx, &mut encoder);
        let _ = world.with_mut::<InputState, _>(input, |state| state.clear_events());
        source.build_scene(&world, ctx, &mut encoder);

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
        assert!(set_frame_target(&world, test_target()));
        let flag = Rc::clone(&clicked);
        world.spawn((UiPanel::new(move |_world, _entity, ui| {
            // A real widget, so the click has to come through egui's own hit
            // testing rather than a hand-rolled rectangle test.
            let response = ui.allocate_rect(button(), egui::Sense::click());
            if response.clicked() {
                flag.set(true);
            }
        }),));

        let input = world.spawn((unlit_ecs::Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input, |state| state.set_size_px(128, 96));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        // The first frame lays the widget out and knows its fonts; nothing is
        // clicked yet because no input has arrived.
        source.build_scene(&world, ctx, &mut encoder);
        assert!(!clicked.get(), "nothing is clicked before any input");

        // The pointer arrives over the button and presses and releases there,
        // in *physical* pixels at a density of one, so points and pixels
        // coincide. A click is complete within the frame egui sees it, which
        // is how a fast click looks to a frame loop.
        let _ = world.with_mut::<InputState, _>(input, |state| {
            let centre = [button().center().x, button().center().y];
            for event in [
                crate::input::MouseEvent::Moved { position: centre },
                crate::input::MouseEvent::Button {
                    position: centre,
                    button: crate::input::MouseButton::Primary,
                    pressed: true,
                    modifiers: crate::input::Modifiers::default(),
                },
                crate::input::MouseEvent::Button {
                    position: centre,
                    button: crate::input::MouseButton::Primary,
                    pressed: false,
                    modifiers: crate::input::Modifiers::default(),
                },
            ] {
                state.push(crate::input::InputEvent::Mouse(event));
            }
        });
        source.build_scene(&world, ctx, &mut encoder);

        assert!(
            clicked.get(),
            "a press and release inside a widget must click it"
        );
    }

    /// What the UI claims is published for the rest of the world to read.
    #[test]
    fn the_ui_publishes_what_it_claimed() {
        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world, test_target()));
        // A widget the pointer is over wants the pointer, which is what makes
        // the claim observable at all.
        world.spawn((UiPanel::new(|_world, _entity, ui| {
            let _ = ui.allocate_rect(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(64.0)),
                egui::Sense::click(),
            );
        }),));

        let input = world.spawn((unlit_ecs::Resource, InputState::default()));
        let _ = world.with_mut::<InputState, _>(input, |state| {
            state.set_size_px(128, 96);
            state.push(crate::input::InputEvent::Mouse(
                crate::input::MouseEvent::Moved {
                    position: [16.0, 16.0],
                },
            ));
        });

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world, ctx, &mut encoder);
        // The claim describes the frame just laid out, so a second frame makes
        // the first one's answer readable.
        source.build_scene(&world, ctx, &mut encoder);

        let capture = world
            .query::<&InputCapture>()
            .next()
            .map(|(_, capture)| *capture)
            .expect("the frame context spawns a capture resource");
        assert!(
            capture.pointer,
            "a widget under the pointer claims it: {capture:?}"
        );
        assert!(capture.any());
    }

    /// Without an event source the UI still draws, with an idle frame.
    #[test]
    fn a_world_without_input_state_draws_an_idle_frame() {
        let (mut world, ctx) = test_world();
        assert!(set_frame_target(&world, test_target()));
        world.spawn((UiPanel::new(|_world, _entity, ui| {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                0.0,
                egui::Color32::RED,
            );
        }),));

        let mut source = UiSource::new();
        let mut encoder = encoder(&world, ctx);
        source.build_scene(&world, ctx, &mut encoder);
        assert!(
            !source.scene().is_empty(),
            "a panel draws with no input in the world"
        );
    }
}
