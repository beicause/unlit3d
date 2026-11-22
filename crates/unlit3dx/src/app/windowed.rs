//! The windowed run: a window on the display, the worlds on threads of
//! their own or stepped by the frame.
//!
//! The host world owns the window and the render sub-world. A desktop run
//! gives physics, audio and gameplay a thread each by default; a browser,
//! and a desktop run that asked for the stepped path, drive every world
//! from the frame loop that draws them. What differs is who calls a tick
//! and how it is paced; the scene is the same either way.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use unlit_ecs::bundle;
use unlit_wgpu::context::RenderDevice;
use unlit3d::egui::EguiTarget;
use unlit3d::frame::OnFrame;
use unlit3d::input::winit::WinitInput;
use unlit3d::input::{InputFeed, InputWriter, refresh_input};
use unlit3d::relay::Snapshot;
use unlit3d::winit::SurfaceOptions;
use unlit3d::winit::builtin::{
    ExitOn, ExitRequest, ExitTrigger, WindowSetup, request_redraw_while_foreground, window,
    winit_input_behaviour,
};
use unlit3d::winit::event::WinitHost;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

use super::App;
use super::capture::Capture;
use super::headless::RENDER_COMMAND_CAPACITY;
use super::pace::Pacing;
use super::scene::{Mode, RenderPorts, RunFrames, ScenePorts, Stepping};
use super::window_frame::{RunFailure, WindowFrames, drive_window};
use crate::gpu::request_gpu;
use crate::host::spawn_sub_world;
use crate::present::{pump_presented, setup_presenter};
use crate::world::{Stepped, Threaded};

/// A windowed run's settings.
pub struct WindowOptions {
    /// The window's title.
    pub title: String,
    /// The size the window opens at, and the size a frame falls back to
    /// before the surface reports its own.
    pub size: (u32, u32),
    /// The render sub-world's mailbox capacity.
    pub render_capacity: usize,
    /// Drive the worlds from the frame loop rather than threads of their
    /// own. A browser always takes this path, whatever the option says.
    /// A client-paced run always takes it too: threads of their own would
    /// step behind the client's back.
    pub stepped: bool,
    /// Leave the frames to the client: no frame behaviour steps the
    /// worlds, and the MCP tools step them through the parked stepper
    /// instead.
    pub interactive: bool,
    /// Serve the scene over MCP on standard input and output, drawing into
    /// an offscreen target so a client can read frames back.
    pub mcp_stdio: bool,
    /// Serve the scene over MCP on a TCP port as well, so network clients
    /// reach the host while the window stays up. The address is bound once
    /// the GPU request resolves; port 0 leaves the port to the OS. Only a
    /// build with the mcp-http feature serves it.
    pub mcp_http_addr: Option<SocketAddr>,
    /// The most frames the run advances before it ends, when it is bounded.
    ///
    /// A run with no bound ends when its window closes; one bounded here
    /// closes its own window at that frame, so a windowed run and a headless
    /// one stop at the same place.
    pub max_frames: Option<u32>,
    /// How the surface presents.
    pub surface: SurfaceOptions,
    /// The frames the run writes out, and where.
    ///
    /// A capture asks the run to present offscreen — the swap chain image is
    /// not readable — and to read each frame it names back after the
    /// presenter has drawn it.
    pub capture: Option<Capture>,
}

impl WindowOptions {
    /// The settings an application's associated constants name.
    #[must_use]
    pub fn for_app<A: App>() -> Self {
        Self {
            title: A::TITLE.to_owned(),
            size: A::SIZE,
            ..Self::default()
        }
    }
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: "unlit3dx".to_owned(),
            size: (1024, 720),
            render_capacity: RENDER_COMMAND_CAPACITY,
            stepped: cfg!(target_arch = "wasm32"),
            interactive: false,
            mcp_stdio: false,
            mcp_http_addr: None,
            max_frames: None,
            surface: SurfaceOptions::default(),
            capture: None,
        }
    }
}

