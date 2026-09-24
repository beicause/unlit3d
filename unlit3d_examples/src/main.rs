//! A windowed unlit cube with an egui overlay, rendered through
//! [`unlit3d::winit::WindowSurface`].
//!
//! Run with `cargo run -p unlit3d_examples`; `Esc` closes the window. Click the
//! panel's button, or type into its text field, to see the UI take input while
//! the cube keeps spinning.
//!
//! The example is the whole frame loop a windowed app needs. The renderer is
//! spawned once as a resource entity, a cube mesh and its material are
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

use std::sync::Arc;
use std::time::Instant;

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

/// The window's initial size, in logical pixels.
const SIZE: (u32, u32) = (960, 720);
/// The number of samples every frame is rendered with.
const SAMPLE_COUNT: u32 = 4;
/// How fast the cube spins, in radians per second.
const SPIN: f32 = 0.8;

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

fn main() {
    init_logging();

    // Panics on the web surface as an opaque `unreachable executed` otherwise;
    // the hook logs the message and its stack into the developer console.
    #[cfg(target_arch = "wasm32")]
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("an event loop");
    // Poll rather than wait: the scene animates every frame.
    event_loop.set_control_flow(ControlFlow::Poll);
    let app = App {
        proxy: Some(event_loop.create_proxy()),
        window: None,
        scene: None,
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
    scene: Option<Scene>,
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

/// Everything a frame draws from.
struct Scene {
    world: LocalWorld,
    /// The renderer resource entity: the handle every renderer access goes
    /// through.
    renderer: Entity,
    /// The camera entity, whose projection follows the window's aspect.
    camera: Entity,
    window_surface: WindowSurface,
    /// Translates the window's events into the world's input events.
    input: WinitInput,
    /// The time the previous frame was drawn at, for the frame delta.
    last_frame: Instant,
}

/// Set by the panel's button, read once by the frame loop.
struct SpinReset(bool);

/// A behaviour component: the cube turns by `radians_per_second`.
struct Spin {
    radians_per_second: f32,
    /// The angle turned so far, advanced once per frame.
    angle: f32,
}

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
            .with_inner_size(winit::dpi::LogicalSize::new(SIZE.0, SIZE.1));

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
                let scene = Scene::new(gpu, window);
                // The first frame goes out through the loop's own redraw.
                scene.window_surface.window().request_redraw();
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
                // The swap chain and the attachments that go with it are
                // rebuilt for the new size, and the camera's projection
                // follows the new aspect so nothing is stretched.
                if size.width == 0 || size.height == 0 {
                    return;
                }
                let aspect = size.width as f32 / size.height as f32;
                scene
                    .world
                    .with_mut::<Camera, _>(scene.camera, |camera| {
                        *camera = camera_view(aspect);
                    })
                    .expect("the camera is an entity");
                let (world, renderer, window_surface) =
                    (&scene.world, scene.renderer, &mut scene.window_surface);
                world
                    .with_mut::<Renderer, _>(renderer, |r| {
                        window_surface.resize(world, r, size.width, size.height);
                    })
                    .expect("the renderer is a resource entity");
            }
            WindowEvent::RedrawRequested => scene.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Ask for another frame every turn of the loop, so the cube animates.
        if let Some(scene) = &self.scene {
            scene.window_surface.window().request_redraw();
        }
    }
}

impl Scene {
    /// Build the scene the GPU context draws.
    ///
    /// Sync, and on the main thread: the ECS world is single-threaded, so it
    /// lives on the thread the event loop runs on. The GPU requests that bring
    /// `gpu` here already happened asynchronously; see [`Gpu::request`].
    fn new(gpu: Gpu, window: Arc<Window>) -> Self {
        let Gpu {
            instance,
            adapter,
            device,
            queue,
            surface,
        } = gpu;

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
        let camera = world.spawn((camera_view(SIZE.0 as f32 / SIZE.1 as f32),));
        world.spawn((
            Transform::default(),
            Spin {
                radians_per_second: SPIN,
                angle: 0.0,
            },
            mesh,
            material,
            UnlitPipeline::new(key),
        ));

        // Input: the adapter spawns the `InputState` resource it fills, and
        // the UI source reads that same resource, so one frame of events is
        // seen by both the panels and the game's own behaviour components.
        let input = WinitInput::new(&mut world);

        // The UI is a frame source like the mesh path, so mounting it is an
        // ordinary spawn. Its panels are entities too: a second panel is a
        // second spawn, and removing one removes its interface.
        spawn_source(&mut world, UiSource::new());
        // A button's press is state, and a behaviour component cannot keep
        // state in itself — it is borrowed while it runs — so the panel writes
        // a sibling component and the frame loop reads it.
        let reset = world.spawn((SpinReset(false),));
        world.spawn((UiPanel::new(move |world, _entity, ui| {
            egui::Window::new("unlit3d").show(ui.ctx(), |ui| {
                ui.label("The cube spins behind this panel.");
                ui.label("Drag or type here: the UI claims the input it uses.");
                if ui.button("Reset the spin").clicked() {
                    let _ = world.with_mut::<SpinReset, _>(reset, |reset| reset.0 = true);
                }
            });
        }),));

        let window_surface = world
            .with_mut::<Renderer, _>(renderer, |r| {
                WindowSurface::new(
                    &world,
                    r,
                    &instance,
                    &adapter,
                    window,
                    surface,
                    SAMPLE_COUNT,
                )
            })
            .expect("the renderer is a resource entity");

        Self {
            world,
            renderer,
            camera,
            window_surface,
            input,
            last_frame: Instant::now(),
        }
    }

    /// Advance the scene's behaviour components and draw one frame.
    fn draw(&mut self) {
        let now = Instant::now();
        let delta_time = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

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
        // this frame's. A query pairs each entity's components, so the spin
        // drives the transform it belongs to.
        for (_, (mut spin, mut transform)) in self.world.query::<(&mut Spin, &mut Transform)>() {
            spin.angle += spin.radians_per_second * delta_time;
            transform.rotation = glam::Quat::from_rotation_y(spin.angle);
        }

        // Acquire, render and present. `acquire` binds the swap chain's next
        // image as the renderer's target; it returns `None` for a frame that
        // should be skipped, such as an occluded window's.
        let (world, renderer, window_surface) =
            (&self.world, self.renderer, &mut self.window_surface);
        world
            .with_mut::<Renderer, _>(renderer, |r| {
                let Some(frame) = window_surface.acquire(world, r) else {
                    return;
                };
                r.render(world);
                let queue = world
                    .get::<wgpu::Queue>(r.context().queue)
                    .expect("the queue resource");
                frame.present(&queue);
            })
            .expect("the renderer is a resource entity");

        // Every consumer has now read this frame's events — the dispatcher and
        // the UI source both — so they can be dropped, keeping the state they
        // left behind.
        if let Some(state) = world.query::<&InputState>().next().map(|(e, _)| e) {
            let _ = world.with_mut::<InputState, _>(state, |state| state.clear_events());
        }

        // Apply whatever a behaviour component or panel queued, so the next
        // frame sees the world it asked for.
        self.world.apply();
    }
}

/// A camera looking at the origin from (0, 1.2, 3.2).
///
/// The built-in pipeline compares depth with `Greater` and clears to the far
/// plane, so the projection is reverse-z infinite.
fn camera_view(aspect: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let eye = glam::Vec3::new(0.0, 1.2, 3.2);
    let view =
        glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::new(0.0, 0.2, 0.0), glam::Vec3::Y);
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
