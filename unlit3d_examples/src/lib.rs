//! An unlit cube with an egui overlay, rendered through
//! [`unlit3d::winit::WindowSurface`] — or, headlessly, into an offscreen target
//! that is read back.
//!
//! Run with `cargo run -p unlit3d_examples`; `Esc` closes the window. Click the
//! panel's button, or drag with the left button, to see the UI take input while
//! the cube keeps spinning.
//!
//! The example is also its own capture tool: `--headless` renders offscreen,
//! reads the frame back and can compare it against a snapshot, so a CI run can
//! check the example's output without a display. That path reads and scores
//! frames with the test harness, so it needs the `snapshot` feature:
//!
//! ```text
//! cargo run -p unlit3d_examples --features snapshot -- --headless --snapshot frame.webp
//! ```
//!
//! The windowed path is the whole frame loop a windowed app needs. The renderer
//! is spawned once as a resource entity, a cube mesh and its material are
//! allocated through it, and every `RedrawRequested` acquires the swap chain's
//! next image, renders the ECS world into it and presents it. A resize is
//! handed to the surface, which reconfigures the swap chain and rebuilds the
//! depth and multisample attachments the renderer draws with.
//!
//! The GPU context is requested asynchronously, because the adapter and device
//! requests are: on the web they resolve on the browser's task queue, so the
//! frame loop must not block on them. The window is created on the main thread
//! — winit hands out a window's raw handle only from the thread that owns it —
//! and the context arrives back through the event loop's proxy, where the scene
//! is built on the thread that owns the ECS world.
//!
//! A suspension does not reset any of that. The platform invalidates the render
//! surface, and on Android destroys the native window under it, but the window
//! handle, the GPU context and the whole ECS world stay: the swap chain alone
//! is released and built again on the next resume, so the app comes back to the
//! state it left — the same spin angle, camera orbit and panel values.
//!
//! Android has no command line to start from: the activity loads the shared
//! library and calls an entry point of its own on a thread of its own, handing
//! it the activity. The crate is therefore a library as well as the binary,
//! and both entries end in the same windowed loop.

mod cli;

#[cfg(target_os = "android")]
pub mod android;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use cli::{Args, Parsed};
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::ui::{UiSource, egui};
use unlit3d::winit::WindowSurface;
// `std::time::Instant` panics on `wasm32-unknown-unknown`, where the standard
// library has no clock; `web-time` reads the browser's `Performance.now()`
// there and re-exports `std::time` everywhere else.
use web_time::Instant;
use wgpu_unlit_render::pipeline::UnlitOptions;
use wgpu_unlit_render::resources::ResourceGraph;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// The number of samples every frame is rendered with.
const SAMPLE_COUNT: u32 = 4;
/// How fast the cube spins, in radians per second.
const SPIN: f32 = 0.8;
/// The timestep the headless path advances the scene by, in seconds, so a
/// captured frame does not depend on how long the frame took to draw.
#[cfg(feature = "snapshot")]
const FIXED_STEP: f32 = 1.0 / 60.0;
/// The format the headless path renders into.
///
/// sRGB, like the view a window surface presents through: it is what lets the
/// built-in unlit shader's colors reach the readback unchanged.
#[cfg(feature = "snapshot")]
const HEADLESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

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
/// Returns the process exit code: the headless capture reports its result
/// through it, so a CI run sees a mismatched snapshot as a failed command.
///
/// Android never comes through here — its activity has no command line and
/// enters at the module that entry point lives in instead — but the function
/// stays part of the library so the two entry points differ in as little as
/// possible.
pub fn run() -> ExitCode {
    init_logging();

    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            stdout(&cli::usage());
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(args)) => args,
        Err(error) => {
            stderr(&format!("error: {error}\n\n{}", cli::usage()));
            return ExitCode::from(2);
        }
    };

    if args.headless {
        return headless(args);
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("an event loop");
    windowed(args, event_loop);
    ExitCode::SUCCESS
}

