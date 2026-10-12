//! A multi-world demo: one scene where rendering, physics and audio each
//! run in their own world, joined by jobs and snapshots.
//!
//! The host world owns the window and drives the frame. A render sub-world
//! sits on the main thread as a nested world and serves the others: it
//! allocates meshes, materials and instances when gameplay asks, and the
//! entity bits it answers with are the handles the rest of the show uses.
//! A physics world and an audio world each run on their own thread as
//! remote worlds. Gameplay — also remote — is the client that drives the
//! scene: it requests bodies from physics, builds the render scene through
//! the render server, publishes transforms, and books the notes each
//! collision plays.
//!
//! Space drops a body; the render world's control panel plays a short
//! MIDI sequence, pauses and stops the audio, and drops more bodies.
//! Bodies fall, bounce off the ground and each other, and every contact
//! hard enough plays a music-box note. The soundfont is embedded at
//! build time: build.rs reads the FluidR3Mono_GM.sf3 file the repository
//! keeps out of version control under assets, and the demo runs silent
//! when it is missing.
//!
//! The --headless switch runs the same scene without a window or
//! threads: the worlds step inline at fixed rates, the run keeps its
//! last frame and audio mix, and writes them into --out.

use std::{process::ExitCode, time::Duration};

use crossbeam_channel::Sender;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use image::ImageEncoder;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit3d::blit::{BlitSource, BlitTexture};
use unlit3d::frame::Frame;
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::winit::WindowSurface;
use unlit3d::winit::builtin::{
    CreateWindowRequest, DisplayHandle, ExitRequest, Resumed, WindowSpec, WinitProxy,
    create_window_on_resume, exit_on_close_requested, exit_on_escape,
    request_redraw_while_foreground, window, winit_input_behaviour,
};
use unlit3d::winit::event::{OnAboutToWait, OnUserEvent, OnWindowEvent, WinitHost};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use unlit3d_mcp::{McpCommand, WorldId};
use unlit3dx::prelude::*;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use unlit3dx_examples::headless;
use unlit3dx_examples::scene::*;
use web_time::Instant;
use winit::event::WindowEvent;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

/// The GPU context the scene draws with.
///
/// Everything here is a send-and-sync wgpu handle, so the handles may
/// cross into the render world's setup; the scene itself cannot cross
/// threads and is set up on the main thread.
#[derive(Clone)]
struct Gpu {
    /// The instance the presentation surface is created from.
    instance: wgpu::Instance,
    /// The adapter the device was requested from.
    adapter: wgpu::Adapter,
    /// The device every buffer and pipeline belongs to.
    device: wgpu::Device,
    /// The queue frames are submitted to.
    queue: wgpu::Queue,
    /// What the device can do beyond the WebGPU baseline, captured while
    /// the adapter is still here to report it.
    capabilities: DeviceCapabilities,
}

impl Gpu {
    /// Request the adapter and device that can present to the probe
    /// surface.
    ///
    /// The probe exists only so the adapter is required to present to the
    /// window, and is dropped before the device is requested.
    async fn request(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        log::debug!("requesting an adapter");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        log::debug!("adapter landed; requesting a device");
        drop(surface);
        let tier = DeviceTier::from_env();
        let capabilities = tier.capabilities_of(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: tier.limits(&adapter.limits()),
                ..Default::default()
            })
            .await?;
        log::debug!("device landed");
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            capabilities,
        })
    }
}

/// The GPU request lifecycle: nothing requested yet, in flight, or
/// resolved.
enum GpuState {
    /// The request has not started.
    Idle,
    /// A request is running off the main thread.
    Requested,
    /// The context came back and the scene can be set up.
    Ready,
}

/// The render sub-world and the renderer resource entity inside it.
struct RenderRef(Option<(Entity, Entity)>);

/// The swap chain the window presents into.
struct SurfaceSlot(Option<WindowSurface>);

/// The attachments the blit world binds before it presents; the blit
/// draws with one sample and no depth, so this only tracks the surface.
struct AttachmentsSlot(Option<FrameAttachments>);

/// The offscreen color texture the render world's frames land in, and the
/// texture a screenshot reads back.
struct Offscreen(Option<wgpu::Texture>);

