#![doc = include_str!("../README.md")]
#![warn(unsafe_code)]

pub mod scenes;

mod cli;

#[cfg(target_os = "android")]
pub mod android;

/// The fullscreen button, and the landscape lock that only a browser permits.
mod fullscreen;

use std::collections::VecDeque;
use std::io::Write;
use std::process::ExitCode;

use cli::{Args, Parsed};
use scenes::{Advance, SceneControl};
use unlit3d::frame::Frame;
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::ui::{UiSource, egui};
use unlit3d::winit::WindowSurface;
use unlit3d::winit::builtin::{
    CreateWindowRequest, DisplayHandle, ExitRequest, Resumed, WindowSpec, WinitProxy,
    create_window_on_resume, exit_on_close_requested, exit_on_escape,
    request_redraw_while_foreground, window, winit_input_behaviour,
};
#[cfg(target_os = "android")]
use unlit3d::winit::event::OnSuspended;
use unlit3d::winit::event::{OnUserEvent, OnWindowEvent, WinitHost};
// `std::time::Instant` panics on `wasm32-unknown-unknown`, where the standard
// library has no clock; `web-time` reads the browser's `Performance.now()`
// there and re-exports `std::time` everywhere else.
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit_wgpu::scene::ViewportRect;
use web_time::Instant;
use winit::event::WindowEvent;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

/// The number of samples the windowed loop presents every frame with.
const SAMPLE_COUNT: u32 = scenes::spin_cube::SAMPLE_COUNT;

/// The timestep a scene is stepped by when it is drawn without a display.
///
/// A windowed run advances by how long its last frame took, so its pace depends
/// on the machine. Drawing to compare against a stored frame has to be
/// reproducible instead, and this is the fixed step that makes it so.
pub const FIXED_STEP: f32 = 1.0 / 60.0;

/// The format a scene is rendered into when it is drawn without a display.
///
/// sRGB, like the view a window surface presents through: it is what lets the
/// built-in unlit shader's colors reach the readback unchanged.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Whether this build can serve the Model Context Protocol.
///
/// It needs the stdio transport and a tokio runtime, neither of which a
/// browser or an activity has; the switch is parsed everywhere but only acts
/// where this is true.
const MCP_SUPPORTED: bool = cfg!(all(not(target_arch = "wasm32"), not(target_os = "android")));

/// Write `text` to standard output.
///
/// Through [`std::io::Write`] rather than `print!`, which the workspace lints
/// against: a command line that prints its usage is not a reason to relax them.
fn stdout(text: &str) {
    let _ = std::io::stdout().write_all(text.as_bytes());
}

/// Write `text` to standard error.
fn stderr(text: &str) {
    let _ = std::io::stderr().write_all(text.as_bytes());
}

/// Run `future` concurrently: on a worker thread on native, in the browser's
/// task queue on the web (where a thread cannot be blocked).
#[cfg(not(target_arch = "wasm32"))]
fn spawn(future: impl core::future::Future<Output = ()> + Send + 'static) {
    std::thread::spawn(move || pollster::block_on(future));
}

/// Run `future` concurrently: on a worker thread on native, in the browser's
/// task queue on the web (where a thread cannot be blocked).
#[cfg(target_arch = "wasm32")]
fn spawn(future: impl core::future::Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(future);
}

/// Install the logger backend and the panic hook the example reports through.
///
/// The backend follows the terminal the platform has: `env_logger` writes to
/// the standard error a command line owns, logcat takes an activity's, and the
/// browser console takes a page's. `RUST_LOG` selects the level where there is
/// an environment to read it from, and `info` is the default, so the example's
/// own startup messages are visible without a variable. A panic surfaces as an
/// opaque `unreachable executed` on the web unless the hook logs it with its
/// stack first.
fn init_logging() {
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init()
        .ok();
    #[cfg(target_os = "android")]
    // The system starts an activity rather than a shell, so there is no
    // environment to read a level from and the desktop default stands.
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    #[cfg(target_arch = "wasm32")]
    {
        console_log::init_with_level(log::Level::Info).ok();
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    }
}

/// Run the example from the command line.
///
/// Returns the process exit code: the snapshot tests report their result
/// through it, so a CI run sees a mismatched snapshot as a failed command.
///
/// Android never comes through here — its activity has no command line and
/// enters at the module that entry point lives in instead — but the function
/// stays part of the library so the two entry points differ in as little as
/// possible.
pub fn run() -> ExitCode {
    init_logging();

    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help(help)) => {
            stdout(&help);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(args)) => args,
        Err(error) => {
            stderr(&format!("error: {error}\n\n{}", cli::usage()));
            return ExitCode::from(2);
        }
    };

    if args.list_scenes {
        stdout(&scenes::list_text());
        return ExitCode::SUCCESS;
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("an event loop");
    windowed(args, event_loop);
    ExitCode::SUCCESS
}

/// Bind an offscreen `size`-pixel target as the renderer's render target, and
/// return the color texture the frames land in.
///
/// The attachments go into the frame's resource graph exactly as a window
/// surface's do, so the renderer specializes its pipelines on them the same
/// way — and the color texture is the one a readback copies out of. A scene
/// declares its own sample count and whether it draws into a depth attachment,
/// matching how its snapshots were captured.
pub fn bind_offscreen_target(
    world: &World,
    renderer: &mut Renderer,
    device: &wgpu::Device,
    size: (u32, u32),
    samples: u32,
    with_depth: bool,
) -> wgpu::Texture {
    use unlit_wgpu::render_attachments::create_color_target;
    use unlit_wgpu::resources::TextureExt;

    let color = create_color_target(device, OFFSCREEN_FORMAT, size.0, size.1);
    // The texture is registered as a resource in its own right, and the view
    // depends on it, so the renderer holding the view keeps the texture alive —
    // and a screenshot can resolve the texture by handle and read it back.
    let color_view = {
        let mut graph = world
            .root_view()
            .get_mut::<ResourceGraph>(renderer.context().graph)
            .expect("the context's resource graph exists");
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments = FrameAttachments::new(
        world,
        renderer.context(),
        OFFSCREEN_FORMAT,
        size,
        samples,
        with_depth,
    );
    attachments.bind(world, renderer, color_view);

    color
}

/// Build the small world that presents an offscreen texture into the swap
/// chain, and return it with the entity carrying its [`BlitTexture`].
///
/// The blit is one of the MCP run's two worlds. The other — the scene — is a
/// [`World`] component of the host as well, so both live on the thread that
/// runs the event loop; this one holds nothing but the [`BlitSource`] and the
/// [`BlitTexture`] sampling `offscreen`, and the window redraws it from
/// whatever frame the scene last produced.
fn build_blit_world(context: &Gpu, offscreen: &wgpu::Texture) -> (World, BlitView) {
    let mut world = World::new();
    let frame = spawn_context(
        &mut world,
        context.device.clone(),
        context.queue.clone(),
        ResourceGraph::new(),
        context.capabilities,
    );
    let renderer = world.root_view_mut().spawn((Renderer::new(frame),));
    // The frame's own view, in the frame's own format: the offscreen target was
    // allocated with a sampleable usage for exactly this. The view and the
    // sampler are registered in this world's graph, which is what the blit
    // binds.
    let view = TextureExt::create_view(offscreen, &wgpu::TextureViewDescriptor::default());
    let texture = BlitTexture::new(&world, frame, view);
    // The texture hangs under the source that draws it: a source renders its
    // own subtree, and a texture spawned beside it would never be sampled.
    let source = world
        .root_view_mut()
        .spawn((Source::new(BlitSource::new()),));
    let texture = world
        .view_mut(source)
        .expect("the blit source is alive")
        .spawn((texture,));
    (world, BlitView { renderer, texture })
}

/// Rebuild the offscreen target for a new window size, and point the blit at
/// it.
///
/// The color texture the scene's frames land in and the attachments the
/// renderer draws into are replaced for the new size. The new texture is
/// registered in the scene world's graph, as at first bind, so a screenshot can
/// still resolve it by handle; the blit world's `BlitTexture` is replaced with
/// a view of it, which makes `BlitSource` rebuild its bind group on the next
/// frame. The `Offscreen` slot is re-pointed at the new texture as well, so it
/// never names a target the frames no longer land in.
fn resize_offscreen(world: &World, app: Entity, size: (u32, u32)) {
    let (width, height) = size;
    if width == 0 || height == 0 {
        return;
    }
    let Some(scene_entity) = world
        .root_view()
        .get::<SceneEntity>(app)
        .and_then(|slot| slot.0)
    else {
        return;
    };
    let Some(blit_entity) = world
        .root_view()
        .get::<BlitEntity>(app)
        .and_then(|slot| slot.0)
    else {
        return;
    };
    let Some(gpu) = gpu(world, app) else {
        return;
    };
    let def = world
        .root_view()
        .get::<InitialScene>(app)
        .expect("the app entity carries its initial scene")
        .0;
    let scene_world = world
        .root_view()
        .get::<World>(scene_entity)
        .expect("the scene entity carries a world");
    let renderer = world
        .root_view()
        .get::<Scene>(scene_entity)
        .expect("the scene entity carries a scene")
        .renderer;
    let target = scene_world
        .root_view()
        .with_mut::<Renderer, _>(renderer, |renderer| {
            bind_offscreen_target(
                &scene_world,
                renderer,
                &gpu.device,
                size,
                def.samples,
                def.depth,
            )
        })
        .expect("the renderer is a resource entity");
    let _ = world
        .root_view()
        .with_mut::<Offscreen, _>(app, |slot| slot.0 = Some(target.clone()));
    let view = TextureExt::create_view(&target, &wgpu::TextureViewDescriptor::default());
    let (texture_entity, blit_renderer) = {
        let blit = world
            .root_view()
            .get::<BlitView>(blit_entity)
            .expect("the blit entity carries a blit view");
        (blit.texture, blit.renderer)
    };
    let blit_world = world
        .root_view()
        .get::<World>(blit_entity)
        .expect("the blit entity carries a world");
    let ctx = blit_world
        .root_view()
        .get::<Renderer>(blit_renderer)
        .expect("the blit renderer is a resource entity")
        .context();
    let texture = BlitTexture::new(&blit_world, ctx, view);
    *world
        .root_view()
        .get_mut::<BlitTexture>(texture_entity)
        .expect("the blit texture entity carries a blit texture") = texture;
}

/// Drive the windowed example on `event_loop` until it exits.
///
/// The loop is built by the caller, because that is the one thing the entry
/// points differ on: Android's loop has to be handed the activity it runs in,
/// which only the activity's own entry point has.
fn windowed(args: Args, event_loop: EventLoop<UserEvent>) {
    // Poll rather than wait: the scene animates every frame.
    event_loop.set_control_flow(ControlFlow::Poll);
    let scene = scenes::by_id(&args.scene).expect("validated by the CLI");
    let proxy = event_loop.create_proxy();

    let mut host = WinitHost::<UserEvent>::new();
    let world = host.world_mut();

    // One writer, shared: the host world carries it and the adapter that
    // feeds it, and every nested world the host reaches gets a clone of it.
    // Each world then builds its own buffer from the same stream.
    let input = InputWriter::new();
    // The writer is read by every source, so it lives in the shared subtree.
    let shared = world.shared();
    world
        .view_mut(shared)
        .expect("the shared scope exists")
        .spawn((input.clone(),));
    world
        .root_view_mut()
        .spawn((winit_input_behaviour(WinitInput::new(input)),));

    // The window is requested at the first resume, not created here: a
    // platform may not allow a window before then.
    let initial_size = args.size.unwrap_or(scene.size);
    #[cfg_attr(
        not(target_arch = "wasm32"),
        expect(unused_mut, reason = "wasm32 reassigns to append the canvas")
    )]
    let mut attributes = Window::default_attributes()
        .with_title("unlit3d + winit")
        .with_inner_size(winit::dpi::LogicalSize::new(initial_size.0, initial_size.1));
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::WindowAttributesExtWebSys;
        attributes = attributes.with_append(true);
    }
    world.root_view_mut().spawn((
        WindowSpec(attributes),
        CreateWindowRequest(None),
        create_window_on_resume(),
    ));

    world
        .root_view_mut()
        .spawn((ExitRequest(false), exit_on_close_requested()));
    world
        .root_view_mut()
        .spawn((ExitRequest(false), exit_on_escape()));
    world
        .root_view_mut()
        .spawn((request_redraw_while_foreground(),));

    // The application's state and its per-callback behaviours share one
    // entity: every component is its own cell, so a callback writing a sibling
    // state component never conflicts with the behaviour being run.
    let app = world.root_view_mut().spawn((
        GpuState::Idle,
        SceneEntity(None),
        Offscreen(None),
        BlitEntity(None),
        SurfaceSlot(None),
        AttachmentsSlot(None),
        LastFrame(Instant::now()),
        InitialScene(scene),
        SelectorPos(None),
        GpuInfoPos(None),
        McpEnabled(args.mcp && MCP_SUPPORTED),
        ExitRequest(false),
        WinitProxy(proxy),
        FrameSkip::default(),
        draw_on_redraw(),
    ));
    // The frame's own behaviours, in order: request the GPU, build the scene
    // and its surface, serve the controls, advance the scene, present it.
    world.root_view_mut().spawn((request_gpu(app),));
    world.root_view_mut().spawn((ensure_presented(app),));
    world.root_view_mut().spawn((serve_scene_controls(app),));
    world.root_view_mut().spawn((advance_scene(app),));
    world.root_view_mut().spawn((present_frame(app),));
    // A resize is a second `OnWindowEvent` behaviour, so it sits on its own
    // entity: two of one family on an entity would be two borrows of one cell.
    world.root_view_mut().spawn((resize_scene(app),));
    // Serving a user event needs the proxy and the app's state, so it captures
    // the app entity; it is a family of its own, so it sits on its own entity.
    world.root_view_mut().spawn((handle_user_event(app),));
    #[cfg(target_os = "android")]
    world.root_view_mut().spawn((release_on_suspend(app),));

    host.run(event_loop).expect("the event loop runs");
}

/// Events delivered to the winit loop from outside a `WindowEvent`.
enum UserEvent {
    /// The async GPU setup finished; the scene is built from it here, on the
    /// main thread.
    Ready(Gpu),
    /// The async GPU setup failed; the message is reported and the app exits.
    Failed(String),
    /// A command from the MCP transport, to run against a world of the host.
    ///
    /// It arrives on this thread because a world is not `Send`; the app
    /// resolves the command's world handle and dispatches it here, between
    /// frames, so no command ever runs while a frame is being drawn.
    ///
    /// The command is wrapped in an `Option` because a user event reaches its
    /// callbacks mutably and the one that runs it takes it out.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    Command(Option<unlit_ecs::mcp::McpCommand>),
}

/// The blit world's driving state, a component of the host alongside its
/// [`World`].
///
/// The blit samples the scene's offscreen frame into the swap chain. It has no
/// scene of its own; the entity carrying the [`BlitTexture`] is kept so a
/// resize can point it at the rebuilt target.
struct BlitView {
    /// The renderer resource entity every renderer access goes through.
    renderer: Entity,
    /// The entity carrying the [`BlitTexture`] the blit samples.
    texture: Entity,
}

/// The GPU context a scene draws with, requested asynchronously.
///
/// Everything here is a `Send + Sync` wgpu handle, so the request can run off
/// the event loop's thread and the context can cross back to it. The scene
/// itself cannot cross: its ECS world is single-threaded, so it is built on
/// the main thread once this arrives.
///
/// The context and its device outlive a suspension: rebuilding them would drop
/// every pipeline, buffer and texture the scene holds, for a change the
/// platform only makes to the native window.
#[derive(Clone)]
struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// What the device can do beyond the WebGPU baseline, captured while the
    /// adapter is still here to report it.
    ///
    /// `wgpu::Device` exposes its limits and features but not the downlevel
    /// flags, and the adapter is not kept once the device exists, so this is
    /// read once and carried; see [`DeviceCapabilities`].
    capabilities: DeviceCapabilities,
}