/// Run an application's scene in a window until the window closes or Escape
/// is pressed.
///
/// # Errors
///
/// Returns the first fatal initialization or frame error on native targets,
/// or an error starting the event loop. Asynchronous browser failures are logged.
pub fn windowed<A: App>(
    mut app: A,
    options: WindowOptions,
    event_loop: EventLoop<()>,
) -> Result<(), String> {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut host = WinitHost::<()>::new();
    let world = host.world_mut();

    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    world
        .root_scope_mut()
        .spawn(bundle![winit_input_behaviour(WinitInput::new(
            input.clone()
        )),]);

    let attributes = Window::default_attributes()
        .with_title(options.title)
        .with_inner_size(winit::dpi::LogicalSize::new(options.size.0, options.size.1));
    // A browser has no window of its own to attach to: winit appends the
    // canvas it creates to the page body.
    #[cfg(target_arch = "wasm32")]
    let attributes = {
        use winit::platform::web::WindowAttributesExtWebSys;
        attributes.with_append(true)
    };
    world.root_scope_mut().spawn(WindowSetup { attributes });
    world
        .root_scope_mut()
        .spawn(ExitOn(ExitTrigger::CloseRequested));
    world.root_scope_mut().spawn(ExitOn(ExitTrigger::Escape));
    // The redraw request is a behaviour: spawned, it asks the window for a
    // frame whenever the world is resumed. Dropping the value would install
    // nothing and the window would never redraw.
    world
        .root_scope_mut()
        .spawn(bundle![request_redraw_while_foreground()]);

    // The render sub-world: spawned empty, on the main thread, before anyone
    // needs it, so the scene holds its proxy from the first tick. Its content
    // lands once the GPU request resolves.
    let render = spawn_sub_world(world, "render", options.render_capacity);
    let panel = Snapshot::<EguiTarget>::new();
    let refresh_millihertz = Arc::new(AtomicU32::new(0));

    let stepped = options.stepped || options.interactive || cfg!(target_arch = "wasm32");
    let ports = ScenePorts {
        render: &render,
        input: &input,
        panel: &panel,
        refresh_millihertz: refresh_millihertz.clone(),
        mode: Mode::Windowed,
    };
    let worlds = if stepped {
        app.spawn_scene::<Stepped>(world, ports)
    } else {
        app.spawn_scene::<Threaded>(world, ports)
    };
    let frames = RunFrames::default();
    let pacing = Pacing {
        client: options.interactive
            && cfg!(all(
                feature = "mcp",
                not(any(target_arch = "wasm32", target_os = "android"))
            ))
            && (options.mcp_stdio
                || (cfg!(feature = "mcp-http") && options.mcp_http_addr.is_some())),
        frames: options.max_frames,
    };
    world
        .root_scope_mut()
        .spawn(bundle![frames.clone(), pacing]);
    let stepper = stepped.then(|| Stepping::new(worlds, A::PHYSICS_HZ, None));
    world
        .root_scope_mut()
        .spawn(bundle![display_refresh_rate(refresh_millihertz)]);

    // The host holds a buffer so the MCP tools can feed one, and a buffer no
    // one folds keeps every event it is fed. Nothing here dispatches: the
    // host has no key behaviour of its own.
    world
        .root_scope_mut()
        .spawn(bundle![OnFrame::new(|world, _entity, _frame| {
            refresh_input(world);
        }),]);

    // The app entity and the behaviours that drive it. The GPU request
    // resolves into the scene setup, which fills the render sub-world and
    // builds the presentation path; from then on the pump and the redraw
    // requests dispatch frames into it.
    let app_entity = world.root_scope_mut().spawn(bundle![ExitRequest(false)]);
    let render_entity = render.entity;
    let mcp = options.mcp_stdio;
    let capture = options.capture.clone();
    let captured = capture.is_some();
    let failure = RunFailure::default();
    world.root_scope_mut().spawn(bundle![
        drive_window(),
        WindowFrames {
            render: render_entity,
            app: app_entity,
            stepper,
            frames: frames.clone(),
            pacing,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            capture: capture.clone(),
            failure: failure.clone(),
        }
    ]);
    world.root_scope_mut().spawn(bundle![request_gpu(
        app_entity,
        {
            let failure = failure.clone();
            move |world, gpu| {
                let setup = app.setup_render(
                    world,
                    RenderPorts {
                        world: render.entity,
                        device: &RenderDevice::from(gpu),
                        panel: Some(panel.clone()),
                    },
                );
                // Two things need the offscreen target: the MCP tools, which
                // read the scene back whichever transport serves them, and a
                // capture, which reads the frame the presenter drew. A plain
                // window draws into the swap chain, which cannot be read.
                let offscreen = mcp
                    || captured
                    || (cfg!(feature = "mcp-http") && options.mcp_http_addr.is_some());
                let outcome = (|| {
                    let (renderer, pass) = {
                        let mut nested = world
                            .root_scope()
                            .try_query_one::<&unlit_ecs::World>(render.entity)
                            .map_err(|error| format!("the render world is unavailable: {error}"))?;
                        let nested = nested.get();
                        (
                            nested
                                .resolve(setup.renderer)
                                .map_err(|error| error.to_string())?,
                            nested
                                .resolve(setup.pass)
                                .map_err(|error| error.to_string())?,
                        )
                    };
                    setup_presenter(
                        world,
                        render.entity,
                        renderer,
                        pass,
                        gpu,
                        options.size,
                        offscreen,
                        options.surface,
                    )
                    .map_err(|error| error.to_string())
                })();
                if let Err(error) = outcome {
                    failure.report(
                        world,
                        app_entity,
                        format!("failed to build the presentation path: {error}"),
                    );
                    return;
                }
                log::info!("the scene is set up");
                #[cfg(all(
                    feature = "mcp",
                    not(target_arch = "wasm32"),
                    not(target_os = "android")
                ))]
                if mcp && let Err(error) = crate::mcp::serve_mcp(world) {
                    failure.report(
                        world,
                        app_entity,
                        format!("the MCP stdio server failed to start: {error}"),
                    );
                    return;
                }
                #[cfg(all(
                    feature = "mcp-http",
                    not(target_arch = "wasm32"),
                    not(target_os = "android")
                ))]
                if let Some(addr) = options.mcp_http_addr {
                    match crate::mcp::serve_http_mcp(world, addr) {
                        Ok(bound) => {
                            log::info!("the MCP HTTP server is listening on http://{bound}/");
                        }
                        Err(message) => {
                            failure.report(
                                world,
                                app_entity,
                                format!("the MCP HTTP server failed to start: {message}"),
                            );
                        }
                    }
                }
            }
        },
        {
            let failure = failure.clone();
            move |world, message| failure.report(world, app_entity, message)
        }
    ),]);
    let (pump, redraw) = pump_presented();
    world.root_scope_mut().spawn(bundle![pump]);
    world.root_scope_mut().spawn(bundle![redraw]);
    world
        .root_scope_mut()
        .spawn(bundle![unlit3d::winit::presenter::resize_presented()]);

    host.run(event_loop)
        .map_err(|error| format!("the event loop failed: {error}"))?;

    // A capture that ended short says so: its frames ran out before the run
    // reached the last one it named, or the window closed first.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    if let Some(capture) = capture {
        let advanced = frames.advanced();
        if let Some(frame) = capture.missing(advanced) {
            log::warn!(
                "the run advanced {advanced} frames and never reached frame {frame}, so the capture is incomplete"
            );
        }
    }
    failure.take().map_or(Ok(()), Err)
}

/// Watch the display the window is on and publish its refresh rate.
///
/// The rate is read every frame, so moving the window to another display or
/// a display changing mode is followed by the next frame. A display that
/// reports nothing leaves the rate at zero, which the gameplay world reads
/// as "keep the default".
fn display_refresh_rate(refresh_millihertz: Arc<AtomicU32>) -> OnFrame {
    OnFrame::new(move |world, _entity, _frame| {
        let rate = window(world)
            .and_then(|window| window.current_monitor())
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .unwrap_or(0);
        refresh_millihertz.store(rate, Ordering::Relaxed);
    })
}