/// The blit world that presents the offscreen target into the swap chain.
///
/// The render world draws into a persistent offscreen texture rather than
/// into the surface: a surface texture is destroyed on present, so it is
/// no target a screenshot could read back from. The blit world owns the
/// swap chain and copies the target over it every frame.
struct BlitRef(Option<BlitView>);

/// The blit world's entities.
struct BlitView {
    /// The entity carrying the blit world.
    world: Entity,
    /// The renderer resource entity the surface is presented through.
    renderer: Entity,
    /// The entity carrying the blitted texture component.
    texture: Entity,
}

/// The GPU context, kept so rebuilds can reach the device again.
struct GpuSlot(Option<Gpu>);

/// When the previous frame started, for the frame's delta time.
struct LastFrame(Instant);

/// The setup cargo: what the one-time scene setup moves into the render
/// world, parked on the app entity until the setup command takes it.
struct SetupCargo(Option<SetupCargoInner>);

/// The parts of the setup cargo that cannot be built twice.
#[derive(Clone)]
struct SetupCargoInner {
    /// The reader end of the transform mailbox gameplay publishes into.
    frame_reader: SnapshotReader<TransformFrame>,
    /// The render sub-world, spawned empty at startup and filled in by
    /// the setup command.
    render: Entity,
    /// The gameplay world's command sender, for the control panel.
    gameplay: Sender<Job<()>>,
    /// The audio world's command sender, for the control panel.
    audio: Sender<Job<()>>,
}

/// One frame's work, deferred to the host's apply.
///
/// The frame is a dispatch of the host world's per-frame behaviours,
/// which need a shared world; the command is what hands them a mutable
/// one. It is applied between two event callbacks, so no frame is drawn
/// while one is handled.
struct DrawFrame {
    /// The app entity carrying the state the frame reads and writes.
    app: Entity,
}

impl Command for DrawFrame {
    fn apply(self: Box<Self>, world: &mut World) {
        let app = self.app;
        let now = Instant::now();
        let delta_time = world
            .get::<LastFrame>(app)
            .map_or(0.0, |last| (now - last.0).as_secs_f32().min(0.1));
        let _ = world.with_mut::<LastFrame, _>(app, |slot| slot.0 = now);
        let size = window(world)
            .map(|window| {
                let size = window.inner_size();
                (size.width, size.height)
            })
            .unwrap_or(WINDOW_SIZE);
        let mut frame = Frame {
            delta_time,
            index: 0,
            size,
        };
        dispatch_frame(world, &mut frame);
    }
}

/// The one-time scene setup, deferred to the host's apply.
///
/// The GPU request has already resolved by the time this runs: the setup
/// needs a mutable world, so it waits for the apply between two events.
struct Setup {
    /// The app entity carrying the setup cargo and the slots to fill.
    app: Entity,
    /// The GPU context the request resolved to.
    gpu: Gpu,
}

