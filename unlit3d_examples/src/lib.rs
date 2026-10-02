#![doc = include_str!("../README.md")]
#![warn(unsafe_code)]

pub mod scenes;

mod cli;

#[cfg(target_os = "android")]
pub mod android;

/// The browser's fullscreen button, and the landscape lock that only
/// fullscreen permits. Nothing is offered away from the web.
mod web;

use std::collections::VecDeque;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use cli::{Args, Parsed};
use scenes::SceneControl;
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::ui::{UiSource, egui};
use unlit3d::winit::WindowSurface;
// `std::time::Instant` panics on `wasm32-unknown-unknown`, where the standard
// library has no clock; `web-time` reads the browser's `Performance.now()`
// there and re-exports `std::time` everywhere else.
use unlit_wgpu::resources::ResourceGraph;
use unlit_wgpu::scene::ViewportRect;
use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

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
    use unlit_wgpu::render_attachments::create_render_target;
    use unlit_wgpu::resources::TextureExt;

    let target = create_render_target(device, OFFSCREEN_FORMAT, size.0, size.1, samples);
    let default_view = |texture: &wgpu::Texture| {
        TextureExt::create_view(texture, &wgpu::TextureViewDescriptor::default())
    };

    let (color_view, depth_view, msaa_view) = {
        let mut graph = world
            .get_mut::<ResourceGraph>(renderer.context().graph)
            .expect("the context's resource graph exists");
        let insert = |graph: &mut ResourceGraph, view: unlit_wgpu::resources::TextureView| {
            graph.insert(view, None)
        };
        let color = insert(&mut graph, default_view(&target.color));
        let depth = with_depth.then(|| insert(&mut graph, default_view(&target.depth)));
        let msaa = target
            .msaa
            .as_ref()
            .map(|msaa| insert(&mut graph, default_view(msaa)));
        (color, depth, msaa)
    };
    renderer.set_render_target(world, Some(color_view), depth_view, msaa_view);

    target.color
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
    let app = App {
        proxy: event_loop.create_proxy(),
        window: None,
        context: GpuState::Idle,
        scene: None,
        surface: None,
        foreground: false,
        // Overwritten by the first frame, so its delta — the gap between
        // startup and that frame — is not mistaken for a frame's own.
        last_frame: Instant::now(),
        initial_size: args.size.unwrap_or(scene.size),
        initial_scene: scene,
        selector_pos: None,
    };

    // Native runs the loop on this thread. The web hands the app to the
    // browser instead, which drives it from its own event callbacks — the
    // loop cannot be run to completion there.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut app = app;
        event_loop.run_app(&mut app).expect("the event loop runs");
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
    }
}

/// Events delivered to the winit loop from outside a `WindowEvent`.
enum UserEvent {
    /// The async GPU setup finished; the scene is built from it here, on the
    /// main thread.
    Ready(Gpu),
    /// The async GPU setup failed; the message is reported and the app exits.
    Failed(String),
}