impl Gpu {
    /// Request the adapter and device that can present to `surface`.
    ///
    /// `surface` is a *probe*: it exists only so the adapter is required to be
    /// able to present to the window, and is dropped again before the device
    /// is requested. The context deliberately ends up owning no surface,
    /// because a surface is tied to the native window it was created from —
    /// Android replaces that window across a suspension — while the adapter
    /// and device are not. Whoever presents builds one for the window it has
    /// at that moment.
    ///
    /// Async because both requests resolve on the browser's task queue on the
    /// web; blocking on them there would hang the page.
    async fn request(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        drop(surface);

        // The tier is what decides how much of the WebGPU baseline the device
        // is asked for, and so which paths it takes. `UNLIT3D_DEVICE_TIER`
        // selects it — the WebGPU baseline by default, WebGL2's shape when
        // asked for; see [`DeviceTier`].
        let tier = DeviceTier::from_env();
        let capabilities = tier.capabilities_of(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: tier.limits(&adapter.limits()),
                ..Default::default()
            })
            .await?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            capabilities,
        })
    }
}

/// Where the one-time GPU setup stands.
///
/// The request is asynchronous, so a resume that arrives before it lands must
/// not start a second one — two devices and two adapters would be built, and
/// only one of them could present. The `Requested` state is what remembers
/// that the setup is already in flight.
#[derive(Default)]
enum GpuState {
    /// No request has been made: the loop is before its first resume, or the
    /// first one failed and the app is exiting.
    #[default]
    Idle,
    /// The request is in flight; `user_event` carries its result.
    Requested,
    /// The context is here and the scene can draw with it.
    Ready(Gpu),
}

/// The scene's world and the frame loop that drives it.
///
/// Deliberately knows nothing about how a frame reaches a display: it advances
/// the scene's behaviour and renders into whatever target is bound, so the
/// windowed run and the snapshot tests differ only in what they bind and what
/// with the result. The scene itself is a [`scenes::SceneDef`] whose build
/// populated the world and returned the [`SceneControl`] this drives.
///
/// Public because the snapshot tests drive the same scenes offscreen: the web
/// renders them and compares each frame against its snapshot, which is the
/// comparison they make.
///
/// The world is *not* owned here: a scene is the driving state of a world the
/// caller owns, and every frame method takes that world back as `&mut World`.
/// A windowed run stores the world as a component of its host world and pulls
/// it out per frame; the snapshot tests keep a local one. Either way one world
/// has exactly one scene driving it, and the scene never reaches across to
/// another.
pub struct Scene {
    /// The renderer resource entity: the handle every renderer access goes
    /// through.
    pub renderer: Entity,
    /// The size the scene's content is drawn at, in pixels: the target's own,
    /// or the letterboxed part of it when [`Self::viewport`] is set.
    ///
    /// This is what the scene's camera is built for and what its behaviour is
    /// handed, so the projection matches what the viewport scales into place.
    size: (u32, u32),
    /// The region of the target the 3D content is drawn into, or `None` to draw
    /// into the whole of it.
    ///
    /// Set when the scene declares an aspect of its own — see
    /// [`scenes::SceneDef::baseline`] — so a target of any shape shows the same
    /// picture at a different scale rather than showing more of it.
    pub viewport: Option<ViewportRect>,
    /// The snapshot the scene's test compares each frame against, if any.
    ///
    /// The scene's per-frame behaviour is not here: it was moved into the
    /// scene's own world as a frame behaviour at build time, so a frame
    /// advances it without `&mut Scene`.
    pub snapshot: scenes::Snapshot,
    /// The frame-rate measurement the windowed shell's readout displays.
    ///
    /// Unused by the snapshot tests, which have no display to report a rate on.
    frame_rate: Entity,
    /// The index of the next frame the scene's behaviour advances from.
    ///
    /// Carried across calls because [`Self::advance`] hands it to the frame
    /// behaviours through the frame's context, and a fixed sequence may skip a
    /// frame without advancing it.
    frame: u32,
    /// The scene-selector's switch component, read once per frame.
    switch: Entity,
    /// The fullscreen button's request component, read once per frame.
    fullscreen: Entity,
    /// The window's fullscreen state, written once per frame for the button to
    /// label itself from: a panel runs with no window to ask.
    fullscreen_active: Entity,
    /// A scene switch requested by the selector, consumed by the frame loop.
    pending: Option<&'static scenes::SceneDef>,
}