impl Command for Setup {
    fn apply(self: Box<Self>, world: &mut World) {
        log::debug!("setting the scene up");
        let app = self.app;
        let gpu = self.gpu;
        let Some(window) = window(world) else {
            log::error!("the scene setup found no window");
            return;
        };
        let _ = world.with_mut::<GpuState, _>(app, |state| *state = GpuState::Ready);

        // The cargo is taken exactly once: the behaviour that queued this
        // command keeps queuing until the slots say the setup landed, and
        // a half-built scene must not be built twice.
        let cargo = world
            .get_mut::<SetupCargo>(app)
            .and_then(|mut cargo| cargo.0.take());
        let Some(cargo) = cargo else {
            return;
        };

        // The sub-world was spawned empty at startup; its content — the
        // resource graph, the camera, the asset-server source and the
        // transform applier — lands now that the GPU context exists. No
        // mesh or material is built here: gameplay requests them through
        // the sub-world's command channel, like any other client.
        let renderer = setup_render_world(
            world,
            cargo.render,
            &gpu.device,
            &gpu.queue,
            gpu.capabilities,
            cargo.frame_reader,
            Some(ControlHooks {
                gameplay: cargo.gameplay,
                audio: cargo.audio,
            }),
        );
        let _ = world.with_mut::<RenderRef, _>(app, |slot| {
            slot.0 = Some((cargo.render, renderer));
        });

        let surface = match gpu.instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                log::error!("failed to build the presentation surface: {error}");
                return;
            }
        };
        let raw_size = window.inner_size();
        let size = if raw_size.width == 0 || raw_size.height == 0 {
            WINDOW_SIZE
        } else {
            (raw_size.width, raw_size.height)
        };
        let offscreen = {
            let Some(nested) = world.get::<World>(cargo.render) else {
                log::error!("the scene setup found no render world");
                return;
            };
            nested.with_mut::<Renderer, _>(renderer, |renderer| {
                bind_offscreen(&nested, renderer, &gpu.device, size)
            })
        };
        let Some(offscreen) = offscreen else {
            log::error!("failed to bind the offscreen target: the renderer was gone");
            return;
        };

        // The blit world owns the swap chain and copies the offscreen
        // target over it every frame. It draws with one sample and no
        // depth: the blit covers the target with a single triangle.
        let mut blit_world = World::new();
        let blit_frame = spawn_context(
            &mut blit_world,
            gpu.device.clone(),
            gpu.queue.clone(),
            ResourceGraph::new(),
            gpu.capabilities,
        );
        let blit_renderer = blit_world.spawn((Renderer::new(blit_frame),));
        let created = blit_world.with_mut::<Renderer, _>(blit_renderer, |renderer| {
            let window_surface = WindowSurface::new(
                &blit_world,
                renderer,
                &gpu.instance,
                &gpu.adapter,
                window.clone(),
                surface,
            );
            let attachments = FrameAttachments::new(
                &blit_world,
                renderer.context(),
                window_surface.color_format(),
                window_surface.size(),
                1,
                false,
            );
            (window_surface, attachments)
        });
        let (window_surface, attachments) = match created {
            Some(created) => created,
            None => {
                log::error!("failed to open the window surface: the renderer was gone");
                return;
            }
        };
        let blit_texture = {
            let context = blit_world
                .get::<Renderer>(blit_renderer)
                .expect("the blit renderer is a resource entity")
                .context();
            let view = TextureExt::create_view(&offscreen, &wgpu::TextureViewDescriptor::default());
            blit_world.spawn((BlitTexture::new(&blit_world, context, view),))
        };
        blit_world.spawn_source(BlitSource::new());
        let blit_entity = world.spawn((blit_world,));

        let _ = world.with_mut::<SurfaceSlot, _>(app, |slot| slot.0 = Some(window_surface));
        let _ = world.with_mut::<AttachmentsSlot, _>(app, |slot| slot.0 = Some(attachments));
        let _ = world.with_mut::<Offscreen, _>(app, |slot| slot.0 = Some(offscreen));
        let _ = world.with_mut::<GpuSlot, _>(app, |slot| slot.0 = Some(gpu.clone()));
        let _ = world.with_mut::<BlitRef, _>(app, |slot| {
            slot.0 = Some(BlitView {
                world: blit_entity,
                renderer: blit_renderer,
                texture: blit_texture,
            })
        });
        log::info!("the scene is set up");

        // The surface is live, so a transport can serve the worlds: the
        // stdio server blocks, so it runs on a thread of its own and posts
        // its commands back through the event loop.
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        if world.get::<McpEnabled>(app).is_some_and(|mcp| mcp.0) {
            start_mcp(world, app);
        }
        window.request_redraw();
    }
}

