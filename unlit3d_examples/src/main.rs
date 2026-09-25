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
//! frame loop must not block on them. The window and its surface are created
//! on the main thread — winit hands out a window's raw handle only from the
//! thread that owns it — and the context arrives back through the event loop's
//! proxy, where the scene is built on the thread that owns the ECS world.

mod cli;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use cli::{Args, Parsed};
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::ui::{UiSource, egui};
use unlit3d::winit::WindowSurface;
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

/// Install the logger backend the example reports through.
///
/// `RUST_LOG` selects the level natively; the default is `info`, so the
/// example's own startup messages are visible without a variable. On the web
/// the browser console is the terminal.
fn init_logging() {
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init()
        .ok();
    #[cfg(target_arch = "wasm32")]
    console_log::init_with_level(log::Level::Info).ok();
}

fn main() -> ExitCode {
    init_logging();

    // Panics on the web surface as an opaque `unreachable executed` otherwise;
    // the hook logs the message and its stack into the developer console.
    #[cfg(target_arch = "wasm32")]
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

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

    windowed(args);
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

/// Run the windowed example: an event loop, a window and its swap chain.
fn windowed(args: Args) {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("an event loop");
    // Poll rather than wait: the scene animates every frame.
    event_loop.set_control_flow(ControlFlow::Poll);
    let app = App {
        proxy: Some(event_loop.create_proxy()),
        window: None,
        scene: None,
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

/// The application, whose scene is built once the GPU context is ready.
struct App {
    /// Taken on the first `resumed` call so initialization happens once.
    proxy: Option<EventLoopProxy<UserEvent>>,
    /// The window, created in `resumed` and taken by the scene once it is
    /// built.
    window: Option<Arc<Window>>,
    /// The windowed scene, live once the GPU context arrived.
    scene: Option<Windowed>,
    /// The window's initial size, from the command line.
    size: (u32, u32),
}

/// The GPU context a scene draws with, requested asynchronously.
///
/// Everything here is a `Send + Sync` wgpu handle, so the request can run off
/// the event loop's thread and the context can cross back to it. The scene
/// itself cannot cross: its ECS world is single-threaded, so it is built on
/// the main thread once this arrives.
struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
}

impl Gpu {
    /// Request the adapter and device that present to `surface`.
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            surface,
        })
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

/// The pointer position at the previous move, so a drag can measure itself.
///
/// A behaviour cannot keep this in its own closure across runs and still be
/// re-entrant, and egui may also run a panel more than once per frame, so the
/// state lives in a component like every other.
struct DragFrom(Option<[f32; 2]>);

/// Counts the frames drawn, for the panel's readout.
///
/// The panel cannot accumulate this in its own closure — egui may run a panel
/// more than once per frame — so the frame loop advances it once and the panel
/// only reads it.
struct FrameCount(u64);

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Taken, so a repeated `resumed` — suspend and resume, say — starts
        // nothing a second time.
        let Some(proxy) = self.proxy.take() else {
            return;
        };

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
        self.window = Some(window.clone());

        // The surface comes first and here, on the thread that owns the
        // window, so the adapter can be required to present to it.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window.clone())
            .expect("the window presents to a surface");

        // The async tail — the adapter and device requests — runs off the
        // event loop's thread (native) or in the browser's task queue (web).
        // The context it produces comes back through `user_event`, where the
        // scene is built on the thread that owns it.
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Ready(gpu) => {
                let window = self
                    .window
                    .take()
                    .expect("the window was created in `resumed`");
                let scene = Windowed::new(gpu, window);
                // The first frame goes out through the loop's own redraw.
                scene.window().request_redraw();
                self.scene = Some(scene);
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
        scene.input().on_window_event(scene.world(), &event);
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
                // The swap chain and the attachments that go with it are
                // rebuilt for the new size, and the camera's projection
                // follows the new aspect so nothing is stretched.
                if size.width == 0 || size.height == 0 {
                    return;
                }
                scene.resize(size.width, size.height);
            }
            WindowEvent::RedrawRequested => scene.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Ask for another frame every turn of the loop, so the cube animates.
        if let Some(scene) = &self.scene {
            scene.window().request_redraw();
        }
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

        // A pointer behaviour: dragging with the left button orbits the
        // camera. The drag reads the cursor from the frame's state and writes
        // the orbit, which the frame loop then turns into a camera.
        world.spawn((OnPointer::new(move |world, _entity, event| {
            let PointerEvent::Moved { position } = event else {
                return;
            };
            // The drag is the difference from the previous move, so the
            // previous position is swapped for this one in the same step.
            let previous = world
                .with_mut::<DragFrom, _>(cube, |drag| drag.0.replace(*position))
                .flatten();
            let Some(previous) = previous else {
                // The first move only records where the drag started.
                return;
            };

            let dragging = world
                .query::<&InputState>()
                .next()
                .is_some_and(|(_, state)| state.buttons.contains(PointerButtons::PRIMARY));
            if !dragging {
                return;
            }
            // A pixel of pointer motion is a fixed turn, so a drag feels the
            // same however large the window is.
            const RADIANS_PER_PIXEL: f32 = 0.01;
            let (dx, dy) = (position[0] - previous[0], position[1] - previous[1]);
            let _ = world.with_mut::<CameraOrbit, _>(camera, |orbit| {
                orbit.azimuth -= dx * RADIANS_PER_PIXEL;
                orbit.elevation = (orbit.elevation - dy * RADIANS_PER_PIXEL).clamp(-1.4, 1.4);
            });
        }),));

        if ui {
            world.spawn((UiPanel::new(move |world, _entity, ui| {
                egui::Window::new("input")
                    .default_pos([16.0, 300.0])
                    .show(ui.ctx(), |ui| {
                        let held = world
                            .query::<&InputState>()
                            .next()
                            .is_some_and(|(_, state)| {
                                state.buttons.contains(PointerButtons::PRIMARY)
                            });
                        ui.label(if held {
                            "left button: down"
                        } else {
                            "left button: up"
                        });
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

/// The scene plus the window swap chain it presents through.
struct Windowed {
    scene: Scene,
    window_surface: WindowSurface,
    /// The time the previous frame was drawn at, for the frame delta.
    last_frame: Instant,
}

impl Windowed {
    /// Build the scene and the surface that presents it.
    fn new(gpu: Gpu, window: Arc<Window>) -> Self {
        let Gpu {
            instance,
            adapter,
            device,
            queue,
            surface,
        } = gpu;
        let size = window.inner_size();
        let scene = Scene::new(
            device,
            queue,
            (size.width, size.height),
            SceneOptions {
                ui: true,
                reproducible: false,
            },
        );

        let window_surface = scene
            .world
            .with_mut::<Renderer, _>(scene.renderer, |renderer| {
                WindowSurface::new(
                    &scene.world,
                    renderer,
                    &instance,
                    &adapter,
                    window,
                    surface,
                    SAMPLE_COUNT,
                )
            })
            .expect("the renderer is a resource entity");

        Self {
            scene,
            window_surface,
            last_frame: Instant::now(),
        }
    }

    /// The window the surface presents into.
    fn window(&self) -> &Arc<Window> {
        self.window_surface.window()
    }

    /// The scene's world, for the input adapter.
    fn world(&self) -> &LocalWorld {
        &self.scene.world
    }

    /// The input adapter, for the window events.
    fn input(&self) -> &WinitInput {
        &self.scene.input
    }

    /// Rebuild the swap chain and the attachments for a new size, and follow
    /// it with the camera's aspect.
    fn resize(&mut self, width: u32, height: u32) {
        self.scene.size = (width, height);
        let (world, renderer, window_surface) = (
            &self.scene.world,
            self.scene.renderer,
            &mut self.window_surface,
        );
        world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                window_surface.resize(world, renderer, width, height);
            })
            .expect("the renderer is a resource entity");
    }

    /// Advance the scene by the time since the previous frame and present it.
    fn draw(&mut self) {
        let now = Instant::now();
        let delta_time = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        self.scene.advance(delta_time);

        // Acquire, render and present. `acquire` binds the swap chain's next
        // image as the renderer's target; it returns `None` for a frame that
        // should be skipped, such as an occluded window's.
        let (world, renderer, window_surface) = (
            &self.scene.world,
            self.scene.renderer,
            &mut self.window_surface,
        );
        world
            .with_mut::<Renderer, _>(renderer, |renderer| {
                let Some(frame) = window_surface.acquire(world, renderer) else {
                    return;
                };
                renderer.render(world);
                let queue = world
                    .get::<wgpu::Queue>(renderer.context().queue)
                    .expect("the queue resource");
                frame.present(&queue);
            })
            .expect("the renderer is a resource entity");

        self.scene.end_frame();
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