/// Set by the windowed shell's scene-selector panel, read once by the frame
/// loop.
struct SceneSwitch(Option<&'static scenes::SceneDef>);

/// Set by the windowed shell's fullscreen button, read once by the frame loop.
///
/// A flag rather than the state to move to: the button asks for the window to
/// change, and which way it changes is read from the window itself at the
/// moment the request is served. A panel runs while the world is borrowed to
/// lay the UI out, so it can only record the click; the frame loop owns the
/// window the request is served on. See [`fullscreen`].
struct FullscreenRequest(bool);

/// Whether the window is fullscreen, written once per frame by the frame loop.
///
/// The button labels itself for the action it would take, and a panel has no
/// window to ask, so the frame loop reads the window's state and leaves it here
/// for the panel to draw from.
struct FullscreenActive(bool);

/// The host entity carrying the scene's world and its [`Scene`].
///
/// `None` until the GPU context arrives. The world itself outlives a
/// suspension — only the swap chain it is presented through does not — so the
/// scene's state survives one.
struct SceneEntity(Option<Entity>);

/// The host entity carrying the MCP blit world, in MCP mode only.
struct BlitEntity(Option<Entity>);

/// The name this example reports a host world under to the MCP tools.
///
/// The example keeps its own component rather than borrowing the core's: it
/// does not depend on unlit3dx, and a name is all the report needs. Targets
/// without the MCP dependency never read it back.
#[cfg_attr(
    any(target_arch = "wasm32", target_os = "android"),
    expect(
        dead_code,
        reason = "only the MCP bridge reads the name back, and it is not built for these targets"
    )
)]
struct WorldName(&'static str);

/// The swap chain the presented world draws through, absent while suspended.
struct SurfaceSlot(Option<WindowSurface>);

/// The depth and multisample attachments the presented frame draws with, built
/// with the surface and dropped with it.
///
/// They live in whichever world the surface presents — the scene's, or the MCP
/// run's blit world — so they are rebuilt when that world is.
struct AttachmentsSlot(Option<FrameAttachments>);

/// The color texture the MCP scene's frames land in, in MCP mode. The window's
/// blit world samples it.
struct Offscreen(Option<wgpu::Texture>);

/// The time the previous frame was drawn at, for the frame delta.
struct LastFrame(Instant);

/// The scene the example starts with; a switch replaces it.
struct InitialScene(&'static scenes::SceneDef);

/// The selector window's rectangle the last time a scene was live, so a scene
/// switch can rebuild it in the same place and at the same size.
struct SelectorPos(Option<egui::Rect>);

/// The GPU-info panel's rectangle the last time a scene was live, so a scene
/// switch can rebuild it where the user left it.
struct GpuInfoPos(Option<egui::Rect>);

/// Where the scene's draggable panels opened, carried from the scene a switch
/// replaced.
///
/// The two rectangles travel together because they are one idea: each is the
/// position egui remembered for a panel, read from the dying scene's context
/// and handed to the new scene's. A scene built without a carried position
/// opens its panels in their default corners.
#[derive(Clone, Copy, Default)]
pub struct PanelRects {
    /// The scene-selector window's last rectangle.
    pub selector: Option<egui::Rect>,
    /// The GPU-info panel's last rectangle.
    pub gpu_info: Option<egui::Rect>,
}

impl PanelRects {
    /// The panels' rectangles as the app entity remembers them.
    fn read(world: &World, app: Entity) -> Self {
        Self {
            selector: world
                .root_view()
                .get::<SelectorPos>(app)
                .and_then(|pos| pos.0),
            gpu_info: world
                .root_view()
                .get::<GpuInfoPos>(app)
                .and_then(|pos| pos.0),
        }
    }
}

/// Whether to serve the world over the Model Context Protocol rather than
/// drive a scene on this thread.
///
/// False on the web and on Android, which have no stdio transport; see
/// [`MCP_SUPPORTED`].
struct McpEnabled(bool);

/// One frame's work, deferred to the host's apply.
///
/// The frame is a dispatch of the host world's `OnFrame` behaviours, which
/// need `&World`; the command is what hands them a `&mut World`, so it
/// computes the frame's delta and drives them. It is applied between two event
/// callbacks, so no frame is drawn while one is being handled.
struct DrawFrame {
    /// The app entity carrying the state the frame reads and writes.
    app: Entity,
}

impl Command for DrawFrame {
    fn apply(self: Box<Self>, world: &mut World) {
        let app = self.app;
        let now = Instant::now();
        let delta_time = world
            .root_view()
            .get::<LastFrame>(app)
            .map_or(0.0, |last| (now - last.0).as_secs_f32());
        let _ = world
            .root_view()
            .with_mut::<LastFrame, _>(app, |slot| slot.0 = now);
        let mut frame = Frame {
            delta_time,
            index: 0,
            size: (0, 0),
        };
        dispatch_frame(world, &mut frame);
    }
}

/// The interval a scene's index advances at, in seconds, or `None` to advance
/// it once per drawn frame.
struct SequenceStep(Option<f32>);

/// Seconds accumulated towards the next advance of a scene's index.
struct SequenceClock(f32);

/// Whether this frame advanced the scene's index, decided by the sequence step
/// and read by the behaviour that runs the scene's own advance.
struct SequenceStepped(bool);

/// A scene's own per-frame behaviour, stored in its world so the frame
/// behaviour that drives it can call it with the world it mutates.
///
/// `None` only while it is being called: the closure needs `&mut World`, and a
/// component cannot be borrowed mutably and handed the world it lives in at the
/// same time, so it is taken out, called, and put back.
struct SceneAdvance(Option<Advance>);

/// Run the scene's own per-frame behaviour, which needs `&mut World`.
struct RunSceneAdvance {
    /// The entity carrying the scene's [`SceneAdvance`] and sequence state.
    entity: Entity,
    /// The frame the behaviour is advanced by.
    frame: Frame,
}

impl Command for RunSceneAdvance {
    fn apply(self: Box<Self>, world: &mut World) {
        let Self { entity, frame } = *self;
        let advance = world
            .root_view()
            .with_mut::<SceneAdvance, _>(entity, |slot| slot.0.take())
            .flatten();
        let Some(mut advance) = advance else {
            return;
        };
        advance(world, frame.index, frame.delta_time, frame.size);
        let _ = world
            .root_view()
            .with_mut::<SceneAdvance, _>(entity, |slot| slot.0 = Some(advance));
    }
}

/// Run one MCP command against a nested world of the host.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
struct RunInWorld {
    /// The host entity carrying the world the command names.
    entity: Entity,
    /// The command to run.
    command: unlit_ecs::mcp::McpCommand,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Command for RunInWorld {
    fn apply(self: Box<Self>, world: &mut World) {
        let Self { entity, command } = *self;
        let Some(mut nested) = world.root_view().get_mut::<World>(entity) else {
            command.fail(format!("host entity {} carries no world", entity.to_bits()));
            return;
        };
        command.dispatch(&mut nested);
    }
}

/// Run one MCP command against the host world itself.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
struct RunInHost {
    /// The command to run.
    command: unlit_ecs::mcp::McpCommand,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Command for RunInHost {
    fn apply(self: Box<Self>, world: &mut World) {
        self.command.dispatch(world);
    }
}

/// The GPU context, once the asynchronous setup has landed.
fn gpu(world: &World, app: Entity) -> Option<Gpu> {
    let state = world.root_view().get::<GpuState>(app)?;
    match &*state {
        GpuState::Ready(gpu) => Some(gpu.clone()),
        GpuState::Idle | GpuState::Requested => None,
    }
}

/// The world a frame is presented through, and the renderer entity inside it.
///
/// In MCP mode the swap chain presents the blit world; otherwise it presents
/// the scene's. Both are components of the host, reached through the entity
/// carrying them.
fn with_presented<R>(world: &World, app: Entity, f: impl FnOnce(&World, Entity) -> R) -> Option<R> {
    if world
        .root_view()
        .get::<McpEnabled>(app)
        .is_some_and(|mcp| mcp.0)
    {
        let blit = world.root_view().get::<BlitEntity>(app)?.0?;
        let nested = world.root_view().get::<World>(blit)?;
        let renderer = world.root_view().get::<BlitView>(blit)?.renderer;
        Some(f(&nested, renderer))
    } else {
        let scene = world.root_view().get::<SceneEntity>(app)?.0?;
        let nested = world.root_view().get::<World>(scene)?;
        let renderer = world.root_view().get::<Scene>(scene)?.renderer;
        Some(f(&nested, renderer))
    }
}

/// Bring the scene and the swap chain up, as far as the state allows.
///
/// A callback only has `&World`, so the scene's world cannot be spawned from
/// one: it is queued and the entity the queue reserves is kept, and the host's
/// apply lands the world. This runs again on the next frame and finishes what
/// the previous pass could not, because the offscreen target, the blit world
/// and the swap chain are all built out of the world that has by then landed.
fn present(world: &World, app: Entity) {
    let Some(gpu) = gpu(world, app) else {
        return;
    };
    let Some(window) = window(world) else {
        return;
    };
    let size = window.inner_size();
    let size = (size.width.max(1), size.height.max(1));
    let def = world
        .root_view()
        .get::<InitialScene>(app)
        .expect("the app entity carries its initial scene")
        .0;
    let mcp = world
        .root_view()
        .get::<McpEnabled>(app)
        .is_some_and(|mcp| mcp.0);

    // Phase one: the scene's world. Its build needs the GPU context and the
    // window's size, both of which are known here, and the queue reserves the
    // entity the world lands on.
    let scene_entity = match world
        .root_view()
        .get::<SceneEntity>(app)
        .and_then(|slot| slot.0)
    {
        Some(entity) => entity,
        None => {
            let (scene_world, scene) = Scene::new(
                gpu.device.clone(),
                gpu.queue.clone(),
                gpu.capabilities,
                size,
                scenes::SceneOptions {
                    ui: def.ui,
                    selector: true,
                    reproducible: false,
                    letterbox: true,
                    sequence_step: def.step_seconds,
                },
                def,
                PanelRects::read(world, app),
            );
            let entity = world
                .root_view()
                .queue()
                .spawn((scene_world, scene, WorldName("render")));
            let _ = world
                .root_view()
                .with_mut::<SceneEntity, _>(app, |slot| slot.0 = Some(entity));
            window.set_title(&format!(
                "unlit3d{} — {}",
                if mcp { " (MCP)" } else { "" },
                def.title
            ));
            return;
        }
    };
    let Some(scene_world) = world.root_view().get::<World>(scene_entity) else {
        // The queued spawn has not landed yet; the next frame retries.
        return;
    };
    let renderer = world
        .root_view()
        .get::<Scene>(scene_entity)
        .expect("the scene entity carries a scene")
        .renderer;
    let _ = world
        .root_view()
        .with_mut::<Scene, _>(scene_entity, |scene| scene.resize(size, def.baseline));

    // Phase two, MCP mode only: the offscreen target the scene renders into,
    // and the world that blits it into the swap chain.
    if mcp {
        if world
            .root_view()
            .get::<Offscreen>(app)
            .is_some_and(|slot| slot.0.is_none())
        {
            let target = scene_world
                .root_view()
                .with_mut::<Renderer, _>(renderer, |renderer| {
                    bind_offscreen_target(
                        &scene_world,
                        renderer,
                        &gpu.device,
                        size,
                        def.samples,
                        def.depth,
                    )
                })
                .expect("the renderer is a resource entity");
            let _ = world
                .root_view()
                .with_mut::<Offscreen, _>(app, |slot| slot.0 = Some(target));
        }
        if world
            .root_view()
            .get::<BlitEntity>(app)
            .is_some_and(|slot| slot.0.is_none())
        {
            let offscreen = world
                .root_view()
                .get::<Offscreen>(app)
                .and_then(|slot| slot.0.clone())
                .expect("the offscreen target was just built");
            let (blit_world, blit) = build_blit_world(&gpu, &offscreen);
            let entity = world
                .root_view()
                .queue()
                .spawn((blit_world, blit, WorldName("blit")));
            let _ = world
                .root_view()
                .with_mut::<BlitEntity, _>(app, |slot| slot.0 = Some(entity));
            return;
        }
    }

    if world
        .root_view()
        .get::<SurfaceSlot>(app)
        .is_some_and(|slot| slot.0.is_some())
    {
        resize_surface(world, app, size.0, size.1);
        return;
    }
    let surface = match gpu.instance.create_surface(window.clone()) {
        Ok(surface) => surface,
        Err(error) => {
            log::error!("failed to build the presentation surface: {error}");
            return;
        }
    };
    let created = with_presented(world, app, |nested, renderer| {
        nested
            .root_view()
            .with_mut::<Renderer, _>(renderer, |renderer| {
                let window_surface = WindowSurface::new(
                    nested,
                    renderer,
                    &gpu.instance,
                    &gpu.adapter,
                    window.clone(),
                    surface,
                );
                let attachments = if mcp {
                    FrameAttachments::new(
                        nested,
                        renderer.context(),
                        window_surface.color_format(),
                        window_surface.size(),
                        1,
                        false,
                    )
                } else {
                    FrameAttachments::new(
                        nested,
                        renderer.context(),
                        window_surface.color_format(),
                        window_surface.size(),
                        SAMPLE_COUNT,
                        true,
                    )
                };
                (window_surface, attachments)
            })
    });
    let Some(Some((window_surface, attachments))) = created else {
        return;
    };
    let _ = world
        .root_view()
        .with_mut::<SurfaceSlot, _>(app, |slot| slot.0 = Some(window_surface));
    let _ = world
        .root_view()
        .with_mut::<AttachmentsSlot, _>(app, |slot| slot.0 = Some(attachments));
    window.request_redraw();
}

/// Re-state the surface's size and the attachments drawn with it.
fn resize_surface(world: &World, app: Entity, width: u32, height: u32) {
    let Some(mut slot) = world.root_view().get_mut::<SurfaceSlot>(app) else {
        return;
    };
    let Some(surface) = slot.0.as_mut() else {
        return;
    };
    with_presented(world, app, |nested, renderer| {
        nested
            .root_view()
            .with_mut::<Renderer, _>(renderer, |renderer| {
                surface.resize(nested, renderer, width, height);
            })
            .expect("the renderer is a resource entity");
    });
    let size = surface.size();
    if let Some(mut attachments) = world.root_view().get_mut::<AttachmentsSlot>(app)
        && let Some(attachments) = attachments.0.as_mut()
    {
        with_presented(world, app, |nested, _renderer| {
            attachments.resize(nested, size);
        });
    }
}

/// Drop the swap chain the platform is about to invalidate.
#[cfg(target_os = "android")]
fn release(world: &World, app: Entity) {
    let Some(mut slot) = world.root_view().get_mut::<SurfaceSlot>(app) else {
        return;
    };
    let Some(surface) = slot.0.take() else {
        return;
    };
    let Some(scene_entity) = world
        .root_view()
        .get::<SceneEntity>(app)
        .and_then(|slot| slot.0)
    else {
        return;
    };
    let Some(nested) = world.root_view().get::<World>(scene_entity) else {
        return;
    };
    let renderer = world
        .root_view()
        .get::<Scene>(scene_entity)
        .expect("the scene entity carries a scene")
        .renderer;
    nested
        .root_view()
        .with_mut::<Renderer, _>(renderer, |renderer| {
            surface.release(&nested, renderer);
        })
        .expect("the renderer is a resource entity");
    if let Some(mut attachments) = world.root_view().get_mut::<AttachmentsSlot>(app) {
        attachments.0 = None;
    }
}

/// Rebuild the app around a different scene.
fn switch_scene(world: &World, app: Entity, def: &'static scenes::SceneDef) {
    let released = world
        .root_view()
        .get_mut::<SurfaceSlot>(app)
        .and_then(|mut slot| slot.0.take());
    if let Some(surface) = released
        && let Some(scene_entity) = world
            .root_view()
            .get::<SceneEntity>(app)
            .and_then(|slot| slot.0)
        && let Some(nested) = world.root_view().get::<World>(scene_entity)
    {
        let renderer = world
            .root_view()
            .get::<Scene>(scene_entity)
            .expect("the scene entity carries a scene")
            .renderer;
        nested
            .root_view()
            .with_mut::<Renderer, _>(renderer, |renderer| {
                surface.release(&nested, renderer);
            })
            .expect("the renderer is a resource entity");
    }
    if let Some(mut attachments) = world.root_view().get_mut::<AttachmentsSlot>(app) {
        attachments.0 = None;
    }
    // The MCP run's offscreen target and the world that blits it were built
    // around the scene a switch just dropped, so they go with it: the rebuilt
    // scene binds a target of its own, and a fresh blit world samples that one.
    // The switching frame skips drawing, so nothing renders into the target
    // being released.
    if let Some(mut offscreen) = world.root_view().get_mut::<Offscreen>(app) {
        offscreen.0 = None;
    }
    if let Some(entity) = world
        .root_view()
        .get_mut::<BlitEntity>(app)
        .and_then(|mut slot| slot.0.take())
    {
        world.root_view().queue().despawn(entity);
    }
    if let Some(entity) = world
        .root_view()
        .get_mut::<SceneEntity>(app)
        .and_then(|mut slot| slot.0.take())
    {
        world.root_view().queue().despawn(entity);
    }
    if let Some(mut initial) = world.root_view().get_mut::<InitialScene>(app) {
        initial.0 = def;
    }
    present(world, app);
}

/// The host frame behaviours' order: the GPU request starts the async setup
/// before anything looks for a GPU, the present-and-build step runs before the
/// scene is touched, the controls are served before the scene advances, and the
/// frame is drawn last.
const FRAME_GPU: FrameBehaviourOrder = FrameBehaviourOrder(0);
const FRAME_PRESENT: FrameBehaviourOrder = FrameBehaviourOrder(10);
const FRAME_CONTROLS: FrameBehaviourOrder = FrameBehaviourOrder(20);
const FRAME_ADVANCE: FrameBehaviourOrder = FrameBehaviourOrder(30);
const FRAME_DRAW: FrameBehaviourOrder = FrameBehaviourOrder(40);

/// The scene world's frame behaviours' order: input is dispatched before the
/// sequence is stepped, and the sequence before the scene's own behaviour runs.
const SCENE_INPUT: FrameBehaviourOrder = FrameBehaviourOrder(0);
const SCENE_SEQUENCE: FrameBehaviourOrder = FrameBehaviourOrder(10);
const SCENE_ADVANCE: FrameBehaviourOrder = FrameBehaviourOrder(20);

/// Run the scene world's input dispatch for the frame.
fn dispatch_input_on_frame() -> OnFrame {
    OnFrame::new(SCENE_INPUT, |world, _entity, _frame| {
        dispatch_input(world);
    })
}

/// Decide whether this frame advances the scene's sequence.
///
/// A scene with a fixed step advances once its clock has accumulated one; one
/// without advances every frame. The decision is recorded in `SequenceStepped`
/// for the behaviour that runs the advance.
fn step_sequence(entity: Entity) -> OnFrame {
    OnFrame::new(SCENE_SEQUENCE, move |world, _entity, frame| {
        let stepped = match world
            .root_view()
            .get::<SequenceStep>(entity)
            .and_then(|step| step.0)
        {
            Some(step) => world
                .root_view()
                .with_mut::<SequenceClock, _>(entity, |clock| {
                    tick(&mut clock.0, step, frame.delta_time)
                })
                .expect("the scene frame entity carries a sequence clock"),
            None => true,
        };
        let _ = world
            .root_view()
            .with_mut::<SequenceStepped, _>(entity, |flag| flag.0 = stepped);
    })
}

/// Run the scene's own per-frame behaviour when the sequence advanced, and
/// count the frame.
///
/// The behaviour needs `&mut World`, which a frame callback does not have, so
/// it is queued; the index is captured before it is counted on, so the
/// behaviour is handed the frame it advances from.
fn run_scene_advance(entity: Entity) -> OnFrame {
    OnFrame::new(SCENE_ADVANCE, move |world, _entity, frame| {
        let stepped = world
            .root_view()
            .get::<SequenceStepped>(entity)
            .is_some_and(|flag| flag.0);
        if !stepped {
            return;
        }
        world.root_view().queue().push(RunSceneAdvance {
            entity,
            frame: *frame,
        });
        frame.index = frame.index.wrapping_add(1);
    })
}

/// Build the scene and its surface, and short-circuit the frame until both are
/// ready.
///
/// The scene's world is spawned through the queue, so it lands on a later
/// `apply`; the surface is built out of that world. Each frame retries, and
/// the frames that cannot yet draw are skipped.
fn ensure_presented(app: Entity) -> OnFrame {
    OnFrame::new(FRAME_PRESENT, move |world, _entity, _frame| {
        let resumed = world
            .root_view()
            .query::<&Resumed>()
            .next()
            .is_some_and(|(_, state)| state.0);
        if !resumed {
            FrameSkip::skip(world);
            return;
        }
        if world
            .root_view()
            .get::<SceneEntity>(app)
            .is_none_or(|slot| slot.0.is_none())
        {
            present(world, app);
            FrameSkip::skip(world);
            return;
        }
        if !world
            .root_view()
            .get::<SurfaceSlot>(app)
            .is_some_and(|slot| slot.0.is_some())
        {
            present(world, app);
            if !world
                .root_view()
                .get::<SurfaceSlot>(app)
                .is_some_and(|slot| slot.0.is_some())
            {
                FrameSkip::skip(world);
            }
        }
    })
}

/// Open a URL the UI asked for, in a new tab when it asked for one.
///
/// The UI cannot open it itself: a panel runs while the world is borrowed and
/// has no page to reach, so it publishes the request in the world and the frame
/// loop serves it here — the same split the fullscreen request follows. One
/// call covers every platform the example builds for.
fn open_url(url: &OpenUrl) {
    // The target hint only means anything in a browser, where it decides
    // between a new tab and the page the app is already on; elsewhere it is
    // ignored.
    let target = if url.new_tab { "_blank" } else { "_self" };
    let mut options = webbrowser::BrowserOptions::new();
    options.with_target_hint(target);
    if let Err(error) =
        webbrowser::open_browser_with_options(webbrowser::Browser::Default, &url.url, &options)
    {
        log::debug!("the URL {} could not be opened: {error}", url.url);
    }
}

/// Serve the scene selector, the fullscreen button and the UI's URL requests.
///
/// A scene switch drops the scene's world, so nothing may still be borrowed
/// from it when the switch is served: the control state is read in one block
/// and acted on after.
fn serve_scene_controls(app: Entity) -> OnFrame {
    OnFrame::new(FRAME_CONTROLS, move |world, _entity, _frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_view()
            .get::<SceneEntity>(app)
            .and_then(|slot| slot.0)
        else {
            return;
        };
        let active = window(world).is_some_and(|window| fullscreen::active(&window));
        let (selector_pos, gpu_info_pos, switch, fullscreen) = {
            let Some(nested) = world.root_view().get::<World>(scene_entity) else {
                return;
            };
            let (selector_pos, gpu_info_pos) = {
                let scene = world
                    .root_view()
                    .get::<Scene>(scene_entity)
                    .expect("the scene component exists");
                (scene.selector_rect(&nested), scene.gpu_info_rect(&nested))
            };
            let switch = world
                .root_view()
                .with_mut::<Scene, _>(scene_entity, |scene| scene.pending.take())
                .flatten();
            let fullscreen = world
                .root_view()
                .get_mut::<Scene>(scene_entity)
                .is_some_and(|mut scene| scene.fullscreen_request(&nested, active));
            (selector_pos, gpu_info_pos, switch, fullscreen)
        };
        let _ = world
            .root_view()
            .with_mut::<SelectorPos, _>(app, |slot| slot.0 = selector_pos);
        let _ = world
            .root_view()
            .with_mut::<GpuInfoPos, _>(app, |slot| slot.0 = gpu_info_pos);
        if let Some(def) = switch {
            switch_scene(world, app, def);
            FrameSkip::skip(world);
            return;
        }
        if fullscreen && let Some(window) = window(world) {
            fullscreen::toggle(&window);
        }
        // The UI publishes the URLs it was asked to open in the scene's own
        // world, so it is drained there rather than here in the host.
        if let Some(nested) = world.root_view().get::<World>(scene_entity) {
            for url in drain_open_urls(&nested) {
                open_url(&url);
            }
        }
    })
}

/// Advance the scene by the frame's delta.
fn advance_scene(app: Entity) -> OnFrame {
    OnFrame::new(FRAME_ADVANCE, move |world, _entity, frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_view()
            .get::<SceneEntity>(app)
            .and_then(|slot| slot.0)
        else {
            return;
        };
        let Some(mut nested) = world.root_view().get_mut::<World>(scene_entity) else {
            return;
        };
        let _ = world
            .root_view()
            .with_mut::<Scene, _>(scene_entity, |scene| {
                scene.advance(&mut nested, frame.delta_time);
            });
    })
}

/// Present the advanced scene, or blit it into the swap chain in MCP mode.
fn present_frame(app: Entity) -> OnFrame {
    OnFrame::new(FRAME_DRAW, move |world, _entity, _frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_view()
            .get::<SceneEntity>(app)
            .and_then(|slot| slot.0)
        else {
            return;
        };
        let Some(mut nested) = world.root_view().get_mut::<World>(scene_entity) else {
            return;
        };
        let renderer = world
            .root_view()
            .get::<Scene>(scene_entity)
            .expect("the scene entity carries a scene")
            .renderer;
        let viewport = world
            .root_view()
            .get::<Scene>(scene_entity)
            .expect("the scene entity carries a scene")
            .viewport;

        if world
            .root_view()
            .get::<McpEnabled>(app)
            .is_some_and(|mcp| mcp.0)
        {
            let nested_world: &World = &nested;
            nested_world
                .root_view()
                .with_mut::<Renderer, _>(renderer, |renderer| renderer.render(nested_world))
                .expect("the renderer is a resource entity");
            let _ = world
                .root_view()
                .with_mut::<Scene, _>(scene_entity, |scene| {
                    scene.end_frame(&mut nested);
                });
            draw_blit(world, app);
            return;
        }

        let Some(mut slot) = world.root_view().get_mut::<SurfaceSlot>(app) else {
            return;
        };
        let Some(surface) = slot.0.as_mut() else {
            return;
        };
        let nested_world: &World = &nested;
        let attachments = world
            .root_view()
            .get_mut::<AttachmentsSlot>(app)
            .expect("the app entity carries its attachments slot");
        let attachments = attachments.0.as_ref();
        nested_world
            .root_view()
            .with_mut::<Renderer, _>(renderer, |renderer| {
                let Some(frame) = surface.acquire(nested_world, renderer) else {
                    return;
                };
                if let Some(attachments) = attachments {
                    attachments.bind(nested_world, renderer, frame.color_view().clone());
                }
                set_frame_viewport(nested_world, viewport.map(FrameViewport));
                renderer.render(nested_world);
                let queue = nested_world
                    .root_view()
                    .get::<wgpu::Queue>(renderer.context().queue)
                    .expect("the queue resource");
                frame.present(&queue);
            })
            .expect("the renderer is a resource entity");
        let _ = world
            .root_view()
            .with_mut::<Scene, _>(scene_entity, |scene| {
                scene.end_frame(&mut nested);
            });
    })
}

/// Present the blit world into the swap chain, in MCP mode.
fn draw_blit(world: &World, app: Entity) {
    let Some(blit_entity) = world
        .root_view()
        .get::<BlitEntity>(app)
        .and_then(|slot| slot.0)
    else {
        return;
    };
    let Some(mut slot) = world.root_view().get_mut::<SurfaceSlot>(app) else {
        return;
    };
    let Some(surface) = slot.0.as_mut() else {
        return;
    };
    let Some(blit_world) = world.root_view().get::<World>(blit_entity) else {
        return;
    };
    let renderer = world
        .root_view()
        .get::<BlitView>(blit_entity)
        .expect("the blit entity carries a blit view")
        .renderer;
    let attachments = world
        .root_view()
        .get_mut::<AttachmentsSlot>(app)
        .expect("the app entity carries its attachments slot");
    let attachments = attachments.0.as_ref();
    blit_world
        .root_view()
        .with_mut::<Renderer, _>(renderer, |renderer| {
            let Some(frame) = surface.acquire(&blit_world, renderer) else {
                return;
            };
            if let Some(attachments) = attachments {
                attachments.bind(&blit_world, renderer, frame.color_view().clone());
            }
            renderer.render(&blit_world);
            let queue = blit_world
                .root_view()
                .get::<wgpu::Queue>(renderer.context().queue)
                .expect("the queue resource");
            frame.present(&queue);
        })
        .expect("the renderer is a resource entity");
}

/// Request the GPU context once a window exists to present through.
///
/// The window is created by the host at the end of the resume callback, so the
/// request waits for a later frame; a `GpuState` that is no longer idle keeps
/// a later frame from starting it twice.
fn request_gpu(app: Entity) -> OnFrame {
    OnFrame::new(FRAME_GPU, move |world, _entity, _frame| {
        if !world
            .root_view()
            .get::<GpuState>(app)
            .is_some_and(|state| matches!(&*state, GpuState::Idle))
        {
            return;
        }
        let Some(window) = window(world) else {
            return;
        };
        let Some(display) = world
            .root_view()
            .query::<&DisplayHandle>()
            .next()
            .map(|(_, handle)| handle.0.clone())
        else {
            return;
        };
        let Some(proxy) = world
            .root_view()
            .get::<WinitProxy<UserEvent>>(app)
            .map(|proxy| proxy.0.clone())
        else {
            return;
        };
        let _ = world
            .root_view()
            .with_mut::<GpuState, _>(app, |state| *state = GpuState::Requested);
        // The display handle is owned, not borrowed: the instance it builds has
        // to outlive this callback. The descriptor is the environment-aware one
        // so `WGPU_BACKEND` still selects a backend.
        spawn(async move {
            let instance = wgpu::util::new_instance_with_webgpu_detection(
                wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)),
            )
            .await;
            let surface = instance
                .create_surface(window)
                .expect("the window presents to a surface");
            match Gpu::request(instance, surface).await {
                Ok(gpu) => {
                    let _ = proxy.send_event(UserEvent::Ready(gpu));
                }
                Err(error) => {
                    let _ = proxy.send_event(UserEvent::Failed(error.to_string()));
                }
            }
        });
    })
}