/// The application.
///
/// The state is split by how long it lives, because a suspension does not reset
/// all of it. The window handle and the GPU context outlive a suspension; the
/// ECS world outlives it too, and holds everything the current scene has — the
/// spin angle, the camera orbit, the panel values. Only the swap chain, which a
/// suspension does invalidate, is dropped and built again.
///
/// Switching scenes is a rebuild of that world: the old scene's world and the
/// surface presenting it go together, and the new scene is built with the same
/// GPU context and window.
struct App {
    /// Sends the async GPU setup's result back to the loop.
    proxy: EventLoopProxy<UserEvent>,
    /// The window, created at the first resume and kept for the app's life.
    ///
    /// The platform's native window comes and goes with a suspension — Android
    /// destroys it and hands a new one back — but this handle outlives that,
    /// and the surface the scene presents through is built from it again.
    window: Option<Arc<Window>>,
    /// Where the one-time asynchronous GPU setup stands.
    context: GpuState,
    /// The scene, live once the GPU context arrived, and kept across a
    /// suspension so the world's state survives it.
    scene: Option<Scene>,
    /// The swap chain the scene presents through, absent while suspended.
    surface: Option<WindowSurface>,
    /// Whether the platform currently has a native window to present into.
    ///
    /// False between a suspension and the resume that ends it, which is the
    /// stretch in which no surface can be built.
    foreground: bool,
    /// The time the previous frame was drawn at, for the frame delta.
    last_frame: Instant,
    /// The window's initial size, from the command line or the scene's own.
    initial_size: (u32, u32),
    /// The scene the example starts with; a switch replaces it.
    initial_scene: &'static scenes::SceneDef,
    /// Where the selector window was left, so a scene switch can rebuild it
    /// at the same place.
    selector_pos: Option<egui::Pos2>,
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

impl GpuState {
    /// The context, once the setup landed.
    fn ready(&mut self) -> Option<&mut Gpu> {
        match self {
            Self::Ready(gpu) => Some(gpu),
            Self::Idle | Self::Requested => None,
        }
    }
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
pub struct Scene {
    /// The ECS world the scene's content lives in.
    pub world: World,
    /// The renderer resource entity: the handle every renderer access goes
    /// through.
    pub renderer: Entity,
    /// Translates the window's events into the world's input events. The
    /// snapshot tests never feed it, so its state stays idle.
    input: WinitInput,
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
    /// The scene's per-frame behaviour and snapshot table.
    pub control: SceneControl,
    /// The frame-rate measurement the windowed shell's readout displays.
    ///
    /// Unused by the snapshot tests, which have no display to report a rate on.
    frame_rate: Entity,
    /// The frame index the scene's behaviour is handed.
    frame: u32,
    /// The interval the index advances at, in seconds, or `None` to advance it
    /// once per drawn frame.
    ///
    /// The snapshot tests advance it every frame so a capture draws exactly the
    /// frames its snapshots froze; the windowed path hands a scene that froze a
    /// sequence a longer interval, so the sequence plays at a watchable pace
    /// instead of one frame per display refresh.
    sequence_step: Option<f32>,
    /// Seconds accumulated towards the next advance of `frame`, used only when
    /// `sequence_step` is set.
    sequence_clock: f32,
    /// The scene-selector's switch component, read once per frame.
    switch: Entity,
    /// The fullscreen button's request component, read once per frame.
    fullscreen: Entity,
    /// A scene switch requested by the selector, consumed by the frame loop.
    pending: Option<&'static scenes::SceneDef>,
}

/// Set by the windowed shell's scene-selector panel, read once by the frame
/// loop.
struct SceneSwitch(Option<&'static scenes::SceneDef>);

/// Set by the windowed shell's fullscreen button, read once by the frame loop.
///
/// A flag rather than the state to move to: the button asks for the document to
/// change, and which way it changes is read from the document itself at the
/// moment the request is served. A panel runs while the world is borrowed to
/// lay the UI out, so it can only record the click; the frame loop owns the
/// window the request is served on. See [`web`].
struct FullscreenRequest(bool);

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Already foreground: a redundant resume, which platforms are allowed
        // to send, must not open a second window or build a second surface.
        if self.foreground {
            return;
        }

        // The window outlives a suspension, so it is opened once and only the
        // surface is rebuilt after one. Android destroys the native window and
        // hands a new one back to the same `Window`, so there is nothing to
        // reopen.
        if self.window.is_none() {
            #[cfg_attr(
                not(target_arch = "wasm32"),
                expect(unused_mut, reason = "wasm32 reassigns to append the canvas")
            )]
            let mut attributes = Window::default_attributes()
                .with_title("unlit3d + winit")
                .with_inner_size(winit::dpi::LogicalSize::new(
                    self.initial_size.0,
                    self.initial_size.1,
                ));

            // winit creates the canvas but does not put it in the page; without
            // this the web build would render to nothing visible.
            #[cfg(target_arch = "wasm32")]
            {
                use winit::platform::web::WindowAttributesExtWebSys;
                attributes = attributes.with_append(true);
            }

