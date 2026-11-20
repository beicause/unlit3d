//! The windowed run: a window on the display, the worlds on threads of
//! their own or stepped by the frame.
//!
//! The host world owns the window and the render sub-world. A desktop run
//! gives physics, audio and gameplay a thread each by default; a browser,
//! and a desktop run that asked for the stepped path, drive every world
//! from the frame loop that draws them. What differs is who calls a tick
//! and how it is paced; the scene is the same either way.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use unlit3d::winit::presenter::PRESENT_ORDER;

use unlit_ecs::{Entity, bundle};
use unlit_wgpu::context::RenderDevice;
use unlit3d::egui::EguiTarget;
use unlit3d::frame::{FrameBehaviourOrder, OnFrame};
use unlit3d::input::winit::WinitInput;
use unlit3d::input::{InputFeed, InputWriter, refresh_input};
use unlit3d::relay::Snapshot;
use unlit3d::winit::SurfaceOptions;
use unlit3d::winit::builtin::{
    ExitOn, ExitRequest, ExitTrigger, Resumed, WindowSetup, request_redraw_while_foreground,
    window, winit_input_behaviour,
};
use unlit3d::winit::event::WinitHost;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

use super::App;
use super::capture::Capture;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use super::capture::capture_presented;
use super::headless::RENDER_COMMAND_CAPACITY;
#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
use super::scene::ClientPacing;
use super::scene::{Mode, RenderPorts, RunFrames, ScenePorts, Stepping};
use crate::gpu::request_gpu;
use crate::host::spawn_sub_world;
use crate::present::{draw_presented, pump_presented, setup_presenter};
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
/// # Panics
///
/// Panics when the event loop fails, which is fatal for a windowed run:
/// there is nothing to fall back to.
pub fn windowed<A: App>(mut app: A, options: WindowOptions, event_loop: EventLoop<()>) {
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
    // The frames the run has advanced, built before anything reads it so the
    // behaviours that spend a budget and keep a capture count the same frames
    // the scene does.
    let frames = RunFrames::default();
    // The stepped worlds run before the draw, so the transforms they publish
    // are the ones this frame shows. The two pacing paths are exclusive: a
    // client-paced run parks its stepper in the host world, where the MCP
    // tools reach it and no frame behaviour steps behind their back; a
    // self-paced run keeps the stepper in its frame behaviour, and no tool
    // can reach it to advance the scene a second way.
    if stepped {
        let stepper = Stepping::new(worlds, A::PHYSICS_HZ, None, frames.clone());
        let stepper = Arc::new(Mutex::new(stepper));
        // Parking the stepper is what lets a client step the scene, and
        // only a build with the tools has a client to hand it to: without
        // one the switch would park the run with nothing left to unpark it,
        // so the frame behaviour paces the run instead.
        let parked = options.interactive
            && cfg!(all(
                feature = "mcp",
                not(target_arch = "wasm32"),
                not(target_os = "android")
            ))
            && (options.mcp_stdio || options.mcp_http_addr.is_some());
        if parked {
            #[cfg(all(
                feature = "mcp",
                not(target_arch = "wasm32"),
                not(target_os = "android")
            ))]
            world
                .root_scope_mut()
                .spawn(bundle![ClientPacing(Arc::clone(&stepper))]);
        } else {
            world
                .root_scope_mut()
                .spawn(bundle![OnFrame::new(move |world, _entity, frame| {
                    // A suspended host shows nothing, so the frame it would
                    // advance is not one the run advanced: stepping through
                    // the suspension would spend the budget on frames nobody
                    // ever saw.
                    let resumed = world
                        .root_scope()
                        .query::<&Resumed>()
                        .iter()
                        .next()
                        .is_none_or(|state| state.0);
                    if !resumed {
                        return;
                    }
                    if let Ok(mut stepper) = stepper.lock() {
                        stepper.step(world, f64::from(frame.delta_time));
                    }
                },)]);
        }
    }
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
    world
        .root_scope_mut()
        .spawn(bundle![request_gpu(app_entity, {
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
                let outcome = setup_presenter(
                    world,
                    render.entity,
                    setup.renderer.entity,
                    setup.pass.entity,
                    gpu,
                    options.size,
                    offscreen,
                    options.surface,
                );
                if let Err(error) = outcome {
                    log::error!("failed to build the presentation path: {error}");
                } else {
                    log::info!("the scene is set up");
                }
                #[cfg(all(
                    feature = "mcp",
                    not(target_arch = "wasm32"),
                    not(target_os = "android")
                ))]
                if mcp {
                    crate::mcp::serve_mcp(world);
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
                            log::error!("the MCP HTTP server failed to start: {message}");
                        }
                    }
                }
            }
        }),]);
    // A stepped run has its stepper advance the count as it steps; a
    // threaded one has no stepper, so the draw counts its own frames.
    let drawn = if stepped { None } else { Some(frames.clone()) };
    world
        .root_scope_mut()
        .spawn(bundle![draw_presented(render_entity, drawn)]);
    // A capture reads the target the presenter just drew into, so it runs
    // one order after it rather than drawing the scene a second time. Reading
    // a frame back needs a filesystem to write it to, which this target has
    // not.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let reported = capture.clone();
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let capturing = reported.is_some();
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    let capturing = false;
    // Only a bounded run or one that captures ends itself: a plain window has
    // neither a length to spend nor a shortfall to report.
    if options.max_frames.is_some() || capturing {
        world.root_scope_mut().spawn(bundle![end_after_frames(
            app_entity,
            options.max_frames,
            frames.clone()
        )]);
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    if let Some(capture) = capture {
        world.root_scope_mut().spawn(bundle![capture_presented(
            render_entity,
            capture,
            frames.clone()
        )]);
    }
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    let _ = &capture;
    let (pump, redraw) = pump_presented();
    world.root_scope_mut().spawn(bundle![pump]);
    world.root_scope_mut().spawn(bundle![redraw]);
    world
        .root_scope_mut()
        .spawn(bundle![unlit3d::winit::presenter::resize_presented()]);

    host.run(event_loop).expect("the event loop runs");

    // A capture that ended short says so: its frames ran out before the run
    // reached the last one it named, or the window closed first.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    if let Some(capture) = reported {
        let advanced = frames.advanced();
        if let Some(frame) = capture.missing(advanced) {
            log::warn!(
                "the run advanced {advanced} frames and never reached frame {frame}, so the capture is incomplete"
            );
        }
    }
}

/// Close the window once the run has advanced as many frames as it was given.
///
/// It counts the frames the scene advanced, not the turns the event loop took:
/// a turn that suspended the app or found no presentation path drew nothing
/// and advanced nothing, and spending a budget on it would end a client-paced
/// run while the client is still stepping it. The frame the budget names is
/// the one the run draws last, since the request lands after the presenter and
/// the capture have seen it.
fn end_after_frames(app: Entity, budget: Option<u32>, frames: RunFrames) -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER + 2),
        move |world, _entity, _frame| {
            let spent = budget.is_some_and(|budget| frames.advanced() >= budget);
            if !spent {
                return;
            }
            if let Ok(mut request) = world.root_scope().try_query_one::<&mut ExitRequest>(app) {
                request.get().0 = true;
            }
        },
    )
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