/// Serve the events the app posts to itself.
fn handle_user_event(app: Entity) -> OnUserEvent<UserEvent> {
    OnUserEvent::new(move |world, _entity, event| match event {
        UserEvent::Ready(gpu) => {
            let _ = world.root_view().with_mut::<GpuState, _>(app, |state| {
                *state = GpuState::Ready(gpu.clone());
            });
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
            if world
                .root_view()
                .get::<McpEnabled>(app)
                .is_some_and(|mcp| mcp.0)
            {
                start_mcp(world, app);
            }
            present(world, app);
        }
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        UserEvent::Command(command) => {
            if let Some(command) = command.take() {
                dispatch_command(world, command);
            }
        }
        UserEvent::Failed(error) => {
            log::error!("failed to start the renderer: {error}");
            let _ = world
                .root_view()
                .with_mut::<ExitRequest, _>(app, |request| request.0 = true);
        }
    })
}

/// Draw on the frames the loop asks for.
fn draw_on_redraw() -> OnWindowEvent {
    OnWindowEvent::new(|world, entity, payload| {
        if matches!(&payload.event, WindowEvent::RedrawRequested) {
            world.root_view().queue().push(DrawFrame { app: entity });
        }
    })
}

/// Re-state the scene and the surface for a new window size.
fn resize_scene(app: Entity) -> OnWindowEvent {
    OnWindowEvent::new(move |world, _entity, payload| {
        let WindowEvent::Resized(size) = &payload.event else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let size = (size.width, size.height);
        let Some(scene_entity) = world
            .root_view()
            .get::<SceneEntity>(app)
            .and_then(|slot| slot.0)
        else {
            return;
        };
        let baseline = world
            .root_view()
            .get::<InitialScene>(app)
            .expect("the app entity carries its initial scene")
            .0
            .baseline;
        let _ = world
            .root_view()
            .with_mut::<Scene, _>(scene_entity, |scene| scene.resize(size, baseline));
        if world
            .root_view()
            .get::<McpEnabled>(app)
            .is_some_and(|mcp| mcp.0)
        {
            resize_offscreen(world, app, size);
        }
        resize_surface(world, app, size.0, size.1);
    })
}

/// Drop the swap chain the platform is about to invalidate.
#[cfg(target_os = "android")]
fn release_on_suspend(app: Entity) -> OnSuspended {
    OnSuspended::new(move |world, _entity, ()| {
        release(world, app);
    })
}

/// Start the MCP stdio transport, which posts each command back to the loop.
///
/// It runs on its own thread because serving the protocol blocks; the proxy is
/// what carries a command back, because a world is not `Send`.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn start_mcp(world: &World, app: Entity) {
    let Some(proxy) = world
        .root_view()
        .get::<WinitProxy<UserEvent>>(app)
        .map(|proxy| proxy.0.clone())
    else {
        return;
    };
    std::thread::Builder::new()
        .name("unlit3d-mcp-transport".to_owned())
        .spawn(move || {
            let sink = move |command| proxy.send_event(UserEvent::Command(Some(command))).is_ok();
            if let Err(error) = unlit_ecs::mcp::serve_stdio_with(sink) {
                log::error!("the MCP server stopped: {error}");
            }
        })
        .expect("the MCP transport thread starts");
}

/// Queue one MCP command against the world it names.
///
/// Running it needs `&mut World`, so it is queued as a command and applied
/// between two event callbacks. The host answers `list_worlds` itself: it is
/// the only one that knows which entities carry a world and what they are
/// called.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn dispatch_command(world: &World, command: unlit_ecs::mcp::McpCommand) {
    use unlit_ecs::mcp::{WorldEntry, WorldId};
    if command.wants_worlds() {
        let mut worlds = vec![WorldEntry {
            world: WorldId::Host,
            name: "host",
        }];
        for (entity, _) in world.root_view().query::<(Entity, &World)>() {
            let Some(name) = world.root_view().get::<WorldName>(entity) else {
                continue;
            };
            worlds.push(WorldEntry {
                world: WorldId::handle(entity),
                name: name.0,
            });
        }
        command.answer_worlds(worlds);
        return;
    }
    match command.world().entity() {
        Some(entity) => world
            .root_view()
            .queue()
            .push(RunInWorld { entity, command }),
        None => world.root_view().queue().push(RunInHost { command }),
    }
}

impl Scene {
    /// Build the scene `def` into a fresh world.
    ///
    /// Sync, and on the main thread: the ECS world is single-threaded, so it
    /// lives on the thread the event loop runs on. The world starts with the
    /// frame's GPU context and the renderer resource entity; the scene's build
    /// adds its sources, meshes, camera and entities, and returns the
    /// [`SceneControl`] that drives them.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        capabilities: DeviceCapabilities,
        size: (u32, u32),
        options: scenes::SceneOptions,
        def: &'static scenes::SceneDef,
        panels: PanelRects,
    ) -> (World, Self) {
        let mut world = World::new();
        let context = spawn_context(
            &mut world,
            device,
            queue,
            ResourceGraph::new(),
            capabilities,
        );
        let renderer = world.root_view_mut().spawn((Renderer::new(context),));
        // The scene's content is built for the region it will be drawn into,
        // not for the whole target: a camera built for the target's aspect
        // would be sheared back out to it by the viewport. A run that does not
        // letterbox — every capture — draws into the whole target, so its
        // content size is the target's own and nothing about it changes.
        let (viewport, content) = letterbox(def.baseline, size, options.letterbox);

        // The UI source is mounted before the scene is built, so a scene's own
        // panels hang in the scope it draws: a source renders its own subtree,
        // and a panel spawned beside it would never be shown.
        let ui = (options.ui || options.selector).then(|| {
            let mut source_ui = UiSource::new();
            if options.reproducible && def.reproducible_ui {
                // egui fades a window in over the first frames and animates
                // widget transitions, all measured against the clock it is
                // handed. With no animation time every one of them is already
                // over, so the first frame is fully drawn and does not depend
                // on when it was taken.
                source_ui
                    .context_mut()
                    .all_styles_mut(|style| style.animation_time = 0.0);
            }
            world.root_view_mut().spawn((Source::new(source_ui),))
        });
        let control = (def.build)(&mut world, context, ui, content, options);
        let SceneControl { advance, snapshot } = control;

        // The shell's own state — the selector's switch, the fullscreen
        // request and its state, the frame-rate reading — lives in the shared
        // subtree. A panel reads and writes it from the UI source's scope,
        // which reaches the shared subtree and nothing beside it.
        let shared = world.shared();
        let (switch, fullscreen, fullscreen_active, frame_rate) = {
            let mut shared_view = world.view_mut(shared).expect("the shared scope exists");
            (
                shared_view.spawn((SceneSwitch(None),)),
                shared_view.spawn((FullscreenRequest(false),)),
                shared_view.spawn((FullscreenActive(false),)),
                shared_view.spawn((FrameRate::default(),)),
            )
        };
        // The scene's per-frame state and its own behaviour, as components: the
        // frame's behaviours advance them without `&mut Scene`. One entity
        // carries them so the behaviour that advances the scene and the state
        // it reads are reached together.
        let frame_entity = world.root_view_mut().spawn((
            SceneAdvance(Some(advance)),
            SequenceStep(options.sequence_step),
            SequenceClock(0.0),
            SequenceStepped(true),
        ));
        // The frame's own dispatch, in order: run the frame's input, step the
        // index a fixed sequence holds each frame for, then advance the scene
        // by the frame the index names.
        world.root_view_mut().spawn((dispatch_input_on_frame(),));
        world.root_view_mut().spawn((step_sequence(frame_entity),));
        world
            .root_view_mut()
            .spawn((run_scene_advance(frame_entity),));
        // A scene world has no window: it starts whole and nothing short-
        // circuits it. Spawning the flag lets a scene behaviour do so.
        world.root_view_mut().spawn((FrameSkip::default(),));

        if let Some(ui) = ui
            && options.selector
        {
            mount_selector(&mut world, ui, switch, def, panels.selector);
            mount_frame_rate(&mut world, ui, frame_rate);
            mount_gpu_info(&mut world, ui, context.device, panels.gpu_info);
            // Every platform has a window the button can take fullscreen, so
            // it is drawn everywhere. A platform whose backend cannot — Android
            // already takes the whole screen for its activity — draws the
            // button and ignores the press.
            mount_fullscreen(&mut world, ui, fullscreen, fullscreen_active);
            // The API docs are a page every platform can open, so the link is
            // drawn everywhere. Mounted after the fullscreen button, which it
            // is placed under.
            mount_api_docs(&mut world, ui);
        }

        (
            world,
            Self {
                renderer,
                size: content,
                viewport,
                snapshot,
                frame_rate,
                frame: 0,
                switch,
                fullscreen,
                fullscreen_active,
                pending: None,
            },
        )
    }

    /// Re-state the target's size, re-letterboxing the content for its shape.
    ///
    /// Called on every resize and on the first frame that knows the window's
    /// size, so the projection, the region the content is drawn into and the
    /// size the scene's behaviour is handed all follow the target together.
    fn resize(&mut self, target: (u32, u32), baseline: Option<(u32, u32)>) {
        let (viewport, content) = letterbox(baseline, target, true);
        self.size = content;
        self.viewport = viewport;
    }

    /// Advance the scene by `delta_time` seconds.
    ///
    /// A frame is the dispatch of the scene world's `OnFrame` behaviours, in
    /// the order they declare: input, the index, and the scene's own behaviour.
    /// The index and the content size travel in the frame's context, so a
    /// behaviour reads them without a component of its own.
    pub fn advance(&mut self, world: &mut World, delta_time: f32) {
        // Measured before anything else, so the reading covers the whole frame
        // — input, behaviour, render and present — and not just the part
        // between here and the next call.
        world
            .root_view()
            .with_mut::<FrameRate, _>(self.frame_rate, |rate| rate.push(delta_time))
            .expect("the frame-rate component exists");

        let mut frame = Frame {
            delta_time,
            index: self.frame,
            size: self.size,
        };
        dispatch_frame(world, &mut frame);
        self.frame = frame.index;

        // The selector's request, read once so the frame loop can act on it.
        self.pending = world
            .root_view()
            .with_mut::<SceneSwitch, _>(self.switch, |s| s.0.take())
            .expect("the switch component exists");
    }

    /// The selector window's rectangle the last time it was laid out, or
    /// `None` before it has been shown once.
    ///
    /// The window's rectangle is remembered by egui, in this scene's context;
    /// the shell reads it from here so a scene switch — which rebuilds that
    /// context — can rebuild the window in the same place and at the same size.
    fn selector_rect(&self, world: &World) -> Option<egui::Rect> {
        self.panel_rect(world, SELECTOR_WINDOW)
    }

    /// The GPU-info panel's rectangle the last time it was laid out, or `None`
    /// before it has been shown once.
    ///
    /// Carried across a scene switch exactly like [`Self::selector_rect`]: the
    /// panel is a window the user may drag, and the switch rebuilds the context
    /// that remembers where it was left.
    fn gpu_info_rect(&self, world: &World) -> Option<egui::Rect> {
        self.panel_rect(world, GPU_INFO_AREA)
    }

    /// The rectangle egui remembers for the window with `id`.
    ///
    /// Several sources share the world; only the UI one holds egui's window
    /// memory, so skip every other source's entity.
    fn panel_rect(&self, world: &World, id: &str) -> Option<egui::Rect> {
        world
            .root_view()
            .query::<&Source>()
            .find_map(|(_, source)| {
                let ui = source.as_ref::<UiSource>()?;
                ui.context()
                    .memory(|memory| memory.area_rect(egui::Id::new(id)))
            })
    }

    /// Record whether the window is fullscreen and report whether the button
    /// asked for a change, clearing the request.
    ///
    /// Both are read and written once per frame by the windowed loop, which is
    /// what owns the window; the scene keeps no opinion of its own about
    /// fullscreen.
    fn fullscreen_request(&mut self, world: &World, active: bool) -> bool {
        let _ = world
            .root_view()
            .with_mut::<FullscreenActive, _>(self.fullscreen_active, |state| state.0 = active);
        world
            .root_view()
            .with_mut::<FullscreenRequest, _>(self.fullscreen, |request| {
                std::mem::take(&mut request.0)
            })
            .expect("the fullscreen request component exists")
    }

    /// Render one frame into whatever target the caller bound.
    ///
    /// Only the snapshot tests draw through this: the windowed run acquires
    /// the swap chain's image and renders between the acquire and the present,
    /// so it needs both under one borrow.
    pub fn render(&mut self, world: &World) {
        world
            .root_view()
            .with_mut::<Renderer, _>(self.renderer, |renderer| renderer.render(world))
            .expect("the renderer is a resource entity");
    }

    /// Finish the frame: apply whatever a behaviour component or panel queued.
    ///
    /// Nothing is cleared: every consumer reads the events from its own
    /// subscription, and the dispatch that reads them takes them.
    pub fn end_frame(&mut self, world: &mut World) {
        world.apply();
    }
}