/// The setup behaviour: once, after the window exists, start the GPU
/// request.
///
/// The request runs off the main thread and answers through the event
/// loop, so this keeps queuing nothing once a request is in flight: a
/// state that is no longer idle skips the frame, and the answer lands as
/// a user event.
fn request_gpu(app: Entity) -> OnFrame {
    OnFrame::new(
        FrameBehaviourOrder(FRAME_SETUP),
        move |world, _entity, _frame| {
            if !world
                .get::<GpuState>(app)
                .is_some_and(|state| matches!(&*state, GpuState::Idle))
            {
                return;
            }
            let Some(window) = window(world) else {
                return;
            };
            if !world
                .query::<&Resumed>()
                .next()
                .is_some_and(|(_, resumed)| resumed.0)
            {
                return;
            }
            let Some(display) = world
                .query::<&DisplayHandle>()
                .next()
                .map(|(_, handle)| handle.0.clone())
            else {
                return;
            };
            let Some(proxy) = world
                .get::<WinitProxy<UserEvent>>(app)
                .map(|proxy| proxy.0.clone())
            else {
                return;
            };
            let _ = world.with_mut::<GpuState, _>(app, |state| *state = GpuState::Requested);
            log::debug!("requesting a GPU context");
            // The display handle is owned, not borrowed: the instance it
            // builds has to outlive this callback. The descriptor is the
            // environment-aware one, so WGPU_BACKEND still selects a
            // backend.
            spawn(async move {
                let instance = wgpu::util::new_instance_with_webgpu_detection(
                    wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)),
                )
                .await;
                let probe = match instance.create_surface(window) {
                    Ok(surface) => surface,
                    Err(error) => {
                        let _ = proxy.send_event(UserEvent::Failed(error.to_string()));
                        return;
                    }
                };
                match Gpu::request(instance, probe).await {
                    Ok(gpu) => {
                        let _ = proxy.send_event(UserEvent::Ready(gpu));
                    }
                    Err(error) => {
                        let _ = proxy.send_event(UserEvent::Failed(error.to_string()));
                    }
                }
            });
        },
    )
}

/// The draw behaviour: drain the render sub-world's command queue, then
/// dispatch the frame into it, render into the offscreen target, and
/// present the target through the window surface.
///
/// Draining is what makes the sub-world a server: gameplay's asset and
/// instance requests land as jobs here, on the main thread, between one
/// frame's render and the next.
fn draw(app: Entity) -> OnFrame {
    let mut frame_count = 0u32;
    OnFrame::new(
        FrameBehaviourOrder(FRAME_DRAW),
        move |world, _entity, frame| {
            frame_count += 1;
            if frame_count.is_multiple_of(120) {
                log::debug!("frame dispatched: {frame_count}");
            }
            let Some((render_world, renderer)) = world
                .get::<RenderRef>(app)
                .and_then(|reference| reference.0)
            else {
                return;
            };
            let Some((blit_world, blit_renderer)) = world
                .get::<BlitRef>(app)
                .and_then(|slot| slot.0.as_ref().map(|blit| (blit.world, blit.renderer)))
            else {
                return;
            };
            let attachments_slot = world.get::<AttachmentsSlot>(app);
            let Some(attachments) = attachments_slot.as_deref().and_then(|slot| slot.0.as_ref())
            else {
                return;
            };
            let Some(mut nested) = world.get_mut::<World>(render_world) else {
                return;
            };
            drain_sub_world(&mut nested);
            dispatch_frame(&mut nested, frame);
            let _ = nested.with_mut::<Renderer, _>(renderer, |renderer| {
                renderer.render(&nested);
            });
            let Some(mut surface) = world.get_mut::<SurfaceSlot>(app) else {
                return;
            };
            let Some(surface) = surface.0.as_mut() else {
                return;
            };
            let Some(blit_nested) = world.get_mut::<World>(blit_world) else {
                return;
            };
            let _ = blit_nested.with_mut::<Renderer, _>(blit_renderer, |renderer| {
                let Some(swap_frame) = surface.acquire(&blit_nested, renderer) else {
                    return;
                };
                attachments.bind(&blit_nested, renderer, swap_frame.color_view().clone());
                renderer.render(&blit_nested);
                let queue = blit_nested
                    .get::<wgpu::Queue>(renderer.context().queue)
                    .expect("the queue resource");
                swap_frame.present(&queue);
            });
        },
    )
}

/// The redraw behaviour: each redraw request dispatches one frame, so the
/// scene advances only when the compositor asks for a picture.
fn redraw(app: Entity) -> OnWindowEvent {
    OnWindowEvent::new(move |world, _entity, payload| {
        if matches!(payload.event, WindowEvent::RedrawRequested) {
            world.queue().push(DrawFrame { app });
        }
    })
}

/// The pump behaviour: one frame per turn of the event loop, so the scene
/// advances on its own. Redraw requests alone go quiet whenever the window
/// stops being visible to the compositor, and a simulation left waiting on
/// them freezes with it.
fn pump(app: Entity) -> OnAboutToWait {
    OnAboutToWait::new(move |world, _entity, ()| {
        world.queue().push(DrawFrame { app });
    })
}

