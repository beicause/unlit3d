//! The multi-world demo's library half: the scene the windowed binary
//! shows, the thread-free headless run the --headless mode and the
//! snapshot tests drive, and the entry point the Android activity calls.
//!
//! A multi-world demo: one scene where rendering, physics and audio each
//! run in their own world, joined by jobs and snapshots.
//!
//! The host world owns the window and drives the frame. A render sub-world
//! sits on the main thread as a nested world and serves the others: it
//! allocates meshes, materials and instances when the scene asks, and the
//! entity bits it answers with are the handles the rest of the show uses.
//! The scene's other worlds — physics, audio and gameplay — are spawned
//! through one runner, so the host carries the same entities and components
//! whether they run on threads of their own or are stepped by the host's
//! frames. The gameplay world holds the scene's objects: one entity each for
//! the scene's assets, the floor, the shapes and the music, every one of them
//! ticking itself. The panel is the fifth object, and lives in the gameplay
//! world with the rest: it lays its frames out there and sends them to the
//! render world, which holds and draws them.
//!
//! Space drops a body. The render world's control panel drives two music
//! tracks — a looping background piece and a one-shot MIDI sequence — each
//! of which pauses, resumes and rewinds on its own, and it drops the next
//! shape or puts every live shape away.
//! Bodies fall, bounce off the ground and each other while the music plays.
//! The soundfont is embedded at
//! build time: build.rs reads the FluidR3Mono_GM.sf3 file the repository
//! keeps out of version control under assets, and the demo runs silent
//! when it is missing.
//!
//! The --headless switch runs the same scene without a window or
//! threads: the worlds are stepped at fixed rates, the run keeps its
//! last frame and audio mix, and writes them into --out. A browser build
//! steps the worlds the same way, because the target has no threads to
//! put them on, and --stepped asks a windowed run to take that same path.

pub mod headless;
pub mod render;
pub mod scene;

#[cfg(target_os = "android")]
pub mod android;

use argh::FromArgs;
use std::process::ExitCode;
use unlit_ecs::bundle;

use crate::scene::*;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use image::ImageEncoder;
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::winit::builtin::{
    CreateWindowRequest, ExitRequest, WindowSpec, create_window_on_resume, exit_on_close_requested,
    exit_on_escape, request_redraw_while_foreground, winit_input_behaviour,
};
use unlit3d::winit::event::WinitHost;
use unlit3dx::prelude::*;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