/// The scene-selector window's title, which is also its egui area id: the
/// shell reads the window's remembered rectangle by this same id.
const SELECTOR_WINDOW: &str = "scenes";

/// The width the selector window opens at, in points.
///
/// Narrow enough that the list leaves the scene visible behind it: the
/// description wraps onto a few lines instead of the window stretching across
/// the frame. A window a user has dragged keeps the width it was dragged to.
const SELECTOR_DEFAULT_WIDTH: f32 = 220.0;

/// Shift a movable window by however far the screen corner it belongs to moved
/// when the screen changed shape.
///
/// egui applies a window's opening position on the pass that first shows it and
/// afterwards only clamps the remembered rectangle into the screen, so a window
/// seeded into a corner keeps its old spot when the display rotates and the
/// frame becomes a different shape. Shifting it by the corner's movement keeps
/// its distance from that corner — including a distance the user dragged it to —
/// instead of leaving it stranded off its edge.
///
/// The window is anchored to the corner `pivot` names, so its pivot position
/// (the point of the window sitting on that corner) is what moves. The screen
/// the last call saw is remembered under `id`, so the shift is computed once
/// per change and the returned position is applied with
/// [`egui::Window::current_pos`] for that one pass; every other pass returns
/// `None` and the window keeps the position egui remembers for it.
fn follow_screen_corner(
    ctx: &egui::Context,
    id: egui::Id,
    pivot: egui::Align2,
) -> Option<egui::Pos2> {
    let screen = ctx.content_rect();
    let screen_key = id.with("screen");
    let previous = ctx.data(|data| data.get_temp::<egui::Rect>(screen_key));
    ctx.data_mut(|data| {
        data.insert_temp(screen_key, screen);
    });
    let previous = previous?;
    if previous == screen {
        return None;
    }
    // The window has to have been laid out for its position to be known; on its
    // first pass it is seeded by the corner it belongs to, which is where this
    // pass would move it anyway.
    let rect = ctx.memory(|memory| memory.area_rect(id))?;
    let factor = pivot_factor(pivot);
    let moved = screen.lerp_inside(factor) - previous.lerp_inside(factor);
    Some(rect.lerp_inside(factor) + moved)
}

/// The fraction of a window's size at which its pivot sits, per axis.
///
/// The pivot of a window seeded into a corner is that corner, so this is the
/// point of the window — and the matching point of the screen — that stays
/// fixed as the window's size changes.
fn pivot_factor(pivot: egui::Align2) -> egui::Vec2 {
    egui::vec2(pivot.x().to_factor(), pivot.y().to_factor())
}

/// Where the selector window opens when no position was carried over — the
/// first build of a session.
///
/// The window's bottom-right corner sits one overlay margin from the screen's,
/// so the list opens out of the way of the frame-rate readout and the
/// fullscreen button in the top corners. The window is seeded by its
/// bottom-right pivot, so that corner stays put as the content decides the
/// window's size; a window a user has dragged is carried by its whole
/// rectangle, which a rebuild restores exactly.
fn selector_default_pos(ctx: &egui::Context) -> egui::Pos2 {
    ctx.content_rect().right_bottom() - egui::Vec2::splat(OVERLAY_MARGIN)
}

/// Mount the windowed shell's scene-selector panel.
///
/// The panel lists every scene and writes its choice into the [`SceneSwitch`]
/// component, which the frame loop reads once after the frame's advance.
fn mount_selector(
    world: &mut World,
    ui: Entity,
    switch: Entity,
    current: &'static scenes::SceneDef,
    initial: Option<egui::Rect>,
) {
    world
        .view_mut(ui)
        .expect("the UI source is alive")
        .spawn((UiPanel::new(move |world, _entity, ui| {
            // The id is set explicitly: `Window::new` derives it from the title's
            // `Atoms` text, whose hash differs from a plain string id, so the
            // shell's read of the remembered rect would never match.
            let id = egui::Id::new(SELECTOR_WINDOW);
            // The window belongs to the bottom-right corner. A fresh one is seeded
            // by that corner, so the corner stays put while the first layout
            // measures the content; a carried rectangle is seeded by its own
            // bottom-right and given its size, so the first frame lays it out
            // exactly where the previous scene left it. `follow_screen_corner`
            // keeps the corner when the screen changes shape.
            let pivot = egui::Align2::RIGHT_BOTTOM;
            let window = egui::Window::new(SELECTOR_WINDOW).id(id).pivot(pivot);
            let window = match initial {
                Some(rect) => window
                    .default_pos(rect.lerp_inside(pivot_factor(pivot)))
                    .default_size(rect.size()),
                None => window
                    .default_width(SELECTOR_DEFAULT_WIDTH)
                    .default_pos(selector_default_pos(ui.ctx())),
            };
            // A titled window that is movable discards `current_pos`: egui
            // re-applies the pivot it had stored before the area began, so the
            // position given here would not stick. The reposition pass therefore
            // also turns the window immovable; every other pass is movable again
            // and a user's drag works as before.
            let window = match follow_screen_corner(ui.ctx(), id, pivot) {
                Some(pos) => window.current_pos(pos).movable(false),
                None => window,
            };
            window.show(ui.ctx(), |ui| {
                ui.label(format!("{} — {}", current.title, current.description));
                ui.separator();
                for scene in scenes::SCENES.iter().copied() {
                    let selected = scene.id == current.id;
                    if ui.selectable_label(selected, scene.title).clicked() {
                        let _ = world.with_mut::<SceneSwitch, _>(switch, |s| s.0 = Some(scene));
                    }
                }
            });
        }),));
}

/// The frame rate the windowed shell reports, as a component so the panel that
/// displays it and the loop that measures it share one value.
///
/// It lives in the world like any other behaviour state rather than being
/// captured by the panel's closure: a panel may run several times in one frame
/// when egui redoes a layout, and a panel must not reach for its own
/// [`UiPanel`], so the measurement belongs in a sibling component.
#[derive(Default)]
struct FrameRate {
    /// The frames measured so far, capped at [`FrameRate::WINDOW`].
    ///
    /// A sliding window rather than a running mean: a scene that stalls once
    /// should not drag the reading down for the rest of the session, and a rate
    /// that has just changed should show up within a few frames.
    samples: VecDeque<f32>,
}

impl FrameRate {
    /// How many frames the reading averages over.
    ///
    /// About a second at 60 Hz: long enough that the number does not flicker
    /// with every frame, short enough that a change is visible promptly.
    const WINDOW: usize = 60;

    /// Record one frame's duration, in seconds.
    fn push(&mut self, delta: f32) {
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(delta);
    }

    /// The mean frame time over the window, in seconds, or `None` before any
    /// frame was measured.
    fn mean_seconds(&self) -> Option<f32> {
        if self.samples.is_empty() {
            return None;
        }
        let total: f32 = self.samples.iter().sum();
        Some(total / self.samples.len() as f32)
    }

    /// The reading as `"60.0 fps · 16.7 ms"`.
    ///
    /// The frame time is shown next to the rate because the two answer
    /// different questions: the rate is what a display is judged against, while
    /// the milliseconds are what a budget is spent in.
    fn text(&self) -> String {
        match self.mean_seconds() {
            Some(seconds) if seconds > 0.0 => {
                format!("{:.1} fps · {:.1} ms", 1.0 / seconds, seconds * 1e3)
            }
            // The first frame has no previous one to measure against.
            _ => "— fps".to_owned(),
        }
    }
}

/// The distance every overlay keeps from the edge of the screen.
///
/// Each corner overlay is anchored to its edge with this offset, so none of
/// them is drawn flush against the boundary — a readout or a button touching
/// the very edge of a phone's display reads as clipped, and the safe-area
/// insets the page already applies to the canvas are not visible to egui.
const OVERLAY_MARGIN: f32 = 8.0;

/// The gap between the frame-rate readout and the fullscreen button when a
/// window is too narrow to hold them on the same row.
const OVERLAY_GAP: f32 = 8.0;

/// The smallest a corner button is drawn, in points.
///
/// A corner button is an overlay on the scene, so it is kept small enough to
/// read as one — a control as tall as a panel's row competes with the frame it
/// annotates — while still being a comfortable target to click.
const OVERLAY_BUTTON_MIN_SIZE: egui::Vec2 = egui::Vec2::new(110.0, 32.0);

/// A corner button's inner padding, symmetric so the label is centred within
/// the button it is grown to.
///
/// A bare [`egui::Button::min_size`] would leave the label sitting in a corner:
/// the button's atoms are aligned by the surrounding layout, which is not
/// centred. Growing the padding instead — the same on every side — is what
/// pushes the label to the middle of the larger button.
const OVERLAY_BUTTON_PADDING: egui::Vec2 = egui::Vec2::new(10.0, 7.0);

/// The API docs the Pages workflow publishes beside the example page.
///
/// Absolute, because the browser and Android only open an `http(s)` URL — a
/// path relative to the page is not something either hands to a browser. The
/// site is the repository's, so a fork that publishes its own should point this
/// at its own Pages URL.
const API_DOCS_URL: &str = "https://beicause.github.io/unlit3d/docs/index.html";

/// The label the API-docs button carries.
const API_DOCS_LABEL: &str = "API docs";

/// The id of the frame-rate readout's area.
const FRAME_RATE_AREA: &str = "unlit3d::frame-rate";

/// The id of the fullscreen button's area.
const FULLSCREEN_AREA: &str = "unlit3d::fullscreen";

/// The id of the API-docs button's area.
const API_DOCS_AREA: &str = "unlit3d::api-docs";

/// The GPU-info panel's title, shown in its title bar.
const GPU_INFO_TITLE: &str = "GPU";

/// The id of the GPU-info panel's area, which is also the id egui remembers
/// the panel's rectangle under.
const GPU_INFO_AREA: &str = "unlit3d::gpu-info";

/// The narrowest the GPU-info panel may be dragged, in points.
///
/// A fresh panel opens as wide as its widest fact needs to stay on one line,
/// so this floor is only reached by dragging. Without it a drag could squeeze
/// the panel down to a column of one letter: the facts are wrapped text in a
/// scroll area, so they follow the panel all the way down.
const GPU_INFO_MIN_WIDTH: f32 = 160.0;

/// Slack added to the GPU-info panel's measured width.
///
/// A label wraps on a strict comparison against the width it is handed, and a
/// window rounds its size to a whole pixel, so a width that fits a fact exactly
/// can still come out a hair too narrow. A couple of points keep the longest
/// fact on its line when the rounding would otherwise break it.
const GPU_INFO_WIDTH_SLACK: f32 = 2.0;

/// The share of the window's width the GPU-info panel may take, at most.
///
/// The panel takes its opening width from its content, and a device can name a
/// limit that is longer than the window is wide. This keeps such a panel from
/// hanging off the edge and leaves the scene visible beside it; a name that
/// still does not fit wraps, and the panel may be dragged wider by hand.
const GPU_INFO_MAX_WIDTH_FRACTION: f32 = 0.85;

/// The share of the window's height the GPU-info panel may take.
///
/// The panel is an overlay on the scene, not a replacement for it: the limits
/// alone are several dozen rows, so the body scrolls within this share of the
/// window instead of covering it.
const GPU_INFO_HEIGHT_FRACTION: f32 = 0.6;

/// Mount the frame-rate readout at the top of the window, centred.
///
/// An [`egui::Area`] rather than a window: it is pinned, not something the user
/// can drag away or accidentally resize, and `interactable(false)` keeps it
/// from swallowing a click meant for the scene underneath it.
fn mount_frame_rate(world: &mut World, ui: Entity, rate: Entity) {
    world
        .view_mut(ui)
        .expect("the UI source is alive")
        .spawn((UiPanel::new(move |world, _entity, ui| {
            let text = world
                .with_mut::<FrameRate, _>(rate, |rate| rate.text())
                .expect("the frame-rate component exists");
            frame_rate_readout(ui, &text);
        }),));
}

/// Draw the frame-rate readout, showing `text`, centred along the top edge of
/// `ui`'s screen.
///
/// The label is set to [`egui::Label::extend`], so the readout grows to fit its
/// text instead of wrapping it. An anchored area lays its contents out against
/// the size it remembered from the previous pass, so a reading that is wider
/// than the last one — the first real reading after the empty placeholder, or
/// any reading after the rate changes — would otherwise be broken across lines
/// in a narrow window.
///
/// A free function rather than the body of the panel's closure so a test can
/// drive it against a bare [`egui::Context`]: what it has to get right — one
/// line, whatever the previous reading was — is a property of the widget tree,
/// not of the world.
fn frame_rate_readout(ui: &mut egui::Ui, text: &str) {
    egui::Area::new(egui::Id::new(FRAME_RATE_AREA))
        .anchor(egui::Align2::CENTER_TOP, [0.0, OVERLAY_MARGIN])
        .interactable(false)
        // The readout is an overlay, not a window: a scene panel dragged over
        // the same corner must not cover it, so it sits in the layer above the
        // one windows are drawn in.
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text).monospace()).extend());
            });
        });
}

/// The device facts the GPU-info panel shows, read once when it is mounted.
///
/// Reading them once rather than every frame keeps the panel's per-frame pass
/// free of work the device cannot change: an adapter's name, the features it
/// was created with and the limits it reports are fixed for the lifetime of
/// the device, so the strings are built where the device is in hand and the
/// pass only lays them out.
struct GpuInfo {
    /// The adapter's name, shown as the panel's title.
    name: String,
    /// One line naming the backend, the kind of device and the driver.
    summary: String,
    /// The rest of the adapter's description, as label/value pairs.
    rows: Vec<(String, String)>,
    /// The names of the features the device was created with.
    features: Vec<&'static str>,
    /// The limits the device reports, as field-name/value pairs.
    limits: Vec<(String, String)>,
}