            let window = match event_loop.create_window(attributes) {
                Ok(window) => Arc::new(window),
                Err(error) => {
                    log::error!("failed to open a window: {error}");
                    event_loop.exit();
                    return;
                }
            };
            self.window = Some(window);
        }
        self.foreground = true;
        // The frame clock restarts here rather than on the last frame, so the
        // stretch the app spent suspended — which is arbitrarily long, and no
        // frame's own — does not reach the scene as one frame's delta.
        self.last_frame = Instant::now();

        // The GPU context is requested once, against the first surface. Its
        // adapter, device and everything built from them survive a suspension,
        // so a later resume only needs a surface for the window it already
        // has; see `Self::present`. `Requested` is what keeps a redundant
        // resume from starting the request twice.
        if matches!(self.context, GpuState::Idle) {
            self.context = GpuState::Requested;
            let window = self.window.clone().expect("the window was just opened");

            // The display handle is taken here, on the thread that owns the
            // event loop, and *owned* so the instance can outlive this borrow.
            // It is what the GLES and WebGL2 backends use to reach the
            // platform's display connection, and it is passed through the
            // environment-aware descriptor, so `WGPU_BACKEND` and friends still
            // select the backend by name.
            let display = event_loop.owned_display_handle();
            let proxy = self.proxy.clone();

            // Everything else runs off the event loop's thread (native) or in
            // the browser's task queue (web): the adapter and device requests
            // are asynchronous on both, and so is finding out whether the
            // browser really has WebGPU.
            //
            // `new_instance_with_webgpu_detection` is what makes that choice.
            // WebGPU support has to be settled when the instance is created —
            // the `navigator.gpu` object alone is not enough, since a browser
            // may expose it and still fail to produce an adapter — so this asks
            // for one before committing and drops the WebGPU backend if there
            // is none. The WebGL2 backend then serves the frame. Building the
            // instance with `Instance::new` instead would commit to WebGPU on
            // the strength of the property alone.
            //
            // The surface is created here rather than on the event loop's
            // thread for the same reason: it needs the instance, and the
            // adapter is required to be able to present to it. It is a *probe*,
            // dropped again by `Gpu::request`; whoever presents builds one for
            // the window it has at that moment.
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
        }