/// Install the logger backend this target writes through.
///
/// A browser and an activity have no terminal: the browser's log goes to the
/// console and its panics are reported there instead of unwinding into an
/// empty page, and an activity's go to logcat.
pub fn init_logging() {
    #[cfg(target_arch = "wasm32")]
    {
        console_log::init_with_level(log::Level::Info).ok();
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
}

/// Run the whole show: five worlds, one window.
///
/// The host world owns the window and the render sub-world, which serves
/// the scene's assets; the physics, audio and gameplay worlds are remote.
/// The scene's objects build it by requesting from the render server, jobs
/// go out, snapshots come back, and nothing but data crosses a thread
/// boundary. With the mcp flag, a Model Context Protocol server over
/// stdio reads the host and its render world. A `stepped` run drives the
/// worlds from the frame loop instead of threads of their own: that is
/// what a browser build always does, and what a desktop run asks for to
/// exercise the same path.
pub fn windowed(mcp: bool, stepped: bool, event_loop: EventLoop<()>) {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut host = WinitHost::<()>::new();
    let world = host.world_mut();

    let input = InputWriter::new();
    world.root_scope_mut().spawn(bundle![input.clone()]);
    // The host world dispatches its own key behaviours, so it subscribes for
    // the events it has not read yet rather than reading a shared list.
    world.root_scope_mut().spawn(bundle![input.subscribe()]);
    world
        .root_scope_mut()
        .spawn(bundle![winit_input_behaviour(WinitInput::new(
            input.clone()
        )),]);

    let attributes = Window::default_attributes()
        .with_title("unlit3dx multi-world")
        .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
    // A browser has no window of its own to attach to: winit appends the
    // canvas it creates to the page body.
    #[cfg(target_arch = "wasm32")]
    let attributes = {
        use winit::platform::web::WindowAttributesExtWebSys;
        attributes.with_append(true)
    };
    world.root_scope_mut().spawn(bundle![
        WindowSpec(attributes),
        CreateWindowRequest(None),
        create_window_on_resume(),
    ]);
    world
        .root_scope_mut()
        .spawn(bundle![ExitRequest(false), exit_on_close_requested()]);
    world
        .root_scope_mut()
        .spawn(bundle![ExitRequest(false), exit_on_escape()]);
    request_redraw_while_foreground();

    // The render sub-world: spawned empty, on the main thread, before
    // anyone needs it, so gameplay holds its proxy from the first tick.
    // Its content lands once the GPU request resolves.
    let render = spawn_sub_world(world, "render", RENDER_COMMAND_CAPACITY);

    // The worlds behind the scene: threads of their own by default,
    // stepped by the frame when the target has none to give them - a
    // browser - or when the run asked for the stepped path.
    // The control panel lives in the gameplay world and sends each frame to
    // the render world, which draws it. Neither asks the other for anything.
    let panels = PanelSlots::default();
    let worlds = if stepped {
        spawn_scene::<Stepped>(
            world,
            render.clone(),
            stepped_audio(),
            panels.clone(),
            &input,
        )
    } else {
        spawn_scene::<Threaded>(
            world,
            render.clone(),
            AudioSetup::Device,
            panels.clone(),
            &input,
        )
    };
    let gameplay_sender = worlds.gameplay.sender.clone();

    // The stepped worlds run before the draw, so the transforms they
    // publish are the ones this frame shows.
    if stepped {
        let stepper = SceneStepper::new(GAME_HZ, PHYSICS_HZ, None);
        world
            .root_scope_mut()
            .spawn(bundle![stepped_frame(worlds, stepper)]);
    }

    // The app entity and the behaviours that drive it. The GPU request
    // resolves into the scene setup, which fills the render sub-world and
    // builds the presentation path; from then on the pump and the redraw
    // requests dispatch frames into it.
    let app = world.root_scope_mut().spawn(bundle![ExitRequest(false)]);
    world.root_scope_mut().spawn(bundle![request_gpu(app, {
        // Only an MCP-driven scene needs the offscreen target: it is what
        // a screenshot reads back. A plain window draws into the swap chain.
        let offscreen = mcp;
        move |world, gpu| {
            let (renderer, pass) = setup_render_world(
                world,
                render.entity,
                &gpu.device,
                &gpu.queue,
                gpu.capabilities,
            );
            // The scene's own content — its camera, and the source that draws
            // whatever the gameplay world publishes — reaches the world it was
            // set up to draw as jobs. The panels themselves never leave the
            // world that holds them; what crosses is the slots they publish
            // into.
            mount_scene_content(&render, pass, Some(panels.clone()));
            let outcome = setup_presenter(
                world,
                render.entity,
                renderer,
                pass,
                gpu,
                WINDOW_SIZE,
                offscreen,
            );
            if let Err(error) = outcome {
                log::error!("failed to build the presentation path: {error}");
            } else {
                log::info!("the scene is set up");
            }
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
            if mcp {
                serve_mcp(world);
            }
        }
    }),]);
    world.root_scope_mut().spawn(bundle![draw_presented()]);
    let (pump, redraw) = pump_presented();
    world.root_scope_mut().spawn(bundle![pump]);
    world.root_scope_mut().spawn(bundle![redraw]);
    world.root_scope_mut().spawn(bundle![resize_presented()]);

    // Space drops a body; the music and the shapes are driven from the
    // render world's control panel.
    world
        .root_scope_mut()
        .spawn(bundle![OnKey::new(move |_world, _entity, key| {
            if !key.pressed || key.repeat {
                return;
            }
            if key.key == Key::Space {
                drop_ball(&gameplay_sender);
            }
        },),]);
    // The frame's input: the host dispatches its own key behaviours before
    // the draw. Nothing clears anything afterwards — each world and the
    // render world's UI source read their own copy of the stream.
    world.root_scope_mut().spawn(bundle![OnFrame::new(
        FrameBehaviourOrder(FRAME_DISPATCH_INPUT),
        |world, _entity, _frame| {
            dispatch_input(world);
        },
    ),]);

    host.run(event_loop).expect("the event loop runs");
}

/// Step the scene's worlds once per frame, before the draw.
///
/// A stepped run has no threads, so the host steps what a threaded build
/// would leave to them. The stepper follows the frame's own duration and
/// carries the step remainders, so the worlds keep their rates whatever
/// the display redraws at.
fn stepped_frame(worlds: SceneHandles, mut stepper: SceneStepper) -> OnFrame {
    OnFrame::new(
        FrameBehaviourOrder(FRAME_INLINE_STEP),
        move |world, _entity, frame| {
            stepper.step(world, &worlds, f64::from(frame.delta_time));
        },
    )
}

/// The audio setup a windowed run takes when the host steps the worlds.
///
/// A browser's stepped world opens the page's Web Audio context: the page
/// drives it, so it needs no thread of its own, and the scene's tracks
/// sound once the context is running. A desktop run that asked for the
/// stepped path has nothing to play to and stays silent.
fn stepped_audio() -> AudioSetup {
    if cfg!(target_arch = "wasm32") {
        AudioSetup::Device
    } else {
        AudioSetup::Silent
    }
}

/// The example's command line: switches for the Model Context Protocol,
/// the headless render and the stepped runner.
#[derive(argh::FromArgs)]
pub struct Args {
    /// serve the worlds over the Model Context Protocol
    #[argh(switch)]
    mcp: bool,

    /// drive the worlds from the frame loop instead of threads of their
    /// own, the path a browser build always takes
    #[argh(switch)]
    stepped: bool,

    /// render headless instead of opening a window: the run's last frame
    /// and audio mix land in --out, then the process exits
    #[cfg_attr(
        any(target_arch = "wasm32", target_os = "android"),
        expect(
            dead_code,
            reason = "only the headless run reads it, and only the host build has one"
        )
    )]
    #[argh(switch)]
    headless: bool,

    /// how many frames the headless render runs, at its fixed 60 Hz
    #[cfg_attr(
        any(target_arch = "wasm32", target_os = "android"),
        expect(
            dead_code,
            reason = "only the headless run reads it, and only the host build has one"
        )
    )]
    #[argh(option, default = "300")]
    frames: u32,

    /// the directory the headless render writes its outputs into
    #[cfg_attr(
        any(target_arch = "wasm32", target_os = "android"),
        expect(
            dead_code,
            reason = "only the headless run reads it, and only the host build has one"
        )
    )]
    #[argh(option, default = "\"headless\".to_owned()")]
    out: String,
}

