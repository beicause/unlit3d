#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod scenes;

mod cli;
mod overlay;

use overlay::{
    FrameRate, GPU_INFO_AREA, SELECTOR_WINDOW, mount_api_docs, mount_frame_rate, mount_fullscreen,
    mount_gpu_info, mount_selector,
};

#[cfg(target_os = "android")]
pub mod android;

/// The fullscreen button, and the landscape lock that only a browser permits.
mod fullscreen;

use std::io::Write;
use std::process::ExitCode;
use unlit_ecs::{Bundle, DynamicBundle, bundle};

use cli::{Args, Parsed};
use scenes::{Advance, SceneControl};
use unlit_wgpu::resources::ResourceGraph;
use unlit_wgpu::scene::ViewportRect;
use unlit3d::frame::Frame;
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::ui::{UiSource, egui};
use unlit3d::winit::builtin::{
    DisplayHandle, ExitOn, ExitRequest, ExitTrigger, Resumed, WindowSetup, WinitProxy,
    request_redraw_while_foreground, window, winit_input_behaviour,
};
#[cfg(target_os = "android")]
use unlit3d::winit::event::OnSuspended;
use unlit3d::winit::event::{OnUserEvent, OnWindowEvent, WinitHost};
use unlit3d::winit::presenter::SCENE_PASS_ORDER;
use winit::event::WindowEvent;
use winit::event_loop::{ControlFlow, EventLoop, EventLoopProxy};
use winit::window::Window;

/// The number of samples the windowed loop presents every frame with.
const SAMPLE_COUNT: u32 = scenes::spin_cube::SAMPLE_COUNT;

/// The timestep a scene is stepped by when it is drawn without a display.
///
/// A windowed run advances by how long its last frame took, so its pace depends
/// on the machine. Drawing to compare against a stored frame has to be
/// reproducible instead, and this is the fixed step that makes it so.
pub const FIXED_STEP: f32 = 1.0 / 60.0;

/// The order the scene and its presentation path come up at.
///
/// It is the one that sets the frame's skip flag, so it comes before every
/// reader of the flag: a behaviour that reads it states a later order rather
/// than leaving the two to chance.
const READY_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(-40);

/// The order the controls run at: after `ensure_presented`, which may have
/// skipped the frame, and first of the readers that act on the flag, since
/// switching a scene skips the frame for the two after them.
const CONTROLS_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(-30);

/// The order the scene advances at: after the controls, which may have skipped
/// the frame, and before the present, which draws what the advance produced.
const ADVANCE_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(-20);

/// The order the scene's region is restated at: after the advance, and before
/// the library's present, which draws the pass the region belongs to.
const VIEWPORT_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(0);

/// The order the scene world's queued commands are applied at: after the
/// library's present, whose render is what queued them.
const FLUSH_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(20);

/// The order the scene world advances its scene at: after `step_sequence`,
/// which decides whether this frame advances and records the decision for the
/// behaviour that runs it. The value shares nothing with the host world's
/// orders: the two worlds dispatch separately.
const SCENE_ADVANCE_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(20);

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
        .scope_mut(shared)
        .expect("the shared scope exists")
        .spawn(bundle![input.clone()]);
    world
        .root_scope_mut()
        .spawn(bundle![winit_input_behaviour(WinitInput::new(input))]);

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
    world.root_scope_mut().spawn(WindowSetup { attributes });

    world
        .root_scope_mut()
        .spawn(ExitOn(ExitTrigger::CloseRequested));
    world.root_scope_mut().spawn(ExitOn(ExitTrigger::Escape));
    world
        .root_scope_mut()
        .spawn(bundle![request_redraw_while_foreground()]);

    // The application's state and its per-callback behaviours share one
    // entity: every component is its own cell, so a callback writing a sibling
    // state component never conflicts with the behaviour being run.
    let app = world.root_scope_mut().spawn(App {
        scene,
        proxy,
        mcp: args.mcp && MCP_SUPPORTED,
    });
    // The frame's own behaviours. Requesting the GPU states no order: nothing
    // reads what it writes in the same frame. The rest state one, chained from
    // the behaviour that brings the scene up and decides whether the frame runs
    // at all, through the readers of that decision, to the library's present
    // and the flush that follows it.
    world.root_scope_mut().spawn(bundle![request_gpu(app)]);
    world.root_scope_mut().spawn(bundle![ensure_presented(app)]);
    world
        .root_scope_mut()
        .spawn(bundle![serve_scene_controls(app)]);
    world.root_scope_mut().spawn(bundle![advance_scene(app)]);
    world.root_scope_mut().spawn(bundle![restate_viewport(app)]);
    world.root_scope_mut().spawn(bundle![present_frames()]);
    world.root_scope_mut().spawn(bundle![flush_scene(app)]);
    let (pump, redraw) = pump_presented();
    world.root_scope_mut().spawn(bundle![pump]);
    world.root_scope_mut().spawn(bundle![redraw]);
    // A resize is a second `OnWindowEvent` behaviour, so it sits on its own
    // entity: two of one family on an entity would be two borrows of one cell.
    world.root_scope_mut().spawn(bundle![resize_scene(app)]);
    // Serving a user event needs the proxy and the app's state, so it captures
    // the app entity; it is a family of its own, so it sits on its own entity.
    world
        .root_scope_mut()
        .spawn(bundle![handle_user_event(app)]);
    #[cfg(target_os = "android")]
    world
        .root_scope_mut()
        .spawn(bundle![release_on_suspend(app)]);

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