        self.present();
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.foreground = false;
        // Android destroys the app's `SurfaceView` when it is suspended, and
        // wgpu requires every surface drawn from it to be dropped before this
        // callback returns. iOS and the web only freeze the app — their canvas
        // outlives the suspension — so theirs is kept.
        //
        // Only the swap chain goes, never the world it presented: the state
        // the app is resumed into is the state it was suspended in.
        #[cfg(target_os = "android")]
        self.release();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Ready(gpu) => {
                // Kept rather than dropped when this lands while suspended:
                // the context is worth keeping, and only the surface is
                // invalid then. `present` builds what it can with it.
                self.context = GpuState::Ready(gpu);
                self.present();
            }
            UserEvent::Failed(error) => {
                log::error!("failed to start the renderer: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        // Every window event goes to the input adapter first, so the frame
        // that follows sees this event whichever branch handles it below. The
        // adapter ignores the events that carry no input.
        scene.input.on_window_event(&scene.world, &event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                // The swap chain and its attachments are rebuilt for the new
                // size below, and the camera's projection follows the new
                // aspect on the next frame so nothing is stretched.
                if size.width == 0 || size.height == 0 {
                    return;
                }
                scene.resize((size.width, size.height), self.initial_scene.baseline);
                self.resize_surface(size.width, size.height);
            }
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Ask for another frame every turn of the loop, so the scene animates.
        // Nothing is asked for while suspended: the app is not being looked at,
        // and on Android its surface is gone until the next resume.
        if self.foreground
            && self.surface.is_some()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}

impl App {
    /// Build whatever the context, the window and the foreground allow.
    ///
    /// Every one of the three arrives on its own schedule — the context from
    /// the async setup, the window and the foreground from the platform's
    /// lifecycle — so this is called after each and does nothing until all
    /// three are here. The scene is built once and outlives every suspension;
    /// the swap chain is built, released and built again around them.
    ///
    /// The scene is the one [`Self::initial_scene`] names — the command line's
    /// `--scene`, replaced when the selector panel requests a switch.
    fn present(&mut self) {
        let (Some(context), Some(window)) = (self.context.ready(), self.window.clone()) else {
            return;
        };
        if !self.foreground {
            return;
        }

        // A window that has not been laid out yet reports nothing, and the scene
        // divides by its size for the camera's aspect — a zero there is a NaN
        // projection. The real size arrives as a resize and corrects this.
        let size = window.inner_size();
        let size = (size.width.max(1), size.height.max(1));

        // The selector's position survives a switch in the shell rather than
        // the scene: egui remembers it in the context the scene's world
        // carries, and that context is rebuilt with the scene. Read before the
        // `&mut self.scene` borrow below — `Pos2` is `Copy`.
        let selector_pos = self.selector_pos;

        let scene = match &mut self.scene {
            // Kept across a suspension, so the world's state survives it.
            Some(scene) => scene,
            None => {
                let def = self.initial_scene;
                self.scene = Some(Scene::new(
                    context.device.clone(),
                    context.queue.clone(),
                    context.capabilities,
                    size,
                    scenes::SceneOptions {
                        ui: def.ui,
                        selector: true,
                        reproducible: false,
                        // A window of any shape shows the same picture, scaled
                        // to whatever fits.
                        letterbox: true,
                        // The windowed loop holds each frame of a fixed
                        // sequence for the scene's own step, so it plays at a
                        // watchable pace.
                        sequence_step: def.step_seconds,
                    },
                    def,
                    selector_pos,
                ));
                if let Some(window) = &self.window {
                    window.set_title(&format!("unlit3d — {}", def.title));
                }
                self.scene.as_mut().expect("the scene was just built")
            }
        };
        scene.resize(size, self.initial_scene.baseline);

        // A surface is built once per foreground stretch: the one kept from
        // before a suspension has only to follow the window it was made from,
        // which a resume may have replaced at another size.
        if self.surface.is_some() {
            self.resize_surface(size.0, size.1);
            return;
        }

        // Built fresh for the window in hand, which is not necessarily the one
        // the adapter was probed against: a suspension replaces the native
        // window, and a surface belongs to the window it was made from.
        let surface = match context.instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                log::error!("failed to build the presentation surface: {error}");
                return;
            }
        };
        let (world, renderer) = (&scene.world, scene.renderer);
        let window_surface = world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                WindowSurface::new(
                    world,
                    renderer,
                    &context.instance,
                    &context.adapter,
                    window,
                    surface,
                    SAMPLE_COUNT,
                )
            })
            .expect("the renderer is a resource entity");
        self.surface = Some(window_surface);
        self.surface
            .as_ref()
            .expect("the surface was just built")
            .window()
            .request_redraw();
    }

    /// Reconfigure the swap chain and its attachments for a new size.
    ///
    /// A no-op when there is no surface to reconfigure — one has not been built
    /// yet, or a suspension released it — and when the surface is already that
    /// size.
    fn resize_surface(&mut self, width: u32, height: u32) {
        let (Some(surface), Some(scene)) = (self.surface.as_mut(), self.scene.as_ref()) else {
            return;
        };
        let (world, renderer) = (&scene.world, scene.renderer);
        world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                surface.resize(world, renderer, width, height);
            })
            .expect("the renderer is a resource entity");
    }

    /// Release the swap chain, keeping everything it presented.
    ///
    /// The world, the device and the pipelines are untouched: only the surface
    /// the platform invalidated goes, and [`Self::present`] builds one again
    /// from the same window.
    ///
    /// Android is the only platform whose suspension destroys what the surface
    /// draws from, so it is the only caller.
    #[cfg(target_os = "android")]
    fn release(&mut self) {
        let (Some(surface), Some(scene)) = (self.surface.take(), self.scene.as_ref()) else {
            return;
        };
        let (world, renderer) = (&scene.world, scene.renderer);
        world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                surface.release(world, renderer);
            })
            .expect("the renderer is a resource entity");
    }

    /// Rebuild the scene and its surface around `def`.
    ///
    /// The old world and the surface presenting it go together: the surface's
    /// attachments live in the old world's resource graph, so it is released
    /// before the world is dropped. The new scene is built with the same GPU
    /// context and window.
    fn switch_scene(&mut self, def: &'static scenes::SceneDef) {
        if let (Some(surface), Some(scene)) = (self.surface.take(), self.scene.as_ref()) {
            let (world, renderer) = (&scene.world, scene.renderer);
            world
                .with_mut::<Renderer, _>(renderer, |renderer| {
                    surface.release(world, renderer);
                })
                .expect("the renderer is a resource entity");
        }
        self.scene = None;
        self.initial_scene = def;
        self.present();
    }

    /// Advance the scene by the time since the previous frame and present it.
    ///
    /// Does nothing while suspended or while the swap chain is released: the
    /// scene is frozen rather than advanced off-screen, so a resume continues
    /// from where it left off instead of jumping.
    fn draw(&mut self) {
        if !self.foreground {
            return;
        }

        // The selector window keeps where the user left it: egui remembers the
        // position in the context this scene's world carries, so the shell
        // reads it here — before a switch drops that world — to rebuild the
        // new scene's selector in the same place. The layout a frame left
        // stands until this frame's panels have run, so the position read is
        // the user's latest.
        self.selector_pos = self.scene.as_ref().and_then(Scene::selector_position);

        // A switch requested by the selector panel is handled before the frame
        // is drawn: the next redraw presents the new scene.
        let switch = self.scene.as_mut().and_then(|scene| scene.take_switch());
        if let Some(def) = switch {
            self.switch_scene(def);
            return;
        }

        // The fullscreen button's request is served here, before the frame is
        // drawn. The click reached the page through winit's own mouse or touch
        // event, so the transient activation the browser demands is open, and
        // the call has to be made from the loop that owns the window. See
        // [`web`].
        let fullscreen = self.scene.as_mut().is_some_and(Scene::take_fullscreen);
        if fullscreen && let Some(window) = &self.window {
            web::toggle(window);
        }

        let (Some(surface), Some(scene)) = (self.surface.as_mut(), self.scene.as_mut()) else {
            return;
        };
        let now = Instant::now();
        let delta_time = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        scene.advance(delta_time);

        // Acquire, render and present. `acquire` binds the swap chain's next
        // image as the renderer's target; it returns `None` for a frame that
        // should be skipped, such as an occluded window's.
        let (world, renderer) = (&scene.world, scene.renderer);
        let viewport = scene.viewport;
        world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                let Some(frame) = surface.acquire(world, renderer) else {
                    return;
                };
                // Stated before the frame is assembled, so every source that
                // draws the 3D content records into this region and the UI
                // overlay, which states none, still covers the whole target.
                set_frame_viewport(world, viewport.map(FrameViewport));
                renderer.render(world);
                let queue = world
                    .get::<wgpu::Queue>(renderer.context().queue)
                    .expect("the queue resource");
                frame.present(&queue);
            })
            .expect("the renderer is a resource entity");

        scene.end_frame();
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
        selector_pos: Option<egui::Pos2>,
    ) -> Self {
        let mut world = World::new();
        let context = spawn_context(
            &mut world,
            device,
            queue,
            ResourceGraph::new(),
            capabilities,
        );
        let renderer = world.spawn((Renderer::new(context),));
        // The scene's content is built for the region it will be drawn into,
        // not for the whole target: a camera built for the target's aspect
        // would be sheared back out to it by the viewport. A run that does not
        // letterbox — every capture — draws into the whole target, so its
        // content size is the target's own and nothing about it changes.
        let (viewport, content) = letterbox(def.baseline, size, options.letterbox);
        let control = (def.build)(&mut world, context, renderer, content, options);
        let input = WinitInput::new(&mut world);
        // The selector's switch component lives in every world; only a windowed
        // run mounts the panel that writes it.
        let switch = world.spawn((SceneSwitch(None),));
        // The fullscreen button's request component likewise.
        let fullscreen = world.spawn((FullscreenRequest(false),));
        // The frame-rate component likewise, so both paths build the same
        // world shape and only the windowed run mounts the panel reading it.
        let frame_rate = world.spawn((FrameRate::default(),));

        // The UI source drives the scene's own panels — and, in a windowed
        // run, the selector panel. A scene captured without UI mounts none.
        if options.ui || options.selector {
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
            spawn_source(&mut world, source_ui);
        }

        if options.selector {
            mount_selector(&mut world, switch, def, selector_pos);
            mount_frame_rate(&mut world, frame_rate);
            // The browser is the one platform with a page to make fullscreen,
            // and a page can still be denied it — an `iframe` without the
            // `fullscreen` permission. No button is drawn where a press could
            // only fail, so a phone never shows a control that does nothing.
            if web::supported() {
                mount_fullscreen(&mut world, fullscreen);
            }
        }

        Self {
            world,
            renderer,
            input,
            size: content,
            viewport,
            control,
            frame_rate,
            frame: 0,
            sequence_step: options.sequence_step,
            sequence_clock: 0.0,
            switch,
            fullscreen,
            pending: None,
        }
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
    pub fn advance(&mut self, delta_time: f32) {
        // Measured before anything else, so the reading covers the whole frame
        // — input, behaviour, render and present — and not just the part
        // between here and the next call.
        self.world
            .with_mut::<FrameRate, _>(self.frame_rate, |rate| rate.push(delta_time))
            .expect("the frame-rate component exists");

        // The frame's input events run the world's behaviour components. The
        // UI source and the dispatcher read the same list, and the caller
        // clears it once both have: this is the whole input step, and doing it
        // before the frame is drawn means the frame that follows reflects it.
        dispatch_input(&self.world);
        self.world.apply();

        // A fixed-sequence scene holds each frame for `sequence_step` seconds;
        // everything else, and every capture, steps once per drawn frame.
        let stepped = match self.sequence_step {
            Some(step) => tick(&mut self.sequence_clock, step, delta_time),
            None => true,
        };

        // The scene's own per-frame behaviour runs after the input, so the
        // world the renderer reads is this frame's. It is handed the target's
        // current size, so a scene whose camera follows the target's aspect
        // re-aims on the very frame a resize reaches the loop.
        if stepped {
            (self.control.advance)(&mut self.world, self.frame, delta_time, self.size);
            self.frame += 1;
        }

        // The selector's request, read once so the frame loop can act on it.
        self.pending = self
            .world
            .with_mut::<SceneSwitch, _>(self.switch, |s| s.0.take())
            .expect("the switch component exists");
    }

    /// A scene switch requested by the selector panel, if any.
    fn take_switch(&mut self) -> Option<&'static scenes::SceneDef> {
        self.pending.take()
    }

    /// Where the selector window was last laid out, or `None` before it has
    /// been shown once.
    ///
    /// The window's rectangle is remembered by egui, in this scene's context;
    /// the shell reads it from here so a scene switch — which rebuilds that
    /// context — can rebuild the window in the same place.
    fn selector_position(&self) -> Option<egui::Pos2> {
        // Several sources share the world; only the UI one holds egui's
        // window memory, so skip every other source's entity.
        self.world.query::<&Source>().find_map(|(_, source)| {
            let ui = source.as_ref::<UiSource>()?;
            ui.context()
                .memory(|memory| memory.area_rect(SELECTOR_WINDOW).map(|rect| rect.min))
        })
    }

    /// Whether the fullscreen button asked for a change, clearing the request.
    ///
    /// Read once per frame by the windowed loop, which is what serves it; the
    /// scene keeps no opinion of its own about fullscreen.
    fn take_fullscreen(&mut self) -> bool {
        self.world
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
    pub fn render(&mut self) {
        let (world, renderer) = (&self.world, self.renderer);
        world
            .with_mut::<Renderer, _>(renderer, |renderer| renderer.render(world))
            .expect("the renderer is a resource entity");
    }

    /// Finish the frame: drop the events every consumer has read, and apply
    /// whatever a behaviour component or panel queued.
    pub fn end_frame(&mut self) {
        if let Some(state) = self.world.query::<&InputState>().next().map(|(e, _)| e) {
            let _ = self
                .world
                .with_mut::<InputState, _>(state, |state| state.clear_events());
        }
        self.world.apply();
    }
}

