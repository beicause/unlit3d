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

mod commands;
mod frame;
mod host;
mod present;
mod scene;
mod state;

#[cfg(test)]
mod tests;

use self::commands::*;
use self::frame::*;
use self::host::*;
use self::present::*;
use self::scene::*;
use self::state::*;

pub use self::scene::Scene;
pub use self::state::PanelRects;