/// Render offscreen, read the frame back and report on it.
///
/// No window and no event loop: the frames are drawn as fast as the device
/// takes them, on a fixed timestep so the same command produces the same
/// picture.
#[cfg(feature = "snapshot")]
fn headless(args: Args) -> ExitCode {
    use wgpu_unlit_test_util::{read_texture_bytes, score_frame_webp, store_frame_webp};

    let context = wgpu_unlit_test_util::Ctx::headless();
    let mut scene = Scene::new(
        context.device.clone(),
        context.queue.clone(),
        args.size,
        SceneOptions {
            ui: !args.no_ui,
            reproducible: true,
        },
    );

    let (world, renderer) = (&scene.world, scene.renderer);
    let target = world
        .with_mut::<Renderer, _>(renderer, |renderer| {
            bind_offscreen_target(world, renderer, &context.device, args.size, SAMPLE_COUNT)
        })
        .expect("the renderer is a resource entity");

    for _ in 0..args.frames {
        scene.advance(FIXED_STEP);
        scene.render();
        scene.end_frame();
    }

    let (width, height) = args.size;
    let bytes_per_pixel = HEADLESS_FORMAT
        .block_copy_size(None)
        .expect("an RGBA8 format has a block copy size");
    let frame = read_texture_bytes(&context, &target, width, height, bytes_per_pixel);
    log::info!(
        "captured a {width}x{height} frame over {} frames",
        args.frames
    );

    let mut failed = false;
    if let Some(path) = &args.output {
        match store_frame_webp(path, &frame, width, height) {
            Ok(()) => log::info!("wrote {}", path.display()),
            Err(error) => {
                stderr(&format!("error: writing {}: {error}\n", path.display()));
                failed = true;
            }
        }
    }

    if let Some(path) = &args.snapshot {
        let label = path.display();
        if args.update {
            match store_frame_webp(path, &frame, width, height) {
                Ok(()) => log::info!("updated the snapshot at {label}"),
                Err(error) => {
                    stderr(&format!("error: writing {label}: {error}\n"));
                    failed = true;
                }
            }
        } else if !path.exists() {
            // Storing a missing snapshot would pass CI by writing the very
            // thing it is meant to check, so the caller has to ask.
            stderr(&format!(
                "error: no snapshot at {label} to compare against; \
                 store one with `--update` and review it\n"
            ));
            failed = true;
        } else {
            match score_frame_webp(path, &frame, width, height) {
                Ok(score) if score >= args.min_score => {
                    log::info!("snapshot {label}: SSIMULACRA2 score {score:.2}");
                }
                Ok(score) => {
                    stderr(&format!(
                        "error: the frame does not match {label}: \
                         SSIMULACRA2 score {score:.2} < {:.2}\n",
                        args.min_score
                    ));
                    failed = true;
                }
                Err(error) => {
                    stderr(&format!("error: comparing against {label}: {error}\n"));
                    failed = true;
                }
            }
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `--headless` without the feature the readback and comparison live behind.
#[cfg(not(feature = "snapshot"))]
fn headless(_args: Args) -> ExitCode {
    stderr(
        "error: `--headless` needs the `snapshot` feature\n\
         try: cargo run -p unlit3d_examples --features snapshot -- --headless ...\n",
    );
    ExitCode::from(2)
}

/// Bind an offscreen `size`-pixel target as the renderer's render target, and
/// return the color texture the frames land in.
///
/// The attachments go into the frame's resource graph exactly as a window
/// surface's do, so the renderer specializes its pipelines on them the same
/// way — and the color texture is the one a readback copies out of.
#[cfg(feature = "snapshot")]
fn bind_offscreen_target(
    world: &LocalWorld,
    renderer: &mut Renderer,
    device: &wgpu::Device,
    size: (u32, u32),
    samples: u32,
) -> wgpu::Texture {
    use wgpu_unlit_render::resources::Resource as GraphResource;

    let target = create_render_target(device, HEADLESS_FORMAT, size.0, size.1, samples);
    let default_view =
        |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());

    let (color_view, depth_view, msaa_view) = {
        let mut graph = world
            .get_mut::<ResourceGraph>(renderer.context().graph)
            .expect("the context's resource graph exists");
        let insert = |graph: &mut ResourceGraph, resource: GraphResource| {
            graph
                .insert_strong(resource, &[])
                .expect("a texture view has no dependencies")
        };
        let color = insert(&mut graph, GraphResource::from(default_view(&target.color)));
        let depth = insert(&mut graph, GraphResource::from(default_view(&target.depth)));
        let msaa = target
            .msaa
            .as_ref()
            .map(|msaa| insert(&mut graph, GraphResource::from(default_view(msaa))));
        (color, depth, msaa)
    };
    renderer.set_render_target(world, Some(color_view), Some(depth_view), msaa_view);

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
        size: args.size,
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
/// ECS world outlives it too, and holds everything the example has — the spin
/// angle, the camera orbit, the panel's values. Only the swap chain, which a
/// suspension does invalidate, is dropped and built again.
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
    /// The window's initial size, from the command line.
    size: (u32, u32),
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
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
/// the behaviour components and renders into whatever target is bound, so the
/// windowed and headless paths differ only in what they bind and what they do
/// with the result.
struct Scene {
    world: LocalWorld,
    /// The renderer resource entity: the handle every renderer access goes
    /// through.
    renderer: Entity,
    /// Translates the window's events into the world's input events. The
    /// headless path never feeds it, so its state stays idle.
    input: WinitInput,
    /// The render target size, in pixels.
    size: (u32, u32),
}

/// What a scene is built with.
#[derive(Clone, Copy)]
struct SceneOptions {
    /// Mount the egui overlay and its panels.
    ui: bool,
    /// Suppress everything egui animates against the clock, so a captured
    /// frame does not depend on when it was drawn.
    reproducible: bool,
}

/// Set by the panel's button, read once by the frame loop.
struct SpinReset(bool);

/// A behaviour component: the cube turns by `radians_per_second`.
///
/// `spinning` is a sibling the panel's checkbox writes, so the panel and the
/// spin never borrow the same component.
struct Spin {
    radians_per_second: f32,
    /// Whether the cube is currently turning.
    spinning: bool,
    /// The angle turned so far, advanced once per frame.
    angle: f32,
}

/// How far the camera has been dragged around the cube, in radians.
///
/// Set by the panel's slider and by dragging with the left button; read by the
/// frame loop when it rebuilds the camera. It is state, so it lives in a
/// component rather than in a behaviour's closure.
struct CameraOrbit {
    /// The azimuth the camera looks from.
    azimuth: f32,
    /// How far above the horizon it sits.
    elevation: f32,
}

/// The pointer the drag is following, and where it was at the previous move.
///
/// A behaviour cannot keep this in its own closure across runs and still be
/// re-entrant, and egui may also run a panel more than once per frame, so the
/// state lives in a component like every other. Latching onto one contact is
/// what keeps a second finger — or a lifted one — from steering the camera.
struct DragFrom(Option<PointerContact>);

/// Counts the frames drawn, for the panel's readout.
///
/// The panel cannot accumulate this in its own closure — egui may run a panel
/// more than once per frame — so the frame loop advances it once and the panel
/// only reads it.
struct FrameCount(u64);

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
                .with_inner_size(winit::dpi::LogicalSize::new(self.size.0, self.size.1));

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
            // The surface comes first and here, on the thread that owns the
            // window, so the adapter can be required to present to it.
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let surface = instance
                .create_surface(window)
                .expect("the window presents to a surface");

            // The async tail — the adapter and device requests — runs off the
            // event loop's thread (native) or in the browser's task queue
            // (web). The context it produces comes back through `user_event`,
            // where the scene is built on the thread that owns it.
            let proxy = self.proxy.clone();
            spawn(async move {
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
                scene.size = (size.width, size.height);
                self.resize_surface(size.width, size.height);
            }
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Ask for another frame every turn of the loop, so the cube animates.
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

        let scene = match &mut self.scene {
            // Kept across a suspension, so the world's state survives it.
            Some(scene) => scene,
            None => {
                self.scene = Some(Scene::new(
                    context.device.clone(),
                    context.queue.clone(),
                    size,
                    SceneOptions {
                        ui: true,
                        reproducible: false,
                    },
                ));
                self.scene.as_mut().expect("the scene was just built")
            }
        };
        scene.size = size;

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

    /// Advance the scene by the time since the previous frame and present it.
    ///
    /// Does nothing while suspended or while the swap chain is released: the
    /// scene is frozen rather than advanced off-screen, so a resume continues
    /// from where it left off instead of jumping.
    fn draw(&mut self) {
        if !self.foreground {
            return;
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
        world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                let Some(frame) = surface.acquire(world, renderer) else {
                    return;
                };
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
    /// Build the scene the GPU context draws.
    ///
    /// Sync, and on the main thread: the ECS world is single-threaded, so it
    /// lives on the thread the event loop runs on.
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        size: (u32, u32),
        options: SceneOptions,
    ) -> Self {
        let SceneOptions { ui, reproducible } = options;
        // The frame's GPU context, the built-in mesh source and the frame
        // driver, all as resource entities. Every renderable entity carries a
        // key built from the source's options.
        let mut world = LocalWorld::new();
        let context = spawn_context(&mut world, device, queue, ResourceGraph::new());
        let mut source = MeshSource::new(&world, context);
        source.register_unlit_family(&world);
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&source.device(&world)));
        let source_entity = spawn_source(&mut world, source);
        let renderer = world.spawn((Resource, Renderer::new(context)));

        // Geometry, its base-color texture and its material, all allocated
        // through the mesh source so they live in the frame's resource graph.
        let (mesh, material) = world
            .with_mut::<Source, _>(source_entity, |source| {
                let source = source
                    .as_mut::<MeshSource>()
                    .expect("the source entity carries a MeshSource");
                let (positions, uvs, colors, indices) = cube();
                let mesh = source.allocate_unlit_mesh(
                    &world,
                    &key,
                    &positions,
                    Some(&uvs),
                    Some(&colors),
                    Some(&indices),
                );
                let texture = checkerboard(&source.device(&world), &source.queue(&world), 64);
                let view = source.register_texture_and_default_view(&world, texture).1;
                // Linear filtering: the checkerboard is a high-frequency
                // pattern, and point sampling it under minification aliases
                // into moire on the faces the camera sees at a glancing angle.
                let sampler = source.register_sampler(
                    &world,
                    Some(wgpu::SamplerDescriptor {
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Linear,
                        anisotropy_clamp: 4,
                        ..Default::default()
                    }),
                );
                let material = source
                    .allocate_unlit_material(&world, &key, view, sampler)
                    .expect("the standard options read a base-color texture");
                (mesh, material)
            })
            .expect("the source entity exists");

        // The camera the frame is viewed from, and the cube itself. A frame
        // with no RenderLoadOps component is opened with the defaults, so the
        // pass clears color and depth on its own.
        let aspect = size.0 as f32 / size.1 as f32;
        let orbit = CameraOrbit {
            azimuth: 0.6,
            elevation: ORBIT_ELEVATION,
        };
        let camera = world.spawn((orbit_camera(aspect, orbit.azimuth, orbit.elevation), orbit));
        // The cube carries its spin and the orbit the panels drive, so one
        // entity owns everything the frame loop and the panels share.
        let cube = world.spawn((
            Transform::default(),
            Spin {
                radians_per_second: SPIN,
                spinning: true,
                angle: 0.0,
            },
            SpinReset(false),
            DragFrom(None),
            FrameCount(0),
            mesh,
            material,
            UnlitPipeline::new(key),
        ));

        // Input: the adapter spawns the `InputState` resource it fills, and
        // the UI source reads that same resource, so one frame of events is
        // seen by both the panels and the game's own behaviour components.
        let input = WinitInput::new(&mut world);

        // The UI is a frame source like the mesh path, so mounting it is an
        // ordinary spawn. It declares `FrameOrder::OVERLAY`, which is what
        // puts it after the cube however the two were mounted.
        //
        // Without it the frame holds only the cube, which is what makes the
        // 3D scene observable on its own.
        let mut source_ui = UiSource::new();
        if reproducible {
            // egui fades a window in over the first frames and animates widget
            // transitions, all measured against the clock it is handed. With no
            // animation time every one of them is already over, so the first
            // frame is fully drawn and does not depend on when it was taken.
            source_ui
                .context_mut()
                .all_styles_mut(|style| style.animation_time = 0.0);
        }
        spawn_source(&mut world, source_ui);

        // Two panels, because a panel is an entity: a second interface is a
        // second spawn, with its own sibling state, and the source drives both
        // without knowing either of them.
        if ui {
            world.spawn((UiPanel::new(move |world, _entity, ui| {
                egui::Window::new("unlit3d").show(ui.ctx(), |ui| {
                    let frames = world.get::<FrameCount>(cube).map_or(0, |frames| frames.0);
                    ui.label(format!("frame {frames}"));
                    ui.label("The cube spins behind this panel.");

                    // A checkbox writes the `Spin` sibling, and the frame loop
                    // advances the angle; a behaviour component cannot hold the
                    // state it reads.
                    let spinning = world.get::<Spin>(cube).is_some_and(|spin| spin.spinning);
                    let mut spinning_now = spinning;
                    if ui.checkbox(&mut spinning_now, "Spin").changed() {
                        let _ =
                            world.with_mut::<Spin, _>(cube, |spin| spin.spinning = spinning_now);
                    }

                    let mut speed = world
                        .get::<Spin>(cube)
                        .map_or(SPIN, |spin| spin.radians_per_second);
                    if ui
                        .add(egui::Slider::new(&mut speed, 0.0..=4.0).text("rad/s"))
                        .changed()
                    {
                        let _ =
                            world.with_mut::<Spin, _>(cube, |spin| spin.radians_per_second = speed);
                    }

                    if ui.button("Reset the spin").clicked() {
                        let _ = world.with_mut::<SpinReset, _>(cube, |reset| reset.0 = true);
                    }
                });
            }),));
        }
        // The second panel shows the world's input state, which is what makes
        // the events visible next to the UI they also drive.
        // A key behaviour: space toggles the spin, and it reads the state the
        // panel's checkbox also writes. Behaviours cannot re-borrow their own
        // component, so the flag lives on the cube entity beside it.
        world.spawn((OnKey::new(move |world, _entity, key| {
            if key.pressed && !key.repeat && key.key == Key::Space {
                let _ = world.with_mut::<Spin, _>(cube, |spin| spin.spinning = !spin.spinning);
            }
        }),));

        // A pointer behaviour: dragging orbits the camera. It is mounted on
        // `OnPointer` rather than `OnMouse`, so the same drag works with a
        // mouse and with a finger on a touch screen — a touch never produces a
        // mouse button, so a mouse-only drag would be dead on a phone.
        world.spawn((OnPointer::new(move |world, _entity, event| {
            let Some(position) = event.position else {
                // A release or a cancellation may arrive without one; there is
                // no move to measure either way.
                if matches!(
                    event.action,
                    PointerAction::Released { .. } | PointerAction::Left
                ) {
                    let _ = world.with_mut::<DragFrom, _>(cube, |drag| drag.0 = None);
                }
                return;
            };

            match event.action {
                // A press starts the drag at the contact that landed, so a
                // later second finger is ignored: only this contact steers.
                PointerAction::Pressed => {
                    let _ = world.with_mut::<DragFrom, _>(cube, |drag| {
                        drag.0 = Some(PointerContact {
                            kind: event.kind,
                            id: event.id,
                            position,
                        });
                    });
                }
                PointerAction::Moved => {
                    // The drag follows the contact it latched onto, and a
                    // contact that never pressed is a hover, which does not
                    // drag.
                    let previous = world.with_mut::<DragFrom, _>(cube, |drag| {
                        let tracked = drag.0.as_mut()?;
                        if (tracked.kind, tracked.id) != (event.kind, event.id) {
                            return None;
                        }
                        // The difference is measured in the same step the
                        // previous position is swapped out.
                        Some(std::mem::replace(&mut tracked.position, position))
                    });
                    let Some(Some(previous)) = previous else {
                        return;
                    };
                    // A pixel of pointer motion is a fixed turn, so a drag
                    // feels the same however large the window is.
                    const RADIANS_PER_PIXEL: f32 = 0.01;
                    let (dx, dy) = (position[0] - previous[0], position[1] - previous[1]);
                    let _ = world.with_mut::<CameraOrbit, _>(camera, |orbit| {
                        // The dragged surface follows the pointer, so the
                        // camera swings the other way: dragging down pulls the
                        // face being looked at down and brings the face above
                        // it into view, not the one below.
                        orbit.azimuth -= dx * RADIANS_PER_PIXEL;
                        orbit.elevation =
                            (orbit.elevation + dy * RADIANS_PER_PIXEL).clamp(-1.4, 1.4);
                    });
                }
                // A lift ends the drag wherever it happened — a cancellation
                // included, since the platform took the gesture away — and the
                // next press starts over from the contact that lands.
                PointerAction::Released { .. } | PointerAction::Left => {
                    let _ = world.with_mut::<DragFrom, _>(cube, |drag| drag.0 = None);
                }
                PointerAction::Zoom(_) | PointerAction::Rotate(_) => {}
            }
        }),));

        if ui {
            world.spawn((UiPanel::new(move |world, _entity, ui| {
                egui::Window::new("input")
                    .default_pos([16.0, 300.0])
                    .show(ui.ctx(), |ui| {
                        let held = world
                            .query::<&InputState>()
                            .next()
                            .is_some_and(|(_, state)| state.pointer_down);
                        ui.label(if held { "pointer: down" } else { "pointer: up" });
                        // The orbit lives on the camera entity, which is also
                        // what the drag behaviour writes.
                        match world.get::<CameraOrbit>(camera) {
                            Some(orbit) => ui.label(format!(
                                "azimuth {:.2}, elevation {:.2}",
                                orbit.azimuth, orbit.elevation
                            )),
                            None => ui.label("no camera state"),
                        };
                    });
            }),));
        }

        Self {
            world,
            renderer,
            input,
            size,
        }
    }

    /// Advance the scene's behaviour components by `delta_time` seconds.
    fn advance(&mut self, delta_time: f32) {
        // The frame's input events run the world's behaviour components. The
        // UI source and the dispatcher read the same list, and the caller
        // clears it once both have: this is the whole input step, and doing it
        // before the frame is drawn means the frame that follows reflects it.
        dispatch_input(&self.world);
        self.world.apply();

        // The panel's button leaves its press as state; consume it here, once.
        for (_, mut reset) in self.world.query::<&mut SpinReset>() {
            if core::mem::take(&mut reset.0) {
                for (_, mut spin) in self.world.query::<&mut Spin>() {
                    spin.angle = 0.0;
                }
            }
        }

        // The behaviour components run first: the world the renderer reads is
        // this frame's. The panel writes the flags and this advances them, so
        // a panel running twice in a frame cannot double a step.
        for (_, mut frames) in self.world.query::<&mut FrameCount>() {
            frames.0 += 1;
        }
        for (_, (mut spin, mut transform)) in self.world.query::<(&mut Spin, &mut Transform)>() {
            if spin.spinning {
                spin.angle += spin.radians_per_second * delta_time;
            }
            transform.rotation = glam::Quat::from_rotation_y(spin.angle);
        }

        // The camera follows the orbit a slider or a drag set. Nothing advanced
        // it per frame, so a still scene has a still camera and two frames are
        // comparable.
        let aspect = self.size.0 as f32 / self.size.1 as f32;
        for (_, (orbit, mut camera)) in self.world.query::<(&CameraOrbit, &mut Camera)>() {
            *camera = orbit_camera(aspect, orbit.azimuth, orbit.elevation);
        }
    }

    /// Render one frame into whatever target the caller bound.
    ///
    /// Only the headless path draws through this: the windowed one acquires
    /// the swap chain's image and renders between the acquire and the present,
    /// so it needs both under one borrow.
    #[cfg(feature = "snapshot")]
    fn render(&mut self) {
        let (world, renderer) = (&self.world, self.renderer);
        world
            .with_mut::<Renderer, _>(renderer, |renderer| renderer.render(world))
            .expect("the renderer is a resource entity");
    }

    /// Finish the frame: drop the events every consumer has read, and apply
    /// whatever a behaviour component or panel queued.
    fn end_frame(&mut self) {
        if let Some(state) = self.world.query::<&InputState>().next().map(|(e, _)| e) {
            let _ = self
                .world
                .with_mut::<InputState, _>(state, |state| state.clear_events());
        }
        self.world.apply();
    }
}

/// How far from the target the camera orbits, in world units.
const ORBIT_RADIUS: f32 = 3.4;

/// How high above the horizon the camera starts, in radians.
///
/// High enough that the cube's top face is unmistakably visible. Much below
/// this the eye sits almost in the top face's own plane, the face projects to
/// well under a pixel, and the cube reads as though it had no lid.
const ORBIT_ELEVATION: f32 = 0.5;

/// The point the camera looks at, in world units.
///
/// The origin, which is the cube's centre. Looking anywhere else pushes the
/// cube off-centre for no reason.
const ORBIT_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 0.0);

/// A camera orbiting [`ORBIT_TARGET`] at `azimuth` and `elevation`, in radians.
///
/// The built-in pipeline compares depth with `Greater` and clears to the far
/// plane, so the projection is reverse-z infinite.
fn orbit_camera(aspect: f32, azimuth: f32, elevation: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let eye = ORBIT_TARGET
        + ORBIT_RADIUS
            * glam::Vec3::new(
                elevation.cos() * azimuth.sin(),
                elevation.sin(),
                elevation.cos() * azimuth.cos(),
            );
    let view = glam::camera::rh::view::look_at_mat4(eye, ORBIT_TARGET, glam::Vec3::Y);
    Camera {
        clip_from_world: projection * view,
        position: eye,
    }
}

/// The raw channels of one mesh: `(positions, uvs, colors, indices)`.
type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// A unit cube with per-face UVs and vertex colors.
fn cube() -> RawMesh {
    // Each face as (normal, tangent); the bitangent is their cross product.
    let faces = [
        ([-1.0f32, 0.0, 0.0], [0.0f32, 0.0, -1.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ];

    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();
    for (normal, tangent) in faces {
        let base = positions.len() as u32;
        let (normal, tangent) = (glam::Vec3::from(normal), glam::Vec3::from(tangent));
        let bitangent = normal.cross(tangent);
        for (u, v) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let position = normal + tangent * u + bitangent * v;
            positions.push(position.to_array());
            uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
            let color = (position + 1.0) * 0.5;
            colors.push([
                (color.x * 255.0) as u8,
                (color.y * 255.0) as u8,
                (color.z * 255.0) as u8,
                255,
            ]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (positions, uvs, colors, indices)
}

/// A `size` x `size` checkerboard base-color texture.
fn checkerboard(device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> wgpu::Texture {
    let mut texels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let value = if (x + y) % 2 == 0 { 235u8 } else { 60 };
            texels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("example::checkerboard"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    texture
}