/// The GPU context a scene draws with, requested asynchronously.
///
/// The type is [`unlit_wgpu::context::GpuContext`]'s, shared with the other
/// entry points in this repository. Everything in it is a `Send + Sync` wgpu
/// handle, so the request can run off the event loop's thread and the context
/// can cross back to it. The scene itself cannot cross: its ECS world is
/// single-threaded, so it is built on the main thread once this arrives.
///
/// The context and its device outlive a suspension: rebuilding them would drop
/// every pipeline, buffer and texture the scene holds, for a change the
/// platform only makes to the native window.
use unlit_wgpu::context::GpuContext as Gpu;

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
    /// The frame pass the scene's own sources record into.
    ///
    /// Everything a scene builds hangs under it, which is what puts the scene
    /// in the frame: a pass records the sources in its own scope, so the pass a
    /// source's entity sits under is the pass its scene is drawn in.
    pub pass: Entity,
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

/// The render world an application presents through, under the name it reports
/// to the MCP tools.
///
/// A world is reachable from the host only as a component of an entity, and the
/// name is what the report lists it by: spawning the two together is what makes
/// the scene findable at all.
struct RenderWorld {
    /// The world the scene draws in.
    world: World,
    /// The scene driving it.
    scene: Scene,
    /// The name the MCP tools list it under.
    name: &'static str,
}

impl Bundle for RenderWorld {
    fn name(&self) -> &'static str {
        self.name
    }

    fn into_bundle(self) -> DynamicBundle {
        bundle![self.world, self.scene, WorldName(self.name)]
    }
}

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

/// The entity carrying the presentation path, absent until the scene and
/// surface are ready.
///
/// The path is its own entity rather than a component of the app because the
/// library's frame behaviours find it by querying for a [Presenter]; dropping
/// the entity is how a scene switch gives the path up.
struct PresenterEntity(Option<Entity>);

/// The scene the example starts with; a switch replaces it.
struct InitialScene(&'static scenes::SceneDef);

/// The selector window's rectangle the last time a scene was live, so a scene
/// switch can rebuild it in the same place and at the same size.
struct SelectorPos(Option<egui::Rect>);

/// The GPU-info panel's rectangle the last time a scene was live, so a scene
/// switch can rebuild it where the user left it.
struct GpuInfoPos(Option<egui::Rect>);

/// The application's state, spawned as one object.
///
/// Every component is its own cell, so a callback writing a sibling state
/// component never conflicts with the behaviour being run. The behaviours are
/// spawned beside this entity rather than in it, each handed the handle back, so
/// a callback reaches the state it reads and the proxy it posts its own events
/// through from one place.
struct App {
    /// The scene the application starts with.
    scene: &'static scenes::SceneDef,
    /// The proxy a callback posts an event to the loop it runs on with.
    proxy: EventLoopProxy<UserEvent>,
    /// Whether to serve the world over the Model Context Protocol.
    mcp: bool,
}

impl Bundle for App {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            GpuState::Idle,
            SceneEntity(None),
            PresenterEntity(None),
            InitialScene(self.scene),
            SelectorPos(None),
            GpuInfoPos(None),
            McpEnabled(self.mcp),
            ExitRequest(false),
            WinitProxy(self.proxy),
            FrameSkip::default(),
        ]
    }
}

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
                .root_scope()
                .query_one::<&SelectorPos>(app)
                .ok()
                .and_then(|mut pos| pos.get().0),
            gpu_info: world
                .root_scope()
                .query_one::<&GpuInfoPos>(app)
                .ok()
                .and_then(|mut pos| pos.get().0),
        }
    }
}