impl GpuInfo {
    /// Read `device`'s adapter description, enabled features and limits.
    fn read(device: &wgpu::Device) -> Self {
        let info = device.adapter_info();
        let features = device.features();
        let limits = device.limits();

        // The field names are the panel's labels, so they are spelled once and
        // `stringify!`d rather than repeated as string literals that could drift
        // from the fields they name.
        Self {
            name: info.name.clone(),
            summary: format!(
                "{} · {} · {}",
                info.backend.to_str(),
                device_type_name(info.device_type),
                info.driver,
            ),
            rows: vec![
                ("driver info".to_owned(), info.driver_info.clone()),
                ("vendor".to_owned(), format!("{:#06x}", info.vendor)),
                ("device".to_owned(), format!("{:#06x}", info.device)),
                ("pci bus".to_owned(), info.device_pci_bus_id.clone()),
                (
                    "subgroup".to_owned(),
                    format!("{}..{}", info.subgroup_min_size, info.subgroup_max_size),
                ),
                (
                    "transient saves memory".to_owned(),
                    match info.transient_saves_memory {
                        Some(saves) => saves.to_string(),
                        None => "unknown".to_owned(),
                    },
                ),
            ],
            features: features.iter_names().map(|(name, _)| name).collect(),
            limits: limit_rows(&limits),
        }
    }
}

/// The device's limits as label/value pairs, in the order the device reports
/// them.
///
/// A [`wgpu::Limits`] is serialisable, so the labels and values come from its
/// own serde description: the field names are the JSON object's keys and the
/// values are its entries. Nothing here names a limit, so a field wgpu adds is
/// shown without a change, and a field it renames is not silently dropped.
///
/// The keys come out in the JSON object's order, which serde_json keeps sorted
/// rather than in declaration order; the list is stable and complete either
/// way, and a reader looking a limit up benefits from the order.
///
/// Falls back to an empty list rather than failing: a panel that loses its
/// limits is a cosmetic loss, and serialising a plain struct of integers has
/// no way to fail.
fn limit_rows(limits: &wgpu::Limits) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(limits) else {
        return Vec::new();
    };
    fields
        .into_iter()
        .map(|(name, value)| (name, json_scalar(&value)))
        .collect()
}

/// One limit's value as text.
///
/// Every field of a [`wgpu::Limits`] is an integer, so a scalar is all there
/// is to render; the fallback covers a future field of another shape without
/// hiding it.
fn json_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// The name of a [`wgpu::DeviceType`], for the panel's summary line.
///
/// [`wgpu::DeviceType`] has no `Display` impl, so its names are spelled here.
fn device_type_name(device_type: wgpu::DeviceType) -> &'static str {
    match device_type {
        wgpu::DeviceType::Other => "other",
        wgpu::DeviceType::IntegratedGpu => "integrated GPU",
        wgpu::DeviceType::DiscreteGpu => "discrete GPU",
        wgpu::DeviceType::VirtualGpu => "virtual GPU",
        wgpu::DeviceType::Cpu => "CPU",
    }
}

/// Where the GPU-info panel opens when no position was carried over.
///
/// Like the selector, it is seeded by the corner it belongs in — here the
/// bottom-left, one overlay margin in — so that corner stays put as the
/// content decides the panel's size; a panel the user has dragged is carried
/// by its whole rectangle, which a rebuild restores exactly.
fn gpu_info_default_pos(ctx: &egui::Context) -> egui::Pos2 {
    ctx.content_rect().left_bottom() + egui::Vec2::new(OVERLAY_MARGIN, -OVERLAY_MARGIN)
}

/// Mount the GPU-info panel, a draggable window in the bottom-left corner.
///
/// A window rather than an anchored area so the user can move it out of the
/// way. It is an ordinary window, in the layer the scene's own windows are
/// drawn in: the title bar then highlights exactly when it is the window on
/// top, the same as the selector's, and a scene panel dragged over it covers
/// it the same way.
///
/// The device is read here, once, where it is in hand; the panel then draws
/// from the strings that read produced.
fn mount_gpu_info(world: &mut World, ui: Entity, device: Entity, initial: Option<egui::Rect>) {
    let info = {
        let view = world.view(ui).expect("the UI source is alive");
        let device = view
            .get::<wgpu::Device>(device)
            .expect("the device component exists");
        GpuInfo::read(&device)
    };
    world
        .view_mut(ui)
        .expect("the UI source is alive")
        .spawn((UiPanel::new(move |_world, _entity, ui| {
            gpu_info_panel(ui, &info, initial);
        }),));
}

/// One line of the GPU-info panel.
///
/// The panel sizes itself from these lines and then draws the same values, so a
/// line's wording and the font it is drawn in are chosen once: a width measured
/// from a differently-styled string would not be the width the panel draws with.
enum GpuInfoLine {
    /// The adapter's name or its one-line description, above the separator.
    Header(egui::WidgetText),
    /// A heading that opens a group of facts, set off by a gap above it.
    Heading(egui::WidgetText),
    /// One fact.
    Fact(egui::WidgetText),
}

impl GpuInfoLine {
    /// The line's text and styling, as it is both measured and drawn.
    fn text(&self) -> &egui::WidgetText {
        match self {
            Self::Header(text) | Self::Heading(text) | Self::Fact(text) => text,
        }
    }

    /// Whether the line is part of the header, above the separating rule.
    fn is_header(&self) -> bool {
        matches!(self, Self::Header(_))
    }

    /// Whether the line opens a section, which is set off by a gap above it.
    fn is_heading(&self) -> bool {
        matches!(self, Self::Heading(_))
    }
}

/// The lines the GPU-info panel shows, top to bottom.
///
/// The wording lives here and nowhere else: the width measurement and the
/// drawing both walk this list, so a fact cannot be measured as one string and
/// drawn as another, nor measured in one font and drawn in another.
fn gpu_info_lines(info: &GpuInfo) -> Vec<GpuInfoLine> {
    let mut lines = Vec::new();
    lines.push(GpuInfoLine::Header(
        egui::RichText::new(&info.name).strong().into(),
    ));
    lines.push(GpuInfoLine::Header(
        egui::RichText::new(&info.summary).monospace().into(),
    ));
    lines.push(GpuInfoLine::Heading(
        egui::RichText::new("Adapter").strong().into(),
    ));
    for (label, value) in &info.rows {
        lines.push(GpuInfoLine::Fact(format!("{label}: {value}").into()));
    }
    lines.push(GpuInfoLine::Heading(
        egui::RichText::new(format!("Features ({})", info.features.len()))
            .strong()
            .into(),
    ));
    if info.features.is_empty() {
        lines.push(GpuInfoLine::Fact("none".into()));
    } else {
        for feature in &info.features {
            lines.push(GpuInfoLine::Fact((*feature).into()));
        }
    }
    lines.push(GpuInfoLine::Heading(
        egui::RichText::new(format!("Limits ({})", info.limits.len()))
            .strong()
            .into(),
    ));
    for (name, value) in &info.limits {
        lines.push(GpuInfoLine::Fact(format!("{name} = {value}").into()));
    }
    lines
}

/// How wide the GPU-info panel has to be to keep every line on one row.
///
/// The facts are strings the device chose, and its own names for its limits run
/// past fifty characters, so a fixed opening width would wrap them. Each line is
/// laid out unwrapped as it will be drawn, the widest one sets the content
/// width, and the window's frame and the body's scroll bar are added back on:
/// the panel's size, unlike the text's, includes both.
///
/// A free function rather than part of the panel so a test can ask what width a
/// given set of lines needs without drawing a pass.
fn gpu_info_width(ui: &egui::Ui, lines: &[GpuInfoLine]) -> f32 {
    let mut widest: f32 = 0.0;
    for line in lines {
        let width = line
            .text()
            .clone()
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                0.0,
                egui::FontSelection::Default,
            )
            .size()
            .x;
        widest = widest.max(width);
    }
    let style = ui.style();
    let frame = egui::Frame::window(style).total_margin().sum().x;
    let scroll_bar = style.spacing.scroll.allocated_width();
    widest + frame + scroll_bar + GPU_INFO_WIDTH_SLACK
}

/// Draw the GPU-info panel for `info`, seeded at `initial` or the bottom-left
/// corner of `ui`'s screen.
///
/// A free function rather than the body of the panel's closure so a test can
/// drive it against a bare [`egui::Context`]: what it has to get right — where
/// it opens, the width it takes from its facts and the facts it prints — is a
/// property of the widget tree, not of the world.
fn gpu_info_panel(ui: &mut egui::Ui, info: &GpuInfo, initial: Option<egui::Rect>) {
    // The id is set explicitly, as in the selector: `Window::new` would derive
    // it from the title text, and the shell reads the remembered rectangle back
    // under this id.
    //
    // The panel is resizable in both directions, like the selector and the
    // scene's own windows: the limits alone are several dozen rows, and how
    // much of the scene the panel covers while they are read is the reader's
    // to decide. It opens as wide as its facts need, and dragging it narrower
    // is what wraps them.
    let id = egui::Id::new(GPU_INFO_AREA);
    // The panel belongs to the bottom-left corner, so it is seeded and kept by
    // that corner: a fresh one opens there, a carried rectangle is seeded by its
    // own bottom-left and given its size, and `follow_screen_corner` moves it
    // when the screen changes shape.
    let pivot = egui::Align2::LEFT_BOTTOM;
    let window = egui::Window::new(GPU_INFO_TITLE)
        .id(id)
        .pivot(pivot)
        .min_width(GPU_INFO_MIN_WIDTH)
        .max_width(ui.ctx().content_rect().width() * GPU_INFO_MAX_WIDTH_FRACTION);
    let lines = gpu_info_lines(info);
    // The lines above the rule are the header; the rest are the facts, which
    // scroll, so the two runs are drawn into different places from the one
    // list.
    let header = lines.iter().take_while(|line| line.is_header()).count();
    let window = match initial {
        Some(rect) => window
            .default_pos(rect.lerp_inside(pivot_factor(pivot)))
            .default_size(rect.size()),
        None => window
            // The size is given as well as the position: an area is laid out
            // against the size it had before the first pass measures it, and
            // egui's default area size is wider than a narrow window, which
            // would push the seeded corner inward when the area is constrained
            // to the screen.
            .default_size(egui::Vec2::new(
                gpu_info_width(ui, &lines),
                ui.ctx().content_rect().height() * GPU_INFO_HEIGHT_FRACTION,
            ))
            .default_pos(gpu_info_default_pos(ui.ctx())),
    };
    // A titled window that is movable discards `current_pos`: egui re-applies
    // the pivot it had stored before the area began, so the position given here
    // would not stick. The reposition pass therefore also turns the window
    // immovable; every other pass is movable again and a user's drag works as
    // before.
    let window = match follow_screen_corner(ui.ctx(), id, pivot) {
        Some(pos) => window.current_pos(pos).movable(false),
        None => window,
    };
    window.show(ui.ctx(), |ui| {
        let (header_lines, body_lines) = lines.split_at(header);
        ui.vertical(|ui| {
            for line in header_lines {
                ui.label(line.text().clone());
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(ui.ctx().content_rect().height() * GPU_INFO_HEIGHT_FRACTION)
                // Fill the panel's width so the facts wrap within it instead of
                // the panel sizing itself to the widest one; shrink to the
                // content's height so a device with few limits gets a short
                // panel, and dragging it taller then does not pad it out with
                // empty space.
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for line in body_lines {
                        if line.is_heading() {
                            ui.add_space(OVERLAY_GAP);
                        }
                        ui.label(line.text().clone());
                    }
                });
        });
    });
}

/// Mount the fullscreen button in the window's top-right corner.
///
/// A button rather than the old "the first press asks for fullscreen": a press
/// on the canvas is not discoverable — a phone shows no hint that it would
/// work, and nothing tells a user who left fullscreen that they may ask again —
/// whereas a button is visible on every platform and keeps working after the
/// browser has left fullscreen. Nothing about the request is latched, so the
/// display can be entered and left as often as the button is pressed.
///
/// The click is recorded rather than served here: a panel runs while the world
/// is borrowed, and it has no window to ask; the frame loop reads the request
/// and calls [`fullscreen::toggle`], which is where a browser accepts it from.
fn mount_fullscreen(world: &mut World, ui: Entity, request: Entity, state: Entity) {
    world
        .view_mut(ui)
        .expect("the UI source is alive")
        .spawn((UiPanel::new(move |world, _entity, ui| {
            let active = world
                .get::<FullscreenActive>(state)
                .is_some_and(|state| state.0);
            if fullscreen_button(ui, active) {
                let _ = world.with_mut::<FullscreenRequest, _>(request, |request| request.0 = true);
            }
        }),));
}

/// Mount the link to the API docs in the window's top-right corner.
///
/// The link lives in the UI rather than in the page so the two corner overlays
/// are laid out by the same code and cannot collide: the page's own link knew
/// nothing about the button, which is what put the two on top of each other.
/// Mounted after [`mount_fullscreen`], because the link is placed under the
/// button it measures.
///
/// The click is recorded rather than served here: a panel runs while the world
/// is borrowed and has no page to reach; the frame loop reads the request and
/// opens it with [`webbrowser`], which covers every platform the example runs
/// on.
fn mount_api_docs(world: &mut World, ui: Entity) {
    world
        .view_mut(ui)
        .expect("the UI source is alive")
        .spawn((UiPanel::new(|_world, _entity, ui| {
            api_docs_button(ui);
        }),));
}

/// The vertical offset a corner button's anchor is drawn at, given the area
/// above it in the same corner.
///
/// `above` and the button share the top row when the window is wide enough for
/// both to sit side by side. When it is not — the frame-rate readout is
/// centred, so on a portrait phone it reaches into the corner the fullscreen
/// button anchors to — the button drops to the row under `above` instead:
/// hiding the end of the reading under the button that is meant to sit beside
/// it on a roomier window does not help. The fallback keeps both legible and
/// separated at any width.
///
/// Both areas are anchored, and an anchored area is laid out against the
/// rectangle the previous pass remembered for it — which is what is compared
/// here, so the decision is made about the very positions egui will use.
fn overlay_button_top(ctx: &egui::Context, above: &str, id: &str) -> f32 {
    let above = ctx.memory(|memory| memory.area_rect(egui::Id::new(above)));
    let button = ctx.memory(|memory| memory.area_rect(egui::Id::new(id)));
    match (above, button) {
        // The area above stops clear of the button's column.
        (Some(above), Some(button)) if above.right() + OVERLAY_GAP < button.left() => {
            OVERLAY_MARGIN
        }
        // The area above reaches into the button's corner: stack beneath it.
        (Some(above), _) => above.bottom() + OVERLAY_GAP,
        // The button is the first thing drawn; the top row is where it belongs.
        (None, _) => OVERLAY_MARGIN,
    }
}

/// Draw a button that owns a corner, and report whether it was pressed.
///
/// `id` names the area, so the button can be read back — the docs link is
/// placed under the fullscreen button by measuring it — and so two corners can
/// be tested apart. A free function rather than the body of a panel's closure
/// so a test can drive it against a bare [`egui::Context`].
fn corner_button(ui: &mut egui::Ui, id: &str, top: f32, label: egui::RichText) -> bool {
    egui::Area::new(egui::Id::new(id))
        .anchor(egui::Align2::RIGHT_TOP, [-OVERLAY_MARGIN, top])
        // A corner button is an overlay, not a window: a scene panel dragged
        // over the corner must not cover it, so it sits in the layer above the
        // one windows are drawn in.
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .show(ui, |ui| {
                    ui.style_mut().spacing.button_padding = OVERLAY_BUTTON_PADDING;
                    ui.add(
                        egui::Button::new(label)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .min_size(OVERLAY_BUTTON_MIN_SIZE),
                    )
                    .clicked()
                })
                .inner
        })
        .inner
}