/// The resize behaviour: match the surface, the blit attachments and the
/// offscreen target to the new window size, skipping the degenerate zero
/// size windows report on minimize.
fn resize(app: Entity) -> OnWindowEvent {
    OnWindowEvent::new(move |world, _entity, payload| {
        let WindowEvent::Resized(size) = &payload.event else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let Some((render_world, renderer)) = world
            .get::<RenderRef>(app)
            .and_then(|reference| reference.0)
        else {
            return;
        };
        let Some((blit_world, blit_renderer, blit_texture)) =
            world.get::<BlitRef>(app).and_then(|slot| {
                slot.0
                    .as_ref()
                    .map(|blit| (blit.world, blit.renderer, blit.texture))
            })
        else {
            return;
        };
        let Some(device) = world
            .get::<GpuSlot>(app)
            .and_then(|slot| slot.0.clone())
            .map(|gpu| gpu.device)
        else {
            return;
        };
        // The surface is taken out so it can be resized by value, and put
        // back once the surface, the attachments and the target all match
        // the window.
        let Some(mut surface) = world
            .get_mut::<SurfaceSlot>(app)
            .and_then(|mut slot| slot.0.take())
        else {
            return;
        };
        let Some(mut attachments) = world
            .get_mut::<AttachmentsSlot>(app)
            .and_then(|mut slot| slot.0.take())
        else {
            let _ = world.with_mut::<SurfaceSlot, _>(app, |slot| slot.0 = Some(surface));
            return;
        };
        macro_rules! restore {
            () => {{
                let _ = world.with_mut::<SurfaceSlot, _>(app, |slot| slot.0 = Some(surface));
                let _ =
                    world.with_mut::<AttachmentsSlot, _>(app, |slot| slot.0 = Some(attachments));
            }};
        }
        let Some(blit_nested) = world.get::<World>(blit_world) else {
            restore!();
            return;
        };
        let _ = blit_nested.with_mut::<Renderer, _>(blit_renderer, |renderer| {
            surface.resize(&blit_nested, renderer, size.width, size.height);
        });
        attachments.resize(&blit_nested, surface.size());
        let Some(nested) = world.get::<World>(render_world) else {
            restore!();
            return;
        };
        let offscreen = nested.with_mut::<Renderer, _>(renderer, |renderer| {
            bind_offscreen(&nested, renderer, &device, surface.size())
        });
        let Some(offscreen) = offscreen else {
            restore!();
            return;
        };
        // Point the blit at the new target: the replaced texture component
        // is what makes the blit source rebuild its bind group on the next
        // frame.
        let view = TextureExt::create_view(&offscreen, &wgpu::TextureViewDescriptor::default());
        let texture = {
            let context = blit_nested
                .get::<Renderer>(blit_renderer)
                .expect("the blit renderer is a resource entity")
                .context();
            BlitTexture::new(&blit_nested, context, view)
        };
        let replaced = blit_nested.with_mut::<BlitTexture, _>(blit_texture, |slot| {
            *slot = texture;
        });
        if replaced.is_none() {
            restore!();
            return;
        }
        let _ = world.with_mut::<SurfaceSlot, _>(app, |slot| slot.0 = Some(surface));
        let _ = world.with_mut::<AttachmentsSlot, _>(app, |slot| slot.0 = Some(attachments));
        let _ = world.with_mut::<Offscreen, _>(app, |slot| slot.0 = Some(offscreen));
    })
}

/// Whether the MCP stdio transport serves this run.
struct McpEnabled(bool);

/// What the event loop wakes the host for beyond winit's own events.
enum UserEvent {
    /// The GPU context came back from the request.
    Ready(Gpu),
    /// The GPU request failed; the text is the error to report.
    Failed(String),
    /// An MCP tool command arrived on the transport thread.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    Command(Option<McpCommand>),
}

/// Run a future concurrently: on a worker thread on native, in the
/// browser's task queue on the web (where a thread cannot be blocked).
#[cfg(not(target_arch = "wasm32"))]
fn spawn(future: impl core::future::Future<Output = ()> + Send + 'static) {
    std::thread::spawn(move || pollster::block_on(future));
}