/// Whether to serve the world over the Model Context Protocol rather than
/// drive a scene on this thread.
///
/// False on the web and on Android, which have no stdio transport; see
/// [`MCP_SUPPORTED`].
struct McpEnabled(bool);

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

/// The scene's per-frame state, spawned with its own advance.
///
/// The state and the behaviour that moves it live on one entity so a frame
/// advances the scene without `&mut Scene`: the index the sequence step keeps
/// and the clock it accumulates into are read by the advance beside them, and
/// neither half means anything without the other.
struct SceneFrame {
    /// The scene's own per-frame behaviour.
    advance: Advance,
    /// The interval the scene's index advances at, or `None` to advance it once
    /// per drawn frame.
    step: Option<f32>,
}

impl Bundle for SceneFrame {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            SceneAdvance(Some(self.advance)),
            SequenceStep(self.step),
            SequenceClock(0.0),
            SequenceStepped(true),
        ]
    }
}

/// Run the scene's own per-frame behaviour, which needs `&mut World`.
struct RunSceneAdvance {
    /// The entity carrying the scene's [`SceneAdvance`] and sequence state.
    entity: Entity,
    /// The frame the behaviour is advanced by.
    frame: Frame,
}

impl Command for RunSceneAdvance {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let Self { entity, frame } = *self;
        let advance = world
            .root_scope()
            .query_one::<&mut SceneAdvance>(entity)
            .ok()
            .and_then(|mut slot| slot.get().0.take());
        let Some(mut advance) = advance else {
            return Ok(());
        };
        advance(world, frame.index, frame.delta_time, frame.size);
        let _ = world
            .root_scope()
            .query_one::<&mut SceneAdvance>(entity)
            .map(|mut slot| slot.get().0 = Some(advance));
        Ok(())
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
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let Self { entity, command } = *self;
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(entity) else {
            command.fail(format!("host entity {entity:?} carries no world"));
            return Ok(());
        };
        command.dispatch(&mut *nested.get());
        Ok(())
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
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        self.command.dispatch(world);
        Ok(())
    }
}

/// The GPU context, once the asynchronous setup has landed.
fn gpu(world: &World, app: Entity) -> Option<Gpu> {
    let mut state = world.root_scope().query_one::<&GpuState>(app).ok()?;
    match state.get() {
        GpuState::Ready(gpu) => Some(gpu.clone()),
        GpuState::Idle | GpuState::Requested => None,
    }
}

/// Build the presentation path once the scene's queued world has landed.
///
/// The window is created before the GPU request, so the raw surface can travel
/// through the asynchronous setup. `BuildPresenter` creates the library surface
/// from that raw handle on the main thread, after the scene world exists.
struct BuildPresenter {
    app: Entity,
    scene: Entity,
    renderer: Entity,
    pass: Entity,
    gpu: Gpu,
    size: (u32, u32),
    surface: wgpu::Surface<'static>,
    path: PresentationPath,
}

impl Command for BuildPresenter {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let presenter = match build_presenter(
            world,
            Some(self.scene),
            self.renderer,
            self.pass,
            self.surface,
            &self.gpu.instance,
            &self.gpu.adapter,
            self.size,
            self.path,
        ) {
            Ok(presenter) => presenter,
            Err(error) => {
                log::error!("failed to build the presentation path: {error}");
                return Ok(());
            }
        };
        let entity = world.root_scope_mut().spawn(bundle![presenter]);
        world
            .root_scope()
            .query_one::<&mut PresenterEntity>(self.app)?
            .get()
            .0 = Some(entity);
        Ok(())
    }
}