/// Read the command line into [`Args`].
///
/// A browser has no command line: `std::env::args()` is empty there, and
/// argh refuses to parse without a program name. Supplying one leaves every
/// option at its default, which is what a browser build runs with.
fn parse_args() -> Result<Args, String> {
    let mut raw: Vec<String> = std::env::args().collect();
    if raw.is_empty() {
        raw.push(env!("CARGO_PKG_NAME").to_owned());
    }
    let refs: Vec<&str> = raw.iter().map(String::as_str).collect();
    match Args::from_args(&refs[..1], &refs[1..]) {
        Ok(args) => Ok(args),
        // argh reports `--help` as an early exit whose status is `Ok`, and a
        // failed parse as one whose status is `Err`; only the text is useful
        // either way.
        Err(exit) if exit.status.is_ok() => {
            log::info!("{}", exit.output);
            std::process::exit(0);
        }
        Err(exit) => Err(exit.output),
    }
}

/// Run the example the binary was started as.
pub fn run() -> ExitCode {
    init_logging();
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            log::error!("the command line could not be read: {message}");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    if args.headless {
        return run_headless(&args);
    }
    let event_loop = EventLoop::<()>::with_user_event()
        .build()
        .expect("an event loop");
    // A browser has no threads to give the worlds, so it steps them
    // whatever the command line says; the switch is how a desktop run
    // reaches that same path.
    windowed(
        args.mcp,
        args.stepped || cfg!(target_arch = "wasm32"),
        event_loop,
    );
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
    let gpu = match pollster::block_on(Gpu::headless()) {
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
        soundfont: unlit3dx_asset_bytes::soundfont().into(),
    };
    let output = headless::run(&config, &gpu.device, &gpu.queue, gpu.capabilities);
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
    let flac_path = out.join("audio.flac");
    if let Err(error) = headless::write_flac(&flac_path, &output.audio, config.audio_sample_rate) {
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
        flac_path.display()
    );
    ExitCode::SUCCESS
}