/// Draw the fullscreen button, labelled for what pressing it does, and report
/// whether it was pressed.
///
/// The label follows the document rather than a local flag: the user can leave
/// fullscreen with the browser's own control — Escape, or the back gesture on a
/// phone — and the button has to offer to go back in afterwards. That is also
/// why it owns its corner rather than sharing a row with the readout: each
/// anchored area keeps to itself, and the button is only drawn on top of the
/// readout's line when the two would otherwise collide.
///
/// The glyphs are drawn from the default font — ⛶ to enter and ⏏ to leave —
/// with a word beside each so the meaning does not rest on a symbol alone. Both
/// labels are laid out to extend rather than wrap: the exit label is the wider
/// of the two, and an anchored area lays its contents out against the size it
/// remembered from the previous pass, so switching to it would otherwise break
/// it across two lines in the narrower area the enter label had established.
fn fullscreen_button(ui: &mut egui::Ui, active: bool) -> bool {
    let label = if active {
        "⏏ Exit fullscreen"
    } else {
        "⛶ Fullscreen"
    };
    let top = overlay_button_top(ui.ctx(), FRAME_RATE_AREA, FULLSCREEN_AREA);
    corner_button(
        ui,
        FULLSCREEN_AREA,
        top,
        egui::RichText::new(label).monospace(),
    )
}

/// Draw the API-docs link under the fullscreen button, and report whether it
/// was pressed.
///
/// The click becomes egui's own URL command, which the UI source publishes in
/// the world for the frame loop to serve, rather than opening the page here: a
/// panel runs while the world is borrowed and cannot wait on one.
fn api_docs_button(ui: &mut egui::Ui) -> bool {
    let top = overlay_button_top(ui.ctx(), FULLSCREEN_AREA, API_DOCS_AREA);
    let label = egui::RichText::new(API_DOCS_LABEL).monospace();
    let clicked = corner_button(ui, API_DOCS_AREA, top, label);
    if clicked {
        ui.ctx().open_url(egui::OpenUrl::same_tab(API_DOCS_URL));
    }
    clicked
}

/// The region a scene's content is drawn into and the size its camera is built
/// for, for a `target`-pixel render target.
///
/// `baseline` is the aspect to keep, or `None` for a scene with no aspect of
/// its own — a UI-only one — which draws into the whole target. `letterbox`
/// turns the whole mechanism off, which is what a capture wants: its
/// captures are the scene's own size and must stay comparable with the stored
/// snapshots.
fn letterbox(
    baseline: Option<(u32, u32)>,
    target: (u32, u32),
    letterbox: bool,
) -> (Option<ViewportRect>, (u32, u32)) {
    match baseline.filter(|_| letterbox) {
        Some(baseline) => (
            Some(scenes::baseline_viewport(baseline, target)),
            scenes::content_size(baseline, target),
        ),
        None => (None, target),
    }
}