/// Bring the scene and its presentation path up, as far as the state allows.
///
/// Spawning the scene world and building its surface both need `&mut World`,
/// so they are queued. The next frame finishes whichever stage has landed.
fn present(world: &World, app: Entity) {
    let Some(gpu) = gpu(world, app) else {
        return;
    };
    let Some(window) = window(world) else {
        return;
    };
    let window_size = window.inner_size();
    let size = (window_size.width.max(1), window_size.height.max(1));
    let def = world
        .root_scope()
        .query_one::<&InitialScene>(app)
        .expect("the app entity carries its initial scene")
        .get()
        .0;
    let mcp = world
        .root_scope()
        .query_one::<&McpEnabled>(app)
        .is_ok_and(|mut mcp| mcp.get().0);
    let scene_entity = match world
        .root_scope()
        .query_one::<&SceneEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0)
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
            let entity = world.root_scope().queue().spawn(RenderWorld {
                world: scene_world,
                scene,
                name: "render",
            });
            let _ = world
                .root_scope()
                .query_one::<&mut SceneEntity>(app)
                .map(|mut slot| slot.get().0 = Some(entity));
            window.set_title(&format!(
                "unlit3d{} — {}",
                if mcp { " (MCP)" } else { "" },
                def.title
            ));
            return;
        }
    };
    if world
        .root_scope()
        .query_one::<&World>(scene_entity)
        .is_err()
    {
        return;
    }
    let (renderer, pass) = {
        let mut scene = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .expect("the scene entity carries a scene");
        scene.get().resize(size, def.baseline);
        (scene.get().renderer, scene.get().pass)
    };
    if presenter_entity(world, app).is_some() {
        return;
    }
    let surface = match gpu.instance.create_surface(window.clone()) {
        Ok(surface) => surface,
        Err(error) => {
            log::error!("failed to build the presentation surface: {error}");
            return;
        }
    };
    let path = if mcp {
        PresentationPath::Intermediate(TargetConfig {
            samples: def.samples,
            with_depth: def.depth,
            ..TargetConfig::default()
        })
    } else {
        PresentationPath::Surface {
            samples: SAMPLE_COUNT,
            with_depth: true,
        }
    };
    world.root_scope().queue().push(BuildPresenter {
        app,
        scene: scene_entity,
        renderer,
        pass,
        gpu,
        size,
        surface,
        path,
    });
    window.request_redraw();
}

/// The entity carrying the presentation path, while one is up.
fn presenter_entity(world: &World, app: Entity) -> Option<Entity> {
    world
        .root_scope()
        .query_one::<&PresenterEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0)
}

/// Release the swap chain, keeping the presentation path for a later resume.
fn release_surface(world: &World, app: Entity) {
    let Some(entity) = presenter_entity(world, app) else {
        return;
    };
    let released = world
        .root_scope()
        .query_one::<&mut Presenter>(entity)
        .map(|mut presenter| presenter.get().release(world));
    if let Ok(Err(error)) = released {
        log::error!("failed to release the presentation path: {error}");
    }
}

/// Release the presentation path and drop it, before its scene goes.
fn release_presenter(world: &World, app: Entity) {
    release_surface(world, app);
    let entity = world
        .root_scope()
        .query_one::<&mut PresenterEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0.take());
    if let Some(entity) = entity {
        world.root_scope().queue().despawn(entity);
    }
}

#[cfg(target_os = "android")]
fn release(world: &World, app: Entity) {
    release_surface(world, app);
}

/// Rebuild the app around a different scene.
fn switch_scene(world: &World, app: Entity, def: &'static scenes::SceneDef) {
    // The target and blit belong to the scene world, so both go with it.
    // The switching frame skips drawing before the replacement is built.
    release_presenter(world, app);
    if let Some(entity) = world
        .root_scope()
        .query_one::<&mut SceneEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0.take())
    {
        world.root_scope().queue().despawn(entity);
    }
    if let Ok(mut initial) = world.root_scope().query_one::<&mut InitialScene>(app) {
        initial.get().0 = def;
    }
    present(world, app);
}
/// Run the scene world's input dispatch for the frame.
fn dispatch_input_on_frame() -> OnFrame {
    OnFrame::new(|world, _entity, _frame| {
        dispatch_input(world);
    })
}

