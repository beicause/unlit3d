//! A windowed example with selectable scenes and an egui overlay, rendered
//! through [`unlit3d::winit::WindowSurface`] — or, headlessly, into an
//! offscreen target that is read back and compared against stored snapshots.
//!
//! Run with `cargo run -p unlit3d_examples`; `Esc` closes the window. The
//! windowed loop shows the scene the command line selected — by default the
//! example's own spinning cube — and a panel lists every scene, so one can be
//! switched to at runtime. Click the panel's button, or drag with the left
//! button, to see the UI take input while the cube keeps spinning.
//!
//! Every scene is a [`scenes::SceneDef`] that builds an ECS world and reports
//! the snapshot each frame verifies against. The scenes are ported from the
//! GPU snapshot tests that used to live in `crates/unlit3d/tests`, and the
//! example's headless path is the check that replaced them: `--headless
//! --scene <ID>` renders the scene offscreen, reads each frame back and
//! compares it against the stored snapshot, so CI runs the same command the
//! tests used to. That path reads and scores frames with the test harness, so
//! it needs the `snapshot` feature:
//!
//! ```text
//! cargo run -p unlit3d_examples --features snapshot -- --headless --scene ecs_skinned
//! ```
//!
//! The windowed path is the whole frame loop a windowed app needs. The renderer
//! is spawned once as a resource entity, a scene's meshes and materials are
//! allocated through its mesh source, and every `RedrawRequested` acquires the
//! swap chain's next image, renders the ECS world into it and presents it. A
//! resize is handed to the surface, which reconfigures the swap chain and
//! rebuilds the depth and multisample attachments the renderer draws with.
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

pub mod scenes;

mod cli;

#[cfg(target_os = "android")]
pub mod android;

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
use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// The number of samples the windowed loop presents every frame with.
const SAMPLE_COUNT: u32 = scenes::cube::SAMPLE_COUNT;

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

    if args.headless {
        return headless(args);
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("an event loop");
    windowed(args, event_loop);
    ExitCode::SUCCESS
}

