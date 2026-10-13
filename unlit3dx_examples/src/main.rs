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
//! the render server, and publishes transforms.
//!
//! Space drops a body. The render world's control panel drives two music
//! tracks — a looping background piece and a one-shot MIDI sequence — each
//! of which pauses, resumes and rewinds on its own, and it drops the next
//! shape or puts every live shape back in the pool.
//! Bodies fall, bounce off the ground and each other while the music plays.
//! The soundfont is embedded at
//! build time: build.rs reads the FluidR3Mono_GM.sf3 file the repository
//! keeps out of version control under assets, and the demo runs silent
//! when it is missing.
//!
//! The --headless switch runs the same scene without a window or
//! threads: the worlds step inline at fixed rates, the run keeps its
//! last frame and audio mix, and writes them into --out.

use std::{process::ExitCode, time::Duration};

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use image::ImageEncoder;
use unlit3d::input::winit::WinitInput;
use unlit3d::prelude::*;
use unlit3d::winit::builtin::{
    CreateWindowRequest, ExitRequest, WindowSpec, create_window_on_resume, exit_on_close_requested,
    exit_on_escape, request_redraw_while_foreground, winit_input_behaviour,
};
use unlit3d::winit::event::{OnAboutToWait, WinitHost};
use unlit3dx::prelude::*;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use unlit3dx_examples::headless;
use unlit3dx_examples::scene::*;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

/// Run the whole show: five worlds, one window.
///
/// The host world owns the window and the render sub-world, which serves
/// the scene's assets; the physics, audio and gameplay worlds are remote.
/// Gameplay builds the scene by requesting from the render server, jobs
/// go out, snapshots come back, and nothing but data crosses a thread
/// boundary. With the mcp flag, a Model Context Protocol server over
/// stdio reads the host and its render world.
fn windowed(
    #[cfg_attr(
        any(target_arch = "wasm32", target_os = "android"),
        allow(unused_variables)
    )]
    mcp: bool,
    event_loop: EventLoop<()>,
) {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut host = WinitHost::<()>::new();
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

    // The physics world: fixed step, default gravity, a ground plane
    // ready before the first ball drops.
    let physics = spawn_physics_world(world, PhysicsConfig::default());
    let physics_sender = physics.sender();
    let physics_reader = world
        .get::<PhysicsInbox>(physics.entity())
        .expect("the physics world carries its inbox")
        .0
        .clone();

    // The audio world: one synthesizer voice plus one MIDI player per
    // track, all fed by the embedded soundfont, mixed into the output
    // stream on the audio thread. The sources are not installed here:
    // a synthesizer has to be built at the rate the device actually
    // opened at, and that is only known once the audio thread has its
    // stream. It publishes the format like any other world publishes a
    // snapshot, and the behaviour below waits for it and queues the
    // install as an ordinary job.
    let audio = spawn_audio_world(world, AudioConfig::default());
    let audio_sender = audio.sender();
    let audio_format = world
        .get::<AudioFormatInbox>(audio.entity())
        .expect("the audio world carries its format inbox")
        .0
        .clone();
    let audio_inbox = world
        .get::<AudioReadyInbox>(audio.entity())
        .expect("the audio world carries its readiness inbox")
        .0
        .clone();
    world.spawn((OnAboutToWait::new({
        let audio_sender = audio_sender.clone();
        let mut installed = false;
        move |_world: &World, _entity, ()| {
            if installed {
                return;
            }
            let Some(format) = audio_format.latest() else {
                return;
            };
            installed = true;
            if let Err(error) = audio_sender.send(install_audio_job(format.sample_rate)) {
                log::error!("the audio world refused its sources ({error:?})");
            }
        }
    }),));

    // The scene's channels, shared by both run modes: the wiring below
    // is the same whether the worlds are threads or inline.
    let (frame_writer, frame_reader) = snapshot_pair::<TransformFrame>();
    let scene = SceneWiring {
        frame_writer,
        frame_reader,
        physics_sender,
        physics_reader,
        audio_sender,
        audio_ready: audio_inbox,
    };
    scene.spawn_ground();

    // The gameplay world: one entity of state, ticked at a fixed rate,
    // driving the scene as the render server's client.
    let gameplay = spawn_remote_world_with(world, "gameplay", 64, (), {
        let render = render.clone();
        let scene = scene.clone();
        let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / GAME_HZ));
        move |world: &mut World, control: &ThreadControl| {
            if control.stop_requested() {
                return false;
            }
            if world.query::<&Gameplay>().next().is_none() {
                world.spawn((scene.gameplay(render.clone()),));
            }
            if let Some((_, mut game)) = world.query::<&mut Gameplay>().next() {
                game.tick();
            }
            clock.wait(control);
            true
        }
    });
    let gameplay_sender = gameplay.sender();

    // The app entity and the behaviours that drive it. The GPU request
    // resolves into the scene setup, which fills the render sub-world and
    // builds the presentation path; from then on the pump and the redraw
    // requests dispatch frames into it.
    let app = world.spawn((ExitRequest(false),));
    world.spawn_frame_behaviour(request_gpu(app, {
        let frame_reader = scene.frame_reader.clone();
        let gameplay_sender = gameplay_sender.clone();
        let audio_sender = scene.audio_sender.clone();
        // Only an MCP-driven scene needs the offscreen target: it is what
        // a screenshot reads back. A plain window draws into the swap chain.
        let offscreen = mcp;
        move |world, gpu| {
            let renderer = setup_render_world(
                world,
                render.entity(),
                &gpu.device,
                &gpu.queue,
                gpu.capabilities,
                frame_reader,
                Some(ControlHooks {
                    gameplay: gameplay_sender,
                    audio: audio_sender,
                }),
            );
            let outcome = setup_presenter(
                world,
                render.entity(),
                renderer,
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
    }));
    world.spawn_frame_behaviour(draw_presented());
    let (pump, redraw) = pump_presented();
    world.spawn((pump,));
    world.spawn((redraw,));
    world.spawn((resize_presented(),));

    // Space drops a body; the music and the shapes are driven from the
    // render world's control panel.
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
    let event_loop = EventLoop::<()>::with_user_event()
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
        soundfont: SOUNDFONT.into(),
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