/// Run a future concurrently: on a worker thread on native, in the
/// browser's task queue on the web (where a thread cannot be blocked).
#[cfg(target_arch = "wasm32")]
fn spawn(future: impl core::future::Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(future);
}

/// Run one MCP command against the world it names.
///
/// Running a job needs a mutable world, so the command is queued and
/// applied between two event callbacks. The host answers the world list
/// itself: it is the only one that knows which entities carry a world.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn dispatch_command(world: &World, command: McpCommand) {
    if command.wants_worlds() {
        let mut worlds = vec![WorldId::Host];
        worlds.extend(
            world
                .query::<(Entity, &World)>()
                .map(|(entity, _)| WorldId::handle(entity)),
        );
        command.answer_worlds(worlds);
        return;
    }
    match command.world().entity() {
        Some(entity) => world.queue().push(RunInWorld { entity, command }),
        None => world.queue().push(RunInHost { command }),
    }
}

/// Run an MCP command against the host world itself.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
struct RunInHost {
    command: McpCommand,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Command for RunInHost {
    fn apply(self: Box<Self>, world: &mut World) {
        self.command.dispatch(world);
    }
}

/// Run an MCP command against the nested world one entity carries.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
struct RunInWorld {
    entity: Entity,
    command: McpCommand,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Command for RunInWorld {
    fn apply(self: Box<Self>, world: &mut World) {
        if let Some(mut nested) = world.get_mut::<World>(self.entity) {
            self.command.dispatch(&mut nested);
        } else {
            self.command
                .fail(format!("entity {} carries no world", self.entity.to_bits()));
        }
    }
}

/// Handle the events the app posts to itself.
///
/// The GPU answer queues the scene setup, a failed request exits, and an
/// MCP command is dispatched against the world it names.
fn handle_user_event(app: Entity) -> OnUserEvent<UserEvent> {
    OnUserEvent::new(move |world, _entity, event| match event {
        UserEvent::Ready(gpu) => {
            world.queue().push(Setup {
                app,
                gpu: gpu.clone(),
            });
        }
        UserEvent::Failed(error) => {
            log::error!("failed to start the renderer: {error}");
            let _ = world.with_mut::<ExitRequest, _>(app, |request| request.0 = true);
        }
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        UserEvent::Command(command) => {
            if let Some(command) = command.take() {
                dispatch_command(world, command);
            }
        }
    })
}

/// Start the MCP stdio transport, which posts each command back to the
/// loop.
///
/// It runs on its own thread because serving the protocol blocks; the
/// proxy is what carries a command back, because a world is not sendable.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn start_mcp(world: &World, app: Entity) {
    let Some(proxy) = world
        .get::<WinitProxy<UserEvent>>(app)
        .map(|proxy| proxy.0.clone())
    else {
        return;
    };
    std::thread::Builder::new()
        .name("unlit3dx-mcp-transport".to_owned())
        .spawn(move || {
            let sink = move |command| proxy.send_event(UserEvent::Command(Some(command))).is_ok();
            if let Err(error) = unlit3d_mcp::serve_stdio_with(sink) {
                log::error!("the MCP server stopped: {error}");
            }
        })
        .expect("the MCP transport thread starts");
}

/// Run the whole show: five worlds, one window.
///
/// The host world owns the window and the render sub-world, which serves
/// the scene's assets; the physics, audio and gameplay worlds are remote.
/// Gameplay builds the scene by requesting from the render server, jobs
/// go out, snapshots come back, and nothing but data crosses a thread
/// boundary. With the mcp flag, a Model Context Protocol server over
/// stdio reads the host and its render world.
fn windowed(mcp: bool, event_loop: EventLoop<UserEvent>) {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut host = WinitHost::<UserEvent>::new();
    let proxy = event_loop.create_proxy();
    let world = host.world_mut();

    let input = InputHandle::new();
    world.spawn((input.clone(),));
    world.spawn((winit_input_behaviour(WinitInput::new(input)),));

    let attributes = Window::default_attributes()
        .with_title("unlit3dx multi-world")
        .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
    world.spawn((
        WindowSpec(attributes),
        CreateWindowRequest(None),
        create_window_on_resume(),
    ));
    world.spawn((ExitRequest(false), exit_on_close_requested()));
    world.spawn((ExitRequest(false), exit_on_escape()));
    request_redraw_while_foreground();

    // The render sub-world: spawned empty, on the main thread, before
    // anyone needs it, so gameplay holds its proxy from the first tick.
    // Its content lands once the GPU request resolves.
    let render = spawn_sub_world(world, RENDER_COMMAND_CAPACITY);
    let render_proxy = world
        .get::<SubWorld>(render)
        .expect("the sub-world proxy carries its commands")
        .clone();

    // The physics world: fixed step, default gravity, a ground plane
    // ready before the first ball drops.
    let physics = spawn_physics_world(world, PhysicsConfig::default());
    let physics_sender = world
        .get::<RemoteWorld>(physics)
        .expect("the physics proxy carries its commands")
        .sender();
    physics_sender
        .send(Box::new(|world: &mut World| {
            spawn_fixed_cuboid(world, GROUND_POSITION, GROUND_HALF, 0.9);
        }))
        .ok();
    let physics_reader = world
        .get::<PhysicsInbox>(physics)
        .expect("the physics proxy carries its inbox")
        .0
        .clone();

    // The audio world: one synthesizer voice plus a MIDI player, both
    // fed by the embedded soundfont, mixed into the output stream on the
    // audio thread.
    let audio = spawn_audio_world(
        world,
        AudioConfig::default(),
        Some(music_box_source(SOUNDFONT.into())),
    );
    let audio_sender = world
        .get::<RemoteWorld>(audio)
        .expect("the audio proxy carries its commands")
        .sender();
    let audio_inbox = world
        .get::<AudioReadyInbox>(audio)
        .expect("the audio proxy carries its readiness inbox")
        .0
        .clone();

    // The gameplay world: one entity of state, ticked at a fixed rate,
    // driving the scene as the render server's client.
    let (frame_writer, frame_reader) = snapshot_pair::<TransformFrame>();
    let gameplay = spawn_remote_world_with(world, "gameplay", 64, (), {
        let render = render_proxy.clone();
        let physics_sender = physics_sender.clone();
        let audio_sender = audio_sender.clone();
        let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / GAME_HZ));
        move |world: &mut World, control: &ThreadControl| {
            if control.stop_requested() {
                return false;
            }
            if world.query::<&Gameplay>().next().is_none() {
                world.spawn((Gameplay::new(
                    frame_writer.clone(),
                    physics_reader.clone(),
                    physics_sender.clone(),
                    audio_sender.clone(),
                    audio_inbox.clone(),
                    render.clone(),
                ),));
            }
            if let Some((_, mut game)) = world.query::<&mut Gameplay>().next() {
                game.tick();
            }
            clock.wait(control);
            true
        }
    });
    let gameplay_sender = world
        .get::<RemoteWorld>(gameplay)
        .expect("the gameplay proxy carries its commands")
        .sender();

    // The app entity: the slots the frame behaviours fill in.
    let app = world.spawn((
        GpuState::Idle,
        RenderRef(None),
        SurfaceSlot(None),
        AttachmentsSlot(None),
        Offscreen(None),
        BlitRef(None),
        GpuSlot(None),
        LastFrame(Instant::now()),
        ExitRequest(false),
        SetupCargo(Some(SetupCargoInner {
            frame_reader,
            render,
            gameplay: gameplay_sender.clone(),
            audio: audio_sender.clone(),
        })),
        McpEnabled(mcp),
        WinitProxy(proxy),
    ));
    world.spawn_frame_behaviour(request_gpu(app));
    world.spawn_frame_behaviour(draw(app));
    world.spawn((redraw(app),));
    world.spawn((pump(app),));
    world.spawn((handle_user_event(app),));

    // Window events: resize on its own entity, keys on another, so the
    // two behaviour families never share a borrow. Space drops a ball; M
    // plays the demo sequence through the audio world's MIDI player.
    world.spawn((resize(app),));
    // Space drops a body; the sequence, the pause and the stop live on
    // the render world's control panel.
    world.spawn((OnKey::new(move |_world, _entity, key| {
        if !key.pressed || key.repeat {
            return;
        }
        if key.key == Key::Space {
            request_drop(&gameplay_sender);
        }
    }),));
    // The frame's input: the host dispatches its own key behaviours
    // before the draw, the render world's UI source reads the same
    // events while it draws, and the events are forgotten once the
    // frame is out, so nothing hears them twice.
    world.spawn_frame_behaviour(OnFrame::new(
        FrameBehaviourOrder(FRAME_DISPATCH_INPUT),
        |world, _entity, _frame| {
            dispatch_input(world);
        },
    ));
    world.spawn_frame_behaviour(OnFrame::new(
        FrameBehaviourOrder(FRAME_CLEAR_INPUT),
        |world, _entity, _frame| {
            let handle = world.query::<&InputHandle>().next().map(|(_, h)| h.clone());
            if let Some(handle) = handle {
                handle.write().clear_events();
            }
        },
    ));

    host.run(event_loop).expect("the event loop runs");
}