/// Render offscreen, read the frames back and report on them.
///
/// No window and no event loop: the frames are drawn as fast as the device
/// takes them, on a fixed timestep so the same command produces the same
/// picture. Every scene's frames are compared against its own snapshots,
/// unless a raw `--snapshot <PATH>` capture was asked for instead.
#[cfg(feature = "snapshot")]
fn headless(args: Args) -> ExitCode {
    use unlit_wgpu_test_util::Ctx;

    let ctx = Ctx::headless();

    // `--scene all` runs every scene, which is what CI does; anything else
    // runs the one the command line named.
    let scenes: Vec<&scenes::SceneDef> = if args.scene == "all" {
        scenes::SCENES.to_vec()
    } else {
        vec![scenes::by_id(&args.scene).expect("validated by the CLI")]
    };

    // Every scene runs even when one fails, so a CI run reports every
    // mismatch in a single pass.
    let mut failed = false;
    for def in scenes {
        failed |= run_headless_scene(&ctx, def, &args);
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

/// Render one scene offscreen and compare its frames against its snapshots.
///
/// Returns whether anything failed: a snapshot that does not match, one that
/// is missing, or a frame that could not be written.
#[cfg(feature = "snapshot")]
fn run_headless_scene(
    ctx: &unlit_wgpu_test_util::Ctx,
    def: &'static scenes::SceneDef,
    args: &Args,
) -> bool {
    use unlit_wgpu_test_util::{read_texture_bytes, store_frame_webp};

    let size = args.size.unwrap_or(def.size);
    let frames = args.frames.unwrap_or(def.frames);
    // A scene's own snapshots only describe a run that reproduces the scene's
    // stored settings. Overriding the size, the frame count or the UI makes a
    // custom capture instead, which compares against `--snapshot <PATH>`.
    let reproduces_scene = args.size.is_none() && args.frames.is_none() && !args.no_ui;
    if args.update && args.snapshot.is_none() && !reproduces_scene {
        stderr(
            "error: `--update` without `--snapshot <PATH>` needs the scene's own \
             size, frame count and UI; pass `--snapshot <PATH>` to store a custom \
             capture instead\n",
        );
        return true;
    }
    let mut scene = Scene::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        size,
        scenes::SceneOptions {
            ui: def.ui && !args.no_ui,
            selector: false,
            reproducible: true,
            // A capture steps the sequence once per frame, so it draws exactly
            // the frames the snapshots froze.
            sequence_step: None,
        },
        def,
    );

    let (world, renderer) = (&scene.world, scene.renderer);
    let target = world
        .with_mut::<Renderer, _>(renderer, |renderer| {
            bind_offscreen_target(world, renderer, &ctx.device, size, def.samples, def.depth)
        })
        .expect("the renderer is a resource entity");

    let (width, height) = size;
    let bytes_per_pixel = HEADLESS_FORMAT
        .block_copy_size(None)
        .expect("an RGBA8 format has a block copy size");

    let mut failed = false;
    for frame in 0..frames {
        scene.advance(FIXED_STEP);
        scene.render();
        scene.end_frame();

        // A scene's own snapshots are compared frame by frame; a raw
        // `--snapshot <PATH>` capture is compared once, after the loop.
        if args.snapshot.is_none()
            && reproduces_scene
            && let Some(name) = (scene.control.snapshot)(frame)
        {
            let bytes = read_texture_bytes(ctx, &target, width, height, bytes_per_pixel);
            let path = args.snapshot_dir.join(&name);
            failed |= compare_frame(&path, &bytes, width, height, args.update, args.min_score);
        }
    }
    log::info!(
        "scene `{}`: captured a {width}x{height} frame over {} frames",
        def.id,
        frames
    );

    // The last frame is read back once more for `--output` and a raw
    // `--snapshot <PATH>` comparison, so it is the last frame drawn whatever
    // the frame count was.
    let last_frame = read_texture_bytes(ctx, &target, width, height, bytes_per_pixel);

    if let Some(path) = &args.output {
        match store_frame_webp(path, &last_frame, width, height) {
            Ok(()) => log::info!("wrote {}", path.display()),
            Err(error) => {
                stderr(&format!("error: writing {}: {error}\n", path.display()));
                failed = true;
            }
        }
    }

    if let Some(path) = &args.snapshot {
        failed |= compare_frame(
            path,
            &last_frame,
            width,
            height,
            args.update,
            args.min_score,
        );
    }

    failed
}

/// Store or score `rgba` against the snapshot at `path`.
///
/// `update` stores the frame; otherwise a missing snapshot is an error rather
/// than a cue to write one, and a present one is scored on SSIMULACRA2.
#[cfg(feature = "snapshot")]
fn compare_frame(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
    update: bool,
    min_score: f64,
) -> bool {
    use unlit_wgpu_test_util::{score_frame_webp, store_frame_webp};

    let label = path.display();
    if update {
        match store_frame_webp(path, rgba, width, height) {
            Ok(()) => {
                log::info!("updated the snapshot at {label}");
                false
            }
            Err(error) => {
                stderr(&format!("error: writing {label}: {error}\n"));
                true
            }
        }
    } else if !path.exists() {
        // Storing a missing snapshot would pass CI by writing the very
        // thing it is meant to check, so the caller has to ask.
        stderr(&format!(
            "error: no snapshot at {label} to compare against; \
             store one with `--update` and review it\n"
        ));
        true
    } else {
        match score_frame_webp(path, rgba, width, height) {
            Ok(score) if score >= min_score => {
                log::info!("snapshot {label}: SSIMULACRA2 score {score:.2}");
                false
            }
            Ok(score) => {
                stderr(&format!(
                    "error: the frame does not match {label}: \
                     SSIMULACRA2 score {score:.2} < {min_score:.2}\n"
                ));
                true
            }
            Err(error) => {
                stderr(&format!("error: comparing against {label}: {error}\n"));
                true
            }
        }
    }
}

/// Bind an offscreen `size`-pixel target as the renderer's render target, and
/// return the color texture the frames land in.
///
/// The attachments go into the frame's resource graph exactly as a window
/// surface's do, so the renderer specializes its pipelines on them the same
/// way — and the color texture is the one a readback copies out of. A scene
/// declares its own sample count and whether it draws into a depth attachment,
/// matching how its snapshots were captured.
#[cfg(feature = "snapshot")]
fn bind_offscreen_target(
    world: &LocalWorld,
    renderer: &mut Renderer,
    device: &wgpu::Device,
    size: (u32, u32),
    samples: u32,
    with_depth: bool,
) -> wgpu::Texture {
    use unlit_wgpu::render_attachments::create_render_target;
    use unlit_wgpu::resources::Resource as GraphResource;

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
        let depth = with_depth
            .then(|| insert(&mut graph, GraphResource::from(default_view(&target.depth))));
        let msaa = target
            .msaa
            .as_ref()
            .map(|msaa| insert(&mut graph, GraphResource::from(default_view(msaa))));
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
/// the scene's behaviour and renders into whatever target is bound, so the
/// windowed and headless paths differ only in what they bind and what they do
/// with the result. The scene itself is a [`scenes::SceneDef`] whose build
/// populated the world and returned the [`SceneControl`] this drives.
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
    /// The scene's per-frame behaviour and snapshot table.
    control: SceneControl,
    /// The frame index the scene's behaviour is handed.
    frame: u32,
    /// The interval the index advances at, in seconds, or `None` to advance it
    /// once per drawn frame.
    ///
    /// The headless path advances it every frame so a capture draws exactly the
    /// frames its snapshots froze; the windowed path hands a scene that froze a
    /// sequence a longer interval, so the sequence plays at a watchable pace
    /// instead of one frame per display refresh.
    sequence_step: Option<f32>,
    /// Seconds accumulated towards the next advance of `frame`, used only when
    /// `sequence_step` is set.
    sequence_clock: f32,
    /// The scene-selector's switch component, read once per frame.
    switch: Entity,
    /// A scene switch requested by the selector, consumed by the frame loop.
    pending: Option<&'static scenes::SceneDef>,
}

/// Set by the windowed shell's scene-selector panel, read once by the frame
/// loop.
struct SceneSwitch(Option<&'static scenes::SceneDef>);

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

        let scene = match &mut self.scene {
            // Kept across a suspension, so the world's state survives it.
            Some(scene) => scene,
            None => {
                let def = self.initial_scene;
                self.scene = Some(Scene::new(
                    context.device.clone(),
                    context.queue.clone(),
                    size,
                    scenes::SceneOptions {
                        ui: def.ui,
                        selector: true,
                        reproducible: false,
                        // The windowed loop holds each frame of a fixed
                        // sequence for the scene's own step, so it plays at a
                        // watchable pace.
                        sequence_step: def.step_seconds,
                    },
                    def,
                ));
                if let Some(window) = &self.window {
                    window.set_title(&format!("unlit3d — {}", def.title));
                }
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

        // A switch requested by the selector panel is handled before the frame
        // is drawn: the next redraw presents the new scene.
        let switch = self.scene.as_mut().and_then(|scene| scene.take_switch());
        if let Some(def) = switch {
            self.switch_scene(def);
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
    /// Build the scene `def` into a fresh world.
    ///
    /// Sync, and on the main thread: the ECS world is single-threaded, so it
    /// lives on the thread the event loop runs on. The world starts with the
    /// frame's GPU context and the renderer resource entity; the scene's build
    /// adds its sources, meshes, camera and entities, and returns the
    /// [`SceneControl`] that drives them.
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        size: (u32, u32),
        options: scenes::SceneOptions,
        def: &'static scenes::SceneDef,
    ) -> Self {
        let mut world = LocalWorld::new();
        let context = spawn_context(&mut world, device, queue, ResourceGraph::new());
        let renderer = world.spawn((Renderer::new(context),));
        let control = (def.build)(&mut world, context, renderer, size, options);

        let input = WinitInput::new(&mut world);
        // The selector's switch component lives in every world; only a windowed
        // run mounts the panel that writes it.
        let switch = world.spawn((SceneSwitch(None),));

        // The UI source drives the scene's own panels — and, in a windowed
        // run, the selector panel. A headless scene without UI mounts none.
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
            mount_selector(&mut world, switch, def);
        }

        Self {
            world,
            renderer,
            input,
            size,
            control,
            frame: 0,
            sequence_step: options.sequence_step,
            sequence_clock: 0.0,
            switch,
            pending: None,
        }
    }

    /// Advance the scene by `delta_time` seconds.
    fn advance(&mut self, delta_time: f32) {
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
        // world the renderer reads is this frame's.
        if stepped {
            (self.control.advance)(&mut self.world, self.frame, delta_time);
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

/// Mount the windowed shell's scene-selector panel.
///
/// The panel lists every scene and writes its choice into the [`SceneSwitch`]
/// component, which the frame loop reads once after the frame's advance.
fn mount_selector(world: &mut LocalWorld, switch: Entity, current: &'static scenes::SceneDef) {
    world.spawn((UiPanel::new(move |world, _entity, ui| {
        egui::Window::new("scenes")
            .default_pos([16.0, 430.0])
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
    use super::tick;

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
}