/// Accumulate `delta_time` into `clock` and report whether a `step`-long
/// interval has passed, subtracting it from the clock if so.
///
/// At most one interval is consumed per call, so a long stall advances a
/// fixed-sequence scene by one frame rather than skipping several. The
/// leftover stays in the clock, so the sequence keeps pace over time.
fn tick(clock: &mut f32, step: f32, delta_time: f32) -> bool {
    *clock += delta_time;
    if *clock >= step {
        *clock -= step;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{
        API_DOCS_AREA, API_DOCS_URL, FRAME_RATE_AREA, FULLSCREEN_AREA, FrameRate, GPU_INFO_AREA,
        GPU_INFO_MIN_WIDTH, GpuInfo, OVERLAY_BUTTON_MIN_SIZE, OVERLAY_MARGIN, api_docs_button,
        egui, frame_rate_readout, fullscreen_button, gpu_info_panel, letterbox, limit_rows, tick,
    };
    // The scene-switch test below drives the app's own slots through a headless
    // device, so it is host-only — and these are the names only it uses.
    #[cfg(not(target_arch = "wasm32"))]
    use super::{
        BlitEntity, InitialScene, OFFSCREEN_FORMAT, Offscreen, SceneEntity, scenes, switch_scene,
    };
    #[cfg(not(target_arch = "wasm32"))]
    use unlit_wgpu_test_util::{Ctx, block_on};
    #[cfg(not(target_arch = "wasm32"))]
    use unlit3d::prelude::{World, create_color_target};

    /// A scene switch drops the MCP run's offscreen target and the blit world
    /// built around the scene it replaced.
    ///
    /// The offscreen texture received the dying scene's frames and the blit
    /// world sampled it, so both are keyed to that scene. Left behind, the
    /// rebuilt scene would render through a renderer that never bound a target
    /// of its own, and the next MCP frame would panic for want of an
    /// attachment.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_scene_switch_drops_the_mcp_offscreen_target_and_its_blit_world() {
        let ctx = block_on(Ctx::headless());
        let mut world = World::new();

        let old_blit = world.root_view_mut().spawn_empty();
        let old_scene = world.root_view_mut().spawn_empty();
        let texture = create_color_target(&ctx.device, OFFSCREEN_FORMAT, 8, 8);
        let app = world.root_view_mut().spawn((
            InitialScene(scenes::default()),
            Offscreen(Some(texture)),
            BlitEntity(Some(old_blit)),
            SceneEntity(Some(old_scene)),
        ));

        let next = scenes::by_id("ui_only").expect("the example ships a UI-only scene");
        switch_scene(&world, app, next);
        world.apply();

        assert!(
            world
                .root_view()
                .get::<Offscreen>(app)
                .expect("the app keeps its slots")
                .0
                .is_none(),
            "the offscreen target went with the scene that filled it"
        );
        let blit = world
            .root_view()
            .get::<BlitEntity>(app)
            .expect("the app keeps its slots");
        assert!(blit.0.is_none(), "the blit world went with the scene");
        assert!(
            !world.root_view().contains(old_blit),
            "the blit world was despawned"
        );
        assert!(
            !world.root_view().contains(old_scene),
            "the old scene was despawned"
        );
        assert_eq!(
            world
                .root_view()
                .get::<InitialScene>(app)
                .expect("the app keeps its initial scene")
                .0
                .id,
            "ui_only"
        );
    }

    /// An egui input for a `screen`-point screen carrying `events`.
    fn raw_input(screen: (f32, f32), events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(screen.0, screen.1),
            )),
            events,
            ..Default::default()
        }
    }

    /// Run one egui pass over a `screen`-point screen with `events`, drawing the
    /// fullscreen button in the state `active`, and report what it drew.
    ///
    /// What the pass drew is both what pressing the button did — the returned
    /// flag — and every galley it laid out, which is how a test reads the
    /// label without reaching into egui's internals.
    fn fullscreen_pass(
        ctx: &egui::Context,
        screen: (f32, f32),
        active: bool,
        events: Vec<egui::Event>,
    ) -> (bool, Vec<String>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(screen.0, screen.1),
            )),
            events,
            ..Default::default()
        };
        let mut clicked = false;
        let mut output = ctx.run_ui(input, |ui| {
            clicked = fullscreen_button(ui, active);
        });
        // egui panics if a texture delta is dropped unapplied; a real frame
        // hands it to the integration, and this test discards it.
        output.textures_delta.clear();
        let labels = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        (clicked, labels)
    }

    /// The centre of the area with `id`, which a previous pass has to have
    /// drawn for this to answer.
    fn area_centre(ctx: &egui::Context, id: &str) -> egui::Pos2 {
        area_rect(ctx, id).center()
    }

    /// The rectangle of the area with `id`, which a previous pass has to have
    /// drawn for this to answer.
    fn area_rect(ctx: &egui::Context, id: &str) -> egui::Rect {
        ctx.memory(|memory| {
            memory
                .area_rect(egui::Id::new(id))
                .expect("the area was drawn")
        })
    }

    /// The pointer events that begin a drag at `pos`: the pointer arrives and
    /// the button goes down.
    fn drag_start(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }

    /// The pointer events that move a held pointer to `to`. The movement has
    /// to be a pass of its own: egui measures a drag as the delta between two
    /// passes, not between two events of one.
    fn drag_move(to: egui::Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(to)]
    }

    /// The pointer events that release a held pointer at `pos`.
    fn drag_release(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }]
    }

    /// The pointer events that tap `pos`: a press and, on a later pass, the
    /// release that egui reports as a click.
    fn tap(pos: egui::Pos2) -> (Vec<egui::Event>, Vec<egui::Event>) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        (
            vec![egui::Event::PointerMoved(pos), button(true)],
            vec![button(false)],
        )
    }

    /// Run one egui pass over a `screen`-point screen with `events`, drawing
    /// the GPU-info panel for `info` where `initial` says, and report every
    /// galley it laid out.
    fn gpu_info_pass(
        ctx: &egui::Context,
        screen: (f32, f32),
        info: &GpuInfo,
        initial: Option<egui::Rect>,
        events: Vec<egui::Event>,
    ) -> Vec<String> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(screen.0, screen.1),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            gpu_info_panel(ui, info, initial);
        });
        // egui panics if a texture delta is dropped unapplied; a real frame
        // hands it to the integration, and this test discards it.
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Run one egui pass over a `screen`-point screen, drawing the GPU-info
    /// panel for `info`, and report every galley it laid out with the number of
    /// rows it was broken into. A fact on a single row is a fact the panel was
    /// wide enough to keep on one line.
    fn gpu_info_rows(
        ctx: &egui::Context,
        screen: (f32, f32),
        info: &GpuInfo,
        initial: Option<egui::Rect>,
    ) -> Vec<(String, usize)> {
        let mut output = ctx.run_ui(raw_input(screen, Vec::new()), |ui| {
            gpu_info_panel(ui, info, initial);
        });
        // egui panics if a texture delta is dropped unapplied; a real frame
        // hands it to the integration, and this test discards it.
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => {
                    Some((text.galley.text().to_owned(), text.galley.rows.len()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_limit_list_comes_from_the_limits_own_description() {
        // The labels are wgpu's own field names, so the test names the ones the
        // type is known to carry rather than a copy of the whole list.
        let rows = limit_rows(&wgpu::Limits::defaults());
        assert!(
            rows.len() > 50,
            "a limits struct has dozens of fields, got {}",
            rows.len()
        );
        for expected in [
            ("maxBindGroups", "4"),
            ("maxTextureDimension1D", "8192"),
            ("maxBufferSize", "268435456"),
        ] {
            assert!(
                rows.iter()
                    .any(|(name, value)| name == expected.0 && value == expected.1),
                "the limits do not show {expected:?}"
            );
        }
    }

    /// The facts a GPU-info panel is asked to show, small enough to read.
    fn sample_gpu_info() -> GpuInfo {
        GpuInfo {
            name: "Test Adapter".to_owned(),
            summary: "vulkan · discrete GPU · test driver".to_owned(),
            rows: vec![("driver info".to_owned(), "1.2.3".to_owned())],
            features: vec!["DEPTH_CLIP_CONTROL", "TEXTURE_COMPRESSION_BC"],
            limits: vec![("max_bind_groups".to_owned(), "4".to_owned())],
        }
    }

    #[test]
    fn the_gpu_info_panel_opens_in_the_bottom_left_corner() {
        // The panel is an overlay on the scene, so the first build has to put
        // it in its corner and keep it inside the window.
        for screen in [(1280.0, 577.0), (800.0, 600.0), (390.0, 844.0)] {
            let ctx = egui::Context::default();
            let info = sample_gpu_info();
            // Two passes: the first measures the content, the second lays the
            // window out against that measurement.
            let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());
            let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());

            let panel = area_rect(&ctx, GPU_INFO_AREA);
            assert!(
                (panel.left() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the panel {panel:?} does not keep the left margin"
            );
            assert!(
                (screen.1 - panel.bottom() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the panel {panel:?} does not keep the bottom margin"
            );
            assert!(
                panel.right() <= screen.0,
                "at {screen:?} the panel {panel:?} runs off the right"
            );
            assert!(
                panel.width() >= GPU_INFO_MIN_WIDTH,
                "at {screen:?} the panel {panel:?} is narrower than its floor"
            );
        }
    }

    #[test]
    fn the_gpu_info_panel_follows_its_corner_when_the_screen_changes_shape() {
        // The panel opens against the bottom-left corner. When the display
        // rotates, the frame becomes a different shape and egui only clamps the
        // remembered rectangle, so the panel has to be moved by how far that
        // corner moved — otherwise it stays where the old, now-rotated screen
        // put it.
        let ctx = egui::Context::default();
        let info = sample_gpu_info();
        for (from, to) in [
            ((1280.0, 577.0), (577.0, 1280.0)),
            ((800.0, 600.0), (600.0, 800.0)),
        ] {
            // Two passes: the first measures the content, the second lays the
            // window out against that measurement.
            let _ = gpu_info_pass(&ctx, from, &info, None, Vec::new());
            let _ = gpu_info_pass(&ctx, from, &info, None, Vec::new());

            let _ = gpu_info_pass(&ctx, to, &info, None, Vec::new());
            let panel = area_rect(&ctx, GPU_INFO_AREA);
            assert!(
                (panel.left() - OVERLAY_MARGIN).abs() < 1.0
                    && (to.1 - panel.bottom() - OVERLAY_MARGIN).abs() < 1.0,
                "after {from:?} -> {to:?} the panel {panel:?} left its corner"
            );
        }
    }

    #[test]
    fn the_gpu_info_panel_opens_wide_enough_for_its_facts() {
        // A limit name is the device's own, and some run past fifty characters.
        // A panel that opened at a fixed width would wrap them, so every fact
        // has to come out on one row.
        let info = sample_gpu_info();
        for screen in [(1280.0, 577.0), (800.0, 600.0), (390.0, 844.0)] {
            let ctx = egui::Context::default();
            // Two passes: the first measures the content, the second lays the
            // window out against that measurement.
            let _ = gpu_info_rows(&ctx, screen, &info, None);
            for (text, rows) in gpu_info_rows(&ctx, screen, &info, None) {
                assert_eq!(
                    rows, 1,
                    "at {screen:?} the panel wrapped {text:?} onto {rows} rows"
                );
            }
        }
    }

    #[test]
    fn the_gpu_info_panel_wraps_a_fact_wider_than_the_window() {
        // The panel's opening width comes from its facts, so a fact wider than
        // the window cannot be kept on one row: it is wrapped inside the window
        // instead of the panel hanging off the edge.
        let mut info = sample_gpu_info();
        info.limits
            .push(("max_long_limit_name".to_owned(), "x".repeat(200)));
        let screen = (800.0, 600.0);
        let ctx = egui::Context::default();
        let _ = gpu_info_rows(&ctx, screen, &info, None);
        let lines = gpu_info_rows(&ctx, screen, &info, None);

        let panel = area_rect(&ctx, GPU_INFO_AREA);
        assert!(
            panel.right() <= screen.0,
            "the panel {panel:?} runs off the right of the {screen:?} screen"
        );
        assert!(
            lines
                .iter()
                .any(|(text, rows)| text.starts_with("max_long_limit_name") && *rows > 1),
            "the oversized fact was not wrapped inside the window"
        );
    }

    #[test]
    fn the_gpu_info_panel_can_be_dragged_by_its_title() {
        // The panel is a window, not a pinned overlay: a user has to be able to
        // move it off whatever it covers. The drag is a gesture spread over
        // several passes, as egui sees a real one.
        let ctx = egui::Context::default();
        let info = sample_gpu_info();
        let screen = (800.0, 600.0);
        let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());
        let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());
        let opened = area_rect(&ctx, GPU_INFO_AREA);

        // Grab the title bar and pull the window up and to the right.
        let from = egui::pos2(opened.center().x, opened.top() + 8.0);
        let to = from + egui::vec2(60.0, -40.0);
        let _ = gpu_info_pass(&ctx, screen, &info, None, drag_start(from));
        let _ = gpu_info_pass(&ctx, screen, &info, None, drag_move(to));
        let _ = gpu_info_pass(&ctx, screen, &info, None, drag_release(to));

        let moved = area_rect(&ctx, GPU_INFO_AREA);
        assert!(
            (moved.min - opened.min - (to - from)).length() < 1.0,
            "the panel did not follow the drag: opened {opened:?}, moved {moved:?}"
        );
    }

    #[test]
    fn the_gpu_info_panel_prints_the_adapter_features_and_limits() {
        let ctx = egui::Context::default();
        let info = sample_gpu_info();
        let _ = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new());
        let text = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new()).join("\n");

        for expected in [
            "Test Adapter",
            "vulkan · discrete GPU · test driver",
            "driver info: 1.2.3",
            "Features (2)",
            "DEPTH_CLIP_CONTROL",
            "TEXTURE_COMPRESSION_BC",
            "Limits (1)",
            "max_bind_groups = 4",
        ] {
            assert!(
                text.contains(expected),
                "the panel does not show {expected:?}"
            );
        }
    }

    #[test]
    fn the_gpu_info_panel_says_none_when_no_feature_is_enabled() {
        let ctx = egui::Context::default();
        let mut info = sample_gpu_info();
        info.features.clear();
        let _ = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new());
        let text = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new()).join("\n");
        assert!(text.contains("Features (0)"), "the count is still stated");
        assert!(text.contains("none"), "an empty feature list is stated");
    }

    #[test]
    fn the_fullscreen_button_offers_the_action_that_is_not_in_effect() {
        let ctx = egui::Context::default();
        // Two passes, because an anchored area is laid out against the size the
        // previous pass left it.
        for active in [false, true] {
            let _ = fullscreen_pass(&ctx, (800.0, 600.0), active, Vec::new());
            let (clicked, labels) = fullscreen_pass(&ctx, (800.0, 600.0), active, Vec::new());
            assert!(!clicked, "nothing was pressed");
            let label = labels.join(" ");
            if active {
                assert!(
                    label.contains("Exit fullscreen"),
                    "leaving is offered while fullscreen, got {label:?}"
                );
            } else {
                assert!(
                    label.contains("Fullscreen") && !label.contains("Exit"),
                    "entering is offered while windowed, got {label:?}"
                );
            }
        }
    }

    #[test]
    fn the_fullscreen_button_reports_a_press() {
        let ctx = egui::Context::default();
        let screen = (800.0, 600.0);
        // An anchored area is laid out at the position the previous pass gave
        // it, and it is only pinned to its corner from the pass after the
        // first, so the tap is aimed at where two passes have put it.
        let _ = fullscreen_pass(&ctx, screen, false, Vec::new());
        let _ = fullscreen_pass(&ctx, screen, false, Vec::new());
        let (press, release) = tap(area_centre(&ctx, FULLSCREEN_AREA));

        // A release with no press before it is not a click, so the request is
        // not raised by a stray event.
        let (clicked, _) = fullscreen_pass(&ctx, screen, false, release.clone());
        assert!(!clicked, "a release alone is not a click");
        // Neither is a press the user has not let go of yet.
        let (clicked, _) = fullscreen_pass(&ctx, screen, false, press);
        assert!(!clicked, "a press alone is not a click");
        let (clicked, _) = fullscreen_pass(&ctx, screen, false, release);
        assert!(clicked, "the release inside the button is a click");
    }

    #[test]
    fn the_fullscreen_button_grows_to_its_wider_label() {
        let ctx = egui::Context::default();
        // The area remembers the size of the narrower enter label, which is
        // what it is laid out against when the exit label is first drawn.
        let _ = fullscreen_pass(&ctx, (800.0, 600.0), false, Vec::new());
        let (_, enter_labels) = fullscreen_pass(&ctx, (800.0, 600.0), false, Vec::new());
        assert_eq!(enter_labels.len(), 1, "the enter label is one line");
        let enter = area_rect(&ctx, FULLSCREEN_AREA);
        let _ = fullscreen_pass(&ctx, (800.0, 600.0), true, Vec::new());
        let leaving = area_rect(&ctx, FULLSCREEN_AREA);
        // A wrapped label makes the button a row taller, which is the bug: the
        // exit label is wider than the enter one, not taller than it.
        assert!(
            (leaving.height() - enter.height()).abs() < 1.0,
            "the exit label wrapped: {enter:?} became {leaving:?}"
        );
        assert!(
            leaving.width() > enter.width(),
            "the exit label is the wider of the two: {enter:?} to {leaving:?}"
        );
    }

    /// The screens the corner overlays are laid out on: wide enough for all
    /// three to share a row, and narrow ones — a portrait phone — where they
    /// have to stack.
    const OVERLAY_SCREENS: [(f32, f32); 4] = [
        (1280.0, 577.0),
        (800.0, 600.0),
        (390.0, 844.0),
        (320.0, 480.0),
    ];

    /// Lay the three corner overlays out on `screen` until their anchored
    /// positions settle, and return their rectangles.
    ///
    /// Each of them anchors against what the previous pass remembered, so one
    /// pass proves nothing: the layout is only settled once every area has a
    /// geometry for the next one to read.
    fn corner_rects(
        ctx: &egui::Context,
        screen: (f32, f32),
    ) -> (egui::Rect, egui::Rect, egui::Rect) {
        let text = "60.0 fps · 16.7 ms";
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(screen.0, screen.1),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                frame_rate_readout(ui, text);
                fullscreen_button(ui, false);
                api_docs_button(ui);
            });
            output.textures_delta.clear();
        }
        (
            area_rect(ctx, FRAME_RATE_AREA),
            area_rect(ctx, FULLSCREEN_AREA),
            area_rect(ctx, API_DOCS_AREA),
        )
    }

    #[test]
    fn the_corner_overlays_do_not_cover_each_other() {
        // The readout sits centred at the top, the fullscreen button anchors to
        // the top-right corner and the docs link under it. On a window wide
        // enough for them they share the top row, and on a narrow one — a
        // portrait phone — the button drops under the readout rather than hide
        // the end of the reading. The link then follows the button down.
        for screen in OVERLAY_SCREENS {
            let ctx = egui::Context::default();
            let (readout, button, docs) = corner_rects(&ctx, screen);
            for (first, second) in [(readout, button), (readout, docs), (button, docs)] {
                assert!(
                    !first.intersects(second),
                    "at {screen:?} {first:?} and {second:?} share screen space"
                );
            }

            // The readout is centred, its top a margin from the window's top.
            assert!(
                (readout.center().x - screen.0 / 2.0).abs() < 1.0,
                "at {screen:?} the readout {readout:?} is not centred"
            );
            assert!(
                (readout.top() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the readout {readout:?} does not keep the top margin"
            );

            // Both corner buttons keep the margin from the right edge and stay
            // on screen, the link under the button.
            for (name, rect) in [("button", button), ("docs", docs)] {
                assert!(
                    (screen.0 - rect.right() - OVERLAY_MARGIN).abs() < 1.0,
                    "at {screen:?} the {name} {rect:?} does not keep the right margin"
                );
                assert!(
                    rect.bottom() <= screen.1,
                    "at {screen:?} the {name} {rect:?} runs off the bottom"
                );
            }
            assert!(
                docs.top() >= button.bottom(),
                "at {screen:?} the docs link {docs:?} is not under the button {button:?}"
            );

            // Wide windows hold the readout and the button on the top row;
            // narrow ones stack the button under the readout.
            if screen.0 >= 800.0 {
                assert!(
                    (button.top() - OVERLAY_MARGIN).abs() < 1.0,
                    "at {screen:?} the button {button:?} should share the top row"
                );
            } else {
                assert!(
                    button.top() >= readout.bottom(),
                    "at {screen:?} the button {button:?} should drop under the readout {readout:?}"
                );
            }
        }
    }

    #[test]
    fn the_docs_button_asks_for_the_api_docs() {
        // The docs link goes through egui's own URL command, which the UI
        // source publishes in the world; nothing here opens a page.
        let ctx = egui::Context::default();
        let screen = (800.0, 600.0);
        // The link is only under the button once a pass has settled both.
        for _ in 0..2 {
            let _ = corner_rects(&ctx, screen);
        }
        let (press, release) = tap(area_centre(&ctx, API_DOCS_AREA));

        let mut output = ctx.run_ui(raw_input(screen, press), |ui| {
            api_docs_button(ui);
        });
        assert!(
            output.platform_output.commands.is_empty(),
            "a press alone asks for nothing"
        );
        output.textures_delta.clear();

        let mut output = ctx.run_ui(raw_input(screen, release), |ui| {
            api_docs_button(ui);
        });
        let commands = std::mem::take(&mut output.platform_output.commands);
        output.textures_delta.clear();
        let opened: Vec<(String, bool)> = commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::OpenUrl(open) => Some((open.url.clone(), open.new_tab)),
                _ => None,
            })
            .collect();
        assert_eq!(
            opened,
            vec![(API_DOCS_URL.to_owned(), false)],
            "the release asks for the API docs in this tab"
        );
    }

    #[test]
    fn the_fullscreen_button_is_at_least_a_touch_target() {
        // A control a phone has to be hit with a finger must not shrink to a
        // row of text: the button is grown to the declared minimum, including
        // the padding that centres the label, on every state and width.
        for screen in [(1280.0, 577.0), (390.0, 844.0)] {
            for active in [false, true] {
                let ctx = egui::Context::default();
                for _ in 0..2 {
                    let _ = fullscreen_pass(&ctx, screen, active, Vec::new());
                }
                let button = area_rect(&ctx, FULLSCREEN_AREA);
                assert!(
                    button.width() >= OVERLAY_BUTTON_MIN_SIZE.x
                        && button.height() >= OVERLAY_BUTTON_MIN_SIZE.y,
                    "at {screen:?} {active} the button {button:?} is below the touch-target size"
                );
            }
        }
    }

    /// Every line count the readout laid a text galley out with, for a screen
    /// of `screen` points showing each of `texts` in turn.
    ///
    /// The readout is anchored, and an anchored area lays its contents out
    /// against the size it remembered from the previous pass, so one pass
    /// proves nothing: the failure it is prone to only shows once the text
    /// outgrows what the last pass left room for. `texts` is therefore a
    /// sequence of readings, one per pass, and the first pass — egui's
    /// invisible sizing pass — draws no text at all and contributes nothing.
    fn readout_lines(screen: (f32, f32), texts: &[&str]) -> Vec<usize> {
        let ctx = egui::Context::default();
        let mut lines = Vec::new();
        for text in texts {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(screen.0, screen.1),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| frame_rate_readout(ui, text));
            // egui panics if a texture delta is dropped unapplied; a real frame
            // hands it to the integration, and this test discards it.
            output.textures_delta.clear();
            for clipped in &output.shapes {
                if let egui::Shape::Text(text) = &clipped.shape {
                    lines.push(text.galley.rows.len());
                }
            }
        }
        lines
    }

    #[test]
    fn the_frame_rate_readout_stays_on_one_line() {
        // The reading is re-laid out every frame, starting from the empty
        // placeholder, and it grows a digit as the average settles: the readout
        // must hold one line through all of it, at any window width.
        for screen in [(1600.0, 900.0), (640.0, 480.0), (320.0, 240.0)] {
            for texts in [
                vec!["60.0 fps · 16.7 ms"; 4],
                vec!["— fps"; 4],
                vec![
                    "— fps",
                    "60.0 fps · 16.7 ms",
                    "60.0 fps · 16.7 ms",
                    "100.0 fps · 10.0 ms",
                ],
            ] {
                let lines = readout_lines(screen, &texts);
                assert!(
                    !lines.is_empty(),
                    "the readout drew nothing at {screen:?} {texts:?}"
                );
                assert_eq!(
                    lines,
                    vec![1; lines.len()],
                    "screen {screen:?} text {texts:?}"
                );
            }
        }
    }

    #[test]
    fn a_clock_reaches_its_step_only_after_enough_time() {
        let mut clock = 0.0;
        let step = 0.5;

        // Half the interval is not enough.
        assert!(!tick(&mut clock, step, 0.25));
        // The interval is reached exactly.
        assert!(tick(&mut clock, step, 0.25));
        // The clock keeps the leftover rather than resetting, so the next
        // interval is reached in the time that is left.
        assert!(!tick(&mut clock, step, 0.4));
        assert!(tick(&mut clock, step, 0.1));
    }

    #[test]
    fn a_long_stall_advances_one_step_and_keeps_the_rest() {
        let mut clock = 0.0;
        // A two-second stall over a half-second step consumes one step and
        // leaves the surplus for later, rather than skipping frames.
        assert!(tick(&mut clock, 0.5, 2.0));
        assert_eq!(clock, 1.5, "the surplus stays in the clock");

        // The surplus is spent over the next three calls, one step each.
        assert!(tick(&mut clock, 0.5, 0.0));
        assert!(tick(&mut clock, 0.5, 0.0));
        assert!(tick(&mut clock, 0.5, 0.0));
        assert_eq!(clock, 0.0, "the surplus is used up");
        assert!(!tick(&mut clock, 0.5, 0.0));
    }

    #[test]
    fn the_frame_rate_reads_nothing_until_a_frame_was_measured() {
        let rate = FrameRate::default();
        assert_eq!(rate.mean_seconds(), None);
        assert_eq!(rate.text(), "— fps");
    }

    #[test]
    fn the_frame_rate_averages_its_window_and_reports_both_units() {
        let mut rate = FrameRate::default();
        // A steady 16 ms a frame is a little over 60 fps.
        for _ in 0..FrameRate::WINDOW {
            rate.push(0.016);
        }
        let mean = rate.mean_seconds().expect("a measured frame");
        assert!((mean - 0.016).abs() < 1e-6);
        assert_eq!(rate.text(), "62.5 fps · 16.0 ms");
    }

    #[test]
    fn the_frame_rate_window_slides_rather_than_averaging_the_session() {
        let mut rate = FrameRate::default();
        // A long slow stretch, then enough fast frames to fill the window
        // again: the reading has to follow, or a scene that stalled once would
        // read slow for the rest of the session.
        for _ in 0..FrameRate::WINDOW * 2 {
            rate.push(0.1);
        }
        for _ in 0..FrameRate::WINDOW {
            rate.push(0.01);
        }
        assert_eq!(rate.samples.len(), FrameRate::WINDOW);
        let mean = rate.mean_seconds().expect("a measured frame");
        assert!((mean - 0.01).abs() < 1e-6, "got {mean}");
    }

    #[test]
    fn only_a_scene_with_a_baseline_and_a_window_letterboxes() {
        // The windowed path keeps a declared baseline's aspect.
        let (viewport, content) = letterbox(Some((960, 720)), (1600, 600), true);
        assert_eq!(content, (800, 600), "the camera is built for the region");
        let viewport = viewport.expect("a letterbox is stated");
        assert_eq!((viewport.width, viewport.height), (800.0, 600.0));
        assert_eq!(viewport.x, 400.0, "centred horizontally");

        // A capture never does: it draws at the scene's own size.
        assert_eq!(
            letterbox(Some((960, 720)), (1600, 600), false),
            (None, (1600, 600))
        );

        // A scene with no aspect of its own draws into the whole target, which
        // is what a UI-only scene wants at any size.
        assert_eq!(letterbox(None, (1600, 600), true), (None, (1600, 600)));

        // A target of the baseline's own shape needs no bars.
        let (viewport, content) = letterbox(Some((960, 720)), (960, 720), true);
        assert_eq!(content, (960, 720));
        let viewport = viewport.expect("a letterbox is stated");
        assert_eq!((viewport.x, viewport.y), (0.0, 0.0));
    }
}