/// Decide whether this frame advances the scene's sequence.
///
/// A scene with a fixed step advances once its clock has accumulated one; one
/// without advances every frame. The decision is recorded in `SequenceStepped`
/// for the behaviour that runs the advance.
fn step_sequence(entity: Entity) -> OnFrame {
    OnFrame::new(move |world, _entity, frame| {
        let stepped = match world
            .root_scope()
            .query_one::<&SequenceStep>(entity)
            .ok()
            .and_then(|mut step| step.get().0)
        {
            Some(step) => world
                .root_scope()
                .query_one::<&mut SequenceClock>(entity)
                .map(|mut clock| tick(&mut clock.get().0, step, frame.delta_time))
                .expect("the scene frame entity carries a sequence clock"),
            None => true,
        };
        let _ = world
            .root_scope()
            .query_one::<&mut SequenceStepped>(entity)
            .map(|mut flag| flag.get().0 = stepped);
    })
}

/// Run the scene's own per-frame behaviour when the sequence advanced, and
/// count the frame.
///
/// The behaviour needs `&mut World`, which a frame callback does not have, so
/// it is queued; the index is captured before it is counted on, so the
/// behaviour is handed the frame it advances from.
fn run_scene_advance(entity: Entity) -> OnFrame {
    OnFrame::with_order(SCENE_ADVANCE_ORDER, move |world, _entity, frame| {
        let stepped = world
            .root_scope()
            .query_one::<&SequenceStepped>(entity)
            .is_ok_and(|mut flag| flag.get().0);
        if !stepped {
            return;
        }
        world.root_scope().queue().push(RunSceneAdvance {
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
    OnFrame::with_order(READY_ORDER, move |world, _entity, _frame| {
        let resumed = world
            .root_scope()
            .query::<(Entity, &Resumed)>()
            .iter()
            .next()
            .is_some_and(|(_, state)| state.0);
        if !resumed {
            FrameSkip::skip(world);
            return;
        }
        if world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .map_or(true, |mut slot| slot.get().0.is_none())
        {
            present(world, app);
            FrameSkip::skip(world);
            return;
        }
        if presenter_entity(world, app).is_some() {
            return;
        }
        present(world, app);
        if presenter_entity(world, app).is_none() {
            FrameSkip::skip(world);
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
    OnFrame::with_order(CONTROLS_ORDER, move |world, _entity, _frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let active = window(world).is_some_and(|window| fullscreen::active(&window));
        let (selector_pos, gpu_info_pos, switch, fullscreen) = {
            let Ok(mut nested) = world.root_scope().query_one::<&World>(scene_entity) else {
                return;
            };
            let (selector_pos, gpu_info_pos) = {
                let mut scene = world
                    .root_scope()
                    .query_one::<&Scene>(scene_entity)
                    .expect("the scene component exists");
                (
                    scene.get().selector_rect(nested.get()),
                    scene.get().gpu_info_rect(nested.get()),
                )
            };
            let switch = world
                .root_scope()
                .query_one::<&mut Scene>(scene_entity)
                .ok()
                .and_then(|mut scene| scene.get().pending.take());
            let fullscreen = world
                .root_scope()
                .query_one::<&mut Scene>(scene_entity)
                .is_ok_and(|mut scene| scene.get().fullscreen_request(nested.get(), active));
            (selector_pos, gpu_info_pos, switch, fullscreen)
        };
        let _ = world
            .root_scope()
            .query_one::<&mut SelectorPos>(app)
            .map(|mut slot| slot.get().0 = selector_pos);
        let _ = world
            .root_scope()
            .query_one::<&mut GpuInfoPos>(app)
            .map(|mut slot| slot.get().0 = gpu_info_pos);
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
        if let Ok(mut nested) = world.root_scope().query_one::<&World>(scene_entity) {
            for url in drain_open_urls(nested.get()) {
                open_url(&url);
            }
        }
    })
}

/// Advance the scene by the frame's delta.
fn advance_scene(app: Entity) -> OnFrame {
    OnFrame::with_order(ADVANCE_ORDER, move |world, _entity, frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(scene_entity) else {
            return;
        };
        let _ = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .map(|mut scene| {
                scene.get().advance(&mut *nested.get(), frame.delta_time);
            });
    })
}

/// Restate the scene's letterboxed region on the pass that draws it.
///
/// A window resize changes the region, and the library's present renders the
/// pass this frame, so this runs before it. The blit pass, when there is one,
/// keeps its full-target viewport. A skipped frame still restates the region:
/// keeping the pass's viewport in step with the window costs nothing and
/// leaves the pass right for the frame that draws again.
fn restate_viewport(app: Entity) -> OnFrame {
    OnFrame::with_order(VIEWPORT_ORDER, move |world, _entity, _frame| {
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let (scene_pass, viewport) = {
            let mut scene = world
                .root_scope()
                .query_one::<&Scene>(scene_entity)
                .expect("the scene entity carries a scene");
            (scene.get().pass, scene.get().viewport)
        };
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(scene_entity) else {
            return;
        };
        nested
            .get()
            .root_scope()
            .query_one::<&mut FramePass>(scene_pass)
            .map(|mut pass| pass.get().viewport = viewport)
            .expect("the scene's own pass exists");
    })
}

/// Apply the commands the scene world queued while presenting.
///
/// The library's present renders the scene world, so its commands are applied
/// after it -- a later order -- rather than by the presenter itself. A skipped
/// frame still flushes: the commands queue up on the scene world either way,
/// and dropping none of them keeps the scene's own record intact.
fn flush_scene(app: Entity) -> OnFrame {
    OnFrame::with_order(FLUSH_ORDER, move |world, _entity, _frame| {
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(scene_entity) else {
            return;
        };
        let _ = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .map(|mut scene| {
                if let Err(error) = scene.get().end_frame(&mut *nested.get()) {
                    log::error!("a command queued by a scene failed: {error}");
                }
            });
    })
}

/// Request the GPU context once a window exists to present through.
///
/// The window is created by the host at the end of the resume callback, so the
/// request waits for a later frame; a `GpuState` that is no longer idle keeps
/// a later frame from starting it twice.
fn request_gpu(app: Entity) -> OnFrame {
    OnFrame::new(move |world, _entity, _frame| {
        if !world
            .root_scope()
            .query_one::<&GpuState>(app)
            .is_ok_and(|mut state| matches!(state.get(), GpuState::Idle))
        {
            return;
        }
        let Some(window) = window(world) else {
            return;
        };
        let Some(display) = world
            .root_scope()
            .query::<(Entity, &DisplayHandle)>()
            .iter()
            .next()
            .map(|(_, handle)| handle.0.clone())
        else {
            return;
        };
        let Ok(proxy) = world
            .root_scope()
            .query_one::<&WinitProxy<UserEvent>>(app)
            .map(|mut proxy| proxy.get().0.clone())
        else {
            return;
        };
        let _ = world
            .root_scope()
            .query_one::<&mut GpuState>(app)
            .map(|mut state| *state.get() = GpuState::Requested);
        // The display handle is owned, not borrowed: the instance it builds has
        // to outlive this callback. The descriptor is the environment-aware one
        // so `WGPU_BACKEND` still selects a backend.
        spawn(async move {
            let instance = wgpu::util::new_instance_with_webgpu_detection(
                wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)),
            )
            .await;
            // A high-performance adapter, so a machine with two GPUs draws with
            // the discrete one; `UNLIT3D_DEVICE_TIER` decides how much of the
            // WebGPU baseline the device is asked for.
            let request = Gpu::present_to(
                instance,
                window,
                DeviceTier::from_env(),
                wgpu::PowerPreference::HighPerformance,
            )
            .await;
            match request {
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
            let _ = world
                .root_scope()
                .query_one::<&mut GpuState>(app)
                .map(|mut state| {
                    *state.get() = GpuState::Ready(gpu.clone());
                });
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
            if world
                .root_scope()
                .query_one::<&McpEnabled>(app)
                .is_ok_and(|mut mcp| mcp.get().0)
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
                .root_scope()
                .query_one::<&mut ExitRequest>(app)
                .map(|mut request| request.get().0 = true);
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
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let baseline = world
            .root_scope()
            .query_one::<&InitialScene>(app)
            .expect("the app entity carries its initial scene")
            .get()
            .0
            .baseline;
        let _ = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .map(|mut scene| scene.get().resize(size, baseline));
        let Some(presenter) = presenter_entity(world, app) else {
            return;
        };
        let resized = world
            .root_scope()
            .query_one::<&mut Presenter>(presenter)
            .map(|mut presenter| presenter.get().resize(world, size));
        if let Ok(Err(error)) = resized {
            log::error!("failed to resize the presentation path: {error}");
        }
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
    let Ok(proxy) = world
        .root_scope()
        .query_one::<&WinitProxy<UserEvent>>(app)
        .map(|mut proxy| proxy.get().0.clone())
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
        for (entity, _) in world.root_scope().query::<(Entity, &World)>().iter() {
            let Ok(mut name) = world.root_scope().query_one::<&WorldName>(entity) else {
                continue;
            };
            let name = name.get();
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
            .root_scope()
            .queue()
            .push(RunInWorld { entity, command }),
        None => world.root_scope().queue().push(RunInHost { command }),
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
        let renderer = world
            .root_scope_mut()
            .spawn(bundle![Renderer::new(context)]);
        // The pass the scene draws in. Everything the scene builds hangs under
        // it — its sources, and the UI source mounted below — because a pass
        // records the sources in its own scope: the pass a source sits under is
        // the pass its drawing lands in. Its order is the first of the frame,
        // so a presenting pass mounted later draws after it.
        let load = RenderLoadOps {
            color: match def.clear {
                Some(color) => wgpu::LoadOp::Clear(color),
                None => color_clear(),
            },
            ..RenderLoadOps::default()
        };
        let pass = world.root_scope_mut().spawn(Pass {
            load,
            ..Pass::new(SCENE_PASS_ORDER)
        });
        // The scene's content is built for the region it will be drawn into,
        // not for the whole target: a camera built for the target's aspect
        // would be sheared back out to it by the viewport. A run that does not
        // letterbox — every capture — draws into the whole target, so its
        // content size is the target's own and nothing about it changes.
        let (viewport, content) = letterbox(def.baseline, size, options.letterbox);
        // The pass carries the region as well as the scene: the driver asks the
        // pass where its sources draw, so a viewport kept only on this struct
        // would never reach the renderer and the content would stretch to the
        // whole target.
        world
            .root_scope()
            .query_one::<&mut FramePass>(pass)
            .map(|mut pass| pass.get().viewport = viewport)
            .expect("the pass was just spawned");

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
            world
                .scope_mut(pass)
                .expect("the pass was just spawned")
                .spawn(bundle![Source::new(source_ui)])
        });
        let control = (def.build)(&mut world, context, pass, ui, content, options);
        let SceneControl { advance, snapshot } = control;

        // The shell's own state — the selector's switch, the fullscreen
        // request and its state, the frame-rate reading — lives in the shared
        // subtree. A panel reads and writes it from the UI source's scope,
        // which reaches the shared subtree and nothing beside it.
        let shared = world.shared();
        let (switch, fullscreen, fullscreen_active, frame_rate) = {
            let mut shared_view = world.scope_mut(shared).expect("the shared scope exists");
            (
                shared_view.spawn(bundle![SceneSwitch(None)]),
                shared_view.spawn(bundle![FullscreenRequest(false)]),
                shared_view.spawn(bundle![FullscreenActive(false)]),
                shared_view.spawn(bundle![FrameRate::default()]),
            )
        };
        // The scene's per-frame state and its own behaviour, as components: the
        // frame's behaviours advance them without `&mut Scene`. One entity
        // carries them so the behaviour that advances the scene and the state
        // it reads are reached together.
        let frame_entity = world.root_scope_mut().spawn(SceneFrame {
            advance,
            step: options.sequence_step,
        });
        // The frame's own dispatch: the input runs first and states no order,
        // since nothing else here reads what it dispatched. Stepping the index
        // and advancing by the frame it names are the one dependency: the
        // advance states an order after the step that records it.
        world
            .root_scope_mut()
            .spawn(bundle![dispatch_input_on_frame()]);
        world
            .root_scope_mut()
            .spawn(bundle![step_sequence(frame_entity)]);
        world
            .root_scope_mut()
            .spawn(bundle![run_scene_advance(frame_entity)]);
        // A scene world has no window: it starts whole and nothing short-
        // circuits it. Spawning the flag lets a scene behaviour do so.
        world.root_scope_mut().spawn(bundle![FrameSkip::default()]);

        if let Some(ui) = ui
            && options.selector
        {
            mount_selector(&mut world, ui, switch, def, panels.selector);
            mount_frame_rate(&mut world, ui, frame_rate);
            let device = context
                .device(&world.root_scope())
                .expect("the device resource");
            mount_gpu_info(&mut world, ui, device, panels.gpu_info);
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
                pass,
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
    /// size, so the projection and the size the scene's behaviour is handed
    /// follow the target together. The region is the scene's own state here and
    /// reaches the pass that draws it from [`restate_viewport`], which is the one
    /// place holding both that pass and this scene.
    fn resize(&mut self, target: (u32, u32), baseline: Option<(u32, u32)>) {
        let (viewport, content) = letterbox(baseline, target, true);
        self.size = content;
        self.viewport = viewport;
    }

    /// Advance the scene by `delta_time` seconds.
    ///
    /// A frame is the dispatch of the scene world's `OnFrame` behaviours. The
    /// input and the index state no order; the scene's own behaviour states one
    /// after the index, since it advances by the frame the index names. The
    /// index and the content size travel in the frame's context, so a behaviour
    /// reads them without a component of its own.
    pub fn advance(&mut self, world: &mut World, delta_time: f32) {
        // Measured before anything else, so the reading covers the whole frame
        // — input, behaviour, render and present — and not just the part
        // between here and the next call.
        world
            .root_scope()
            .query_one::<&mut FrameRate>(self.frame_rate)
            .map(|mut rate| rate.get().push(delta_time))
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
            .root_scope()
            .query_one::<&mut SceneSwitch>(self.switch)
            .map(|mut s| s.get().0.take())
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
            .root_scope()
            .query::<(Entity, &Source)>()
            .iter()
            .find_map(|(_, source)| {
                let ui = source.as_ref::<UiSource>().ok()?;
                ui.context()
                    .memory(|memory| memory.area_rect(egui::Id::unique(id)))
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
            .root_scope()
            .query_one::<&mut FullscreenActive>(self.fullscreen_active)
            .map(|mut state| state.get().0 = active);
        world
            .root_scope()
            .query_one::<&mut FullscreenRequest>(self.fullscreen)
            .map(|mut request| std::mem::take(&mut request.get().0))
            .expect("the fullscreen request component exists")
    }

    /// Render one frame into whatever target the caller bound.
    ///
    /// Only the snapshot tests draw through this: the windowed run acquires
    /// the swap chain's image and renders between the acquire and the present,
    /// so it needs both under one borrow.
    pub fn render(&mut self, world: &World) {
        world
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)
            .map(|mut renderer| renderer.get().render(world))
            .expect("the renderer is a resource entity");
    }

    /// Finish the frame: apply whatever a behaviour component or panel queued.
    ///
    /// Nothing is cleared: every consumer reads the events from its own
    /// subscription, and the dispatch that reads them takes them.
    pub fn end_frame(&mut self, world: &mut World) -> Result<(), EntityAccessError> {
        world.apply()
    }
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
    use super::{letterbox, tick};
    // The scene-switch test below drives the app's own slots, so it is the only
    // part of this module that needs these names.
    #[cfg(not(target_arch = "wasm32"))]
    use super::{InitialScene, PresenterEntity, SceneEntity, scenes, switch_scene};
    #[cfg(not(target_arch = "wasm32"))]
    use unlit_ecs::bundle;
    #[cfg(not(target_arch = "wasm32"))]
    use unlit3d::prelude::World;
    /// A scene switch drops the presentation path, which was built around the
    /// scene it replaced.
    ///
    /// The path's surface and the pass that presents through it belong to the
    /// dying scene's world, so the rebuilt scene builds a fresh one instead of
    /// presenting through a pass that went with the world it drew.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_scene_switch_drops_the_presentation_path() {
        let mut world = World::new();

        let old_scene = world.root_scope_mut().spawn(bundle![]);
        let old_presenter = world.root_scope_mut().spawn(bundle![]);
        let app = world.root_scope_mut().spawn(bundle![
            InitialScene(scenes::default()),
            PresenterEntity(Some(old_presenter)),
            SceneEntity(Some(old_scene)),
        ]);

        let next = scenes::by_id("ui_only").expect("the example ships a UI-only scene");
        switch_scene(&world, app, next);
        world.apply().unwrap();

        assert!(
            world
                .root_scope()
                .query_one::<&PresenterEntity>(app)
                .expect("the app keeps its presentation slot")
                .get()
                .0
                .is_none(),
            "the presentation path went with the scene it presented"
        );
        assert!(
            !world.root_scope().contains(old_presenter),
            "the presentation path's entity was despawned"
        );
        assert!(
            !world.root_scope().contains(old_scene),
            "the old scene was despawned"
        );
        assert_eq!(
            world
                .root_scope()
                .query_one::<&InitialScene>(app)
                .expect("the app keeps its initial scene")
                .get()
                .0
                .id,
            "ui_only"
        );
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