/// The example's command line: switches for the Model Context Protocol
/// and the headless render.
#[derive(argh::FromArgs)]
struct Args {
    /// serve the worlds over the Model Context Protocol
    #[argh(switch)]
    mcp: bool,

    /// render headless instead of opening a window: the run's last frame
    /// and audio mix land in --out, then the process exits
    #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(dead_code))]
    #[argh(switch)]
    headless: bool,

    /// how many frames the headless render runs, at its fixed 60 Hz
    #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(dead_code))]
    #[argh(option, default = "300")]
    frames: u32,

    /// the directory the headless render writes its outputs into
    #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(dead_code))]
    #[argh(option, default = "\"headless\".to_owned()")]
    out: String,
}

fn main() -> ExitCode {
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Args = argh::from_env();
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    if args.headless {
        return run_headless(&args);
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("an event loop");
    windowed(args.mcp, event_loop);
    ExitCode::SUCCESS
}

/// Render the scene headless and write the run's outputs.
///
/// The headless run has no window, no threads and no audio output: the
/// same worlds run inline on this thread, stepped at their fixed rates,
/// and the run's last frame and audio mix are written as files.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn run_headless(args: &Args) -> ExitCode {
    if args.frames == 0 {
        log::error!("--frames must be at least 1");
        return ExitCode::FAILURE;
    }
    let (device, queue, capabilities) = match headless_gpu() {
        Ok(gpu) => gpu,
        Err(error) => {
            log::error!("the headless run could not open a GPU: {error}");
            return ExitCode::FAILURE;
        }
    };
    let config = headless::HeadlessConfig {
        size: WINDOW_SIZE,
        frames: args.frames,
        capture: vec![args.frames - 1],
        audio_sample_rate: headless::AUDIO_SAMPLE_RATE,
        soundfont: SOUNDFONT.into(),
    };
    let output = headless::run(&config, &device, &queue, capabilities);
    let out = std::path::Path::new(&args.out);
    if let Err(error) = std::fs::create_dir_all(out) {
        log::error!("could not create {}: {error}", out.display());
        return ExitCode::FAILURE;
    }
    let png_path = out.join("screenshot.png");
    let mut png = Vec::new();
    if let Err(error) = image::codecs::png::PngEncoder::new(&mut png).write_image(
        &output.frames[0],
        config.size.0,
        config.size.1,
        image::ExtendedColorType::Rgba8,
    ) {
        log::error!("could not encode the screenshot: {error}");
        return ExitCode::FAILURE;
    }
    let wav_path = out.join("audio.wav");
    if let Err(error) = headless::write_wav(&wav_path, &output.audio, config.audio_sample_rate) {
        log::error!("could not write the audio: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = std::fs::write(&png_path, png) {
        log::error!("could not write {}: {error}", png_path.display());
        return ExitCode::FAILURE;
    }
    log::info!(
        "the headless run wrote {} and {}",
        png_path.display(),
        wav_path.display()
    );
    ExitCode::SUCCESS
}

/// A GPU context for the headless run: the same headless adapter the
/// snapshot tests use.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn headless_gpu()
-> Result<(wgpu::Device, wgpu::Queue, DeviceCapabilities), Box<dyn std::error::Error>> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        ..Default::default()
    }))
    .map_err(|error| format!("no adapter for a headless GPU context: {error}"))?;
    let tier = DeviceTier::from_env();
    let capabilities = tier.capabilities_of(&adapter);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: tier.limits(&adapter.limits()),
        ..Default::default()
    }))?;
    Ok((device, queue, capabilities))
}