/// The scene-selector window's title, which is also its egui area id: the
/// shell reads the window's remembered rectangle by this same id.
const SELECTOR_WINDOW: &str = "scenes";

/// Where the selector window opens when no position was carried over — the
/// first build of a session.
const SELECTOR_POS: egui::Pos2 = egui::Pos2::new(16.0, 430.0);

/// Mount the windowed shell's scene-selector panel.
///
/// The panel lists every scene and writes its choice into the [`SceneSwitch`]
/// component, which the frame loop reads once after the frame's advance.
fn mount_selector(
    world: &mut World,
    switch: Entity,
    current: &'static scenes::SceneDef,
    initial: Option<egui::Pos2>,
) {
    world.spawn((UiPanel::new(move |world, _entity, ui| {
        // The id is set explicitly: `Window::new` derives it from the title's
        // `Atoms` text, whose hash differs from a plain string id, so the
        // shell's read of the remembered rect below would never match.
        egui::Window::new(SELECTOR_WINDOW)
            .id(egui::Id::new(SELECTOR_WINDOW))
            .default_pos(initial.unwrap_or(SELECTOR_POS))
            .show(ui.ctx(), |ui| {
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

/// The smallest the fullscreen button is drawn, in points.
///
/// A control a phone has to be hit with a finger wants a taller target than a
/// row of text: this is the height the touch-target guidelines converge on,
/// and the button is grown to it.
const FULLSCREEN_BUTTON_MIN_SIZE: egui::Vec2 = egui::Vec2::new(140.0, 44.0);

/// The button's inner padding, symmetric so the label is centred within the
/// button it is grown to.
///
/// A bare [`egui::Button::min_size`] would leave the label sitting in a corner:
/// the button's atoms are aligned by the surrounding layout, which is not
/// centred. Growing the padding instead — the same on every side — is what
/// pushes the label to the middle of the larger button.
const FULLSCREEN_BUTTON_PADDING: egui::Vec2 = egui::Vec2::new(16.0, 16.0);

/// The id of the frame-rate readout's area.
const FRAME_RATE_AREA: &str = "unlit3d::frame-rate";

/// The id of the fullscreen button's area.
const FULLSCREEN_AREA: &str = "unlit3d::fullscreen";

/// Mount the frame-rate readout at the top of the window, centred.
///
/// An [`egui::Area`] rather than a window: it is pinned, not something the user
/// can drag away or accidentally resize, and `interactable(false)` keeps it
/// from swallowing a click meant for the scene underneath it.
fn mount_frame_rate(world: &mut World, rate: Entity) {
    world.spawn((UiPanel::new(move |world, _entity, ui| {
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
/// and calls [`web::toggle`], which is where a browser accepts it from.
fn mount_fullscreen(world: &mut World, request: Entity) {
    world.spawn((UiPanel::new(move |world, _entity, ui| {
        let active = web::active();
        let clicked = fullscreen_button(ui, active);
        if clicked {
            let _ = world.with_mut::<FullscreenRequest, _>(request, |request| request.0 = true);
        }
    }),));
}

/// The vertical offset the fullscreen button's anchor is drawn at.
///
/// The readout and the button share the top row when the window is wide enough
/// for both to sit side by side. When it is not — the readout is centred, so on
/// a portrait phone it reaches into the corner the button anchors to — the
/// button drops to the row under the readout instead: hiding the end of the
/// reading under the button that is meant to sit beside it on a roomier window
/// does not help. The fallback keeps both legible and separated at any width.
///
/// Both areas are anchored, and an anchored area is laid out against the
/// rectangle the previous pass remembered for it — which is what is compared
/// here, so the decision is made about the very positions egui will use.
fn fullscreen_button_top(ctx: &egui::Context) -> f32 {
    let readout = ctx.memory(|memory| memory.area_rect(egui::Id::new(FRAME_RATE_AREA)));
    let button = ctx.memory(|memory| memory.area_rect(egui::Id::new(FULLSCREEN_AREA)));
    match (readout, button) {
        // The readout stops clear of the button's column.
        (Some(readout), Some(button)) if readout.right() + OVERLAY_GAP < button.left() => {
            OVERLAY_MARGIN
        }
        // The readout reaches into the button's corner: stack beneath it.
        (Some(readout), _) => readout.bottom() + OVERLAY_GAP,
        // The button is the first thing drawn; the top row is where it belongs.
        (None, _) => OVERLAY_MARGIN,
    }
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
/// A free function rather than the body of the panel's closure so a test can
/// drive it against a bare [`egui::Context`], in both states, without a world
/// or a window.
fn fullscreen_button(ui: &mut egui::Ui, active: bool) -> bool {
    let label = if active {
        "⏏ Exit fullscreen"
    } else {
        "⛶ Fullscreen"
    };
    let label = egui::RichText::new(label).monospace();
    egui::Area::new(egui::Id::new(FULLSCREEN_AREA))
        .anchor(
            egui::Align2::RIGHT_TOP,
            [-OVERLAY_MARGIN, fullscreen_button_top(ui.ctx())],
        )
        // The button is an overlay, not a window: a scene panel dragged over
        // the corner must not cover it, so it sits in the layer above the one
        // windows are drawn in.
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .show(ui, |ui| {
                    // The glyphs are drawn from the default font — ⛶ to enter and
                    // ⏏ to leave — with a word beside each so the meaning does not
                    // rest on a symbol alone. Both labels are laid out to extend
                    // rather than wrap: the exit label is the wider of the two, and
                    // an anchored area lays its contents out against the size it
                    // remembered from the previous pass, so switching to it would
                    // otherwise break it across two lines in the narrower area the
                    // enter label had established.
                    ui.style_mut().spacing.button_padding = FULLSCREEN_BUTTON_PADDING;
                    ui.add(
                        egui::Button::new(label)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .min_size(FULLSCREEN_BUTTON_MIN_SIZE),
                    )
                    .clicked()
                })
                .inner
        })
        .inner
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
        FRAME_RATE_AREA, FULLSCREEN_AREA, FULLSCREEN_BUTTON_MIN_SIZE, FrameRate, OVERLAY_MARGIN,
        egui, frame_rate_readout, fullscreen_button, letterbox, tick,
    };

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

    #[test]
    fn the_fullscreen_button_does_not_cover_the_frame_rate_readout() {
        // The readout sits centred at the top and the button anchors to the
        // top-right corner; on a window wide enough for both they share the top
        // row, and on a narrow one — a portrait phone — the button drops to the
        // row under the readout rather than hide the end of the reading.
        for screen in [
            (1280.0, 577.0),
            (800.0, 600.0),
            (390.0, 844.0),
            (320.0, 480.0),
        ] {
            let ctx = egui::Context::default();
            let text = "60.0 fps · 16.7 ms";
            // Three passes: an anchored area lays out against the size it
            // remembered from the previous pass, and the button's top depends
            // on the readout's remembered rectangle, so the layout only
            // settles once both have a geometry to remember.
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
                });
                output.textures_delta.clear();
            }

            let readout = ctx.memory(|memory| {
                memory
                    .area_rect(egui::Id::new(FRAME_RATE_AREA))
                    .expect("the readout was drawn")
            });
            let button = ctx.memory(|memory| {
                memory
                    .area_rect(egui::Id::new(FULLSCREEN_AREA))
                    .expect("the button was drawn")
            });
            assert!(
                !readout.intersects(button),
                "at {screen:?} the readout {readout:?} and the button {button:?} share screen space"
            );

            // The readout is centred, its top a margin from the window's top.
            assert!(
                (readout.center().x - screen.0 / 2.0).abs() < 1.0,
                "at {screen:?} the readout {readout:?} is not centred"
            );
            assert!(
                (readout.top() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the readout {readout:?} does not keep the top margin"
            );

            // The button keeps the margin from the right edge and stays on screen.
            assert!(
                (screen.0 - button.right() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the button {button:?} does not keep the right margin"
            );
            assert!(
                button.bottom() <= screen.1,
                "at {screen:?} the button {button:?} runs off the bottom"
            );

            // Wide windows hold both on the top row; narrow ones stack the
            // button under the readout.
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
                    button.width() >= FULLSCREEN_BUTTON_MIN_SIZE.x
                        && button.height() >= FULLSCREEN_BUTTON_MIN_SIZE.y,
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
