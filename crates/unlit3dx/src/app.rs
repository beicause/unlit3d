//! The skeleton a multi-world application runs on.
//!
//! A run and a scene are two different things, and this module keeps them
//! apart. A *scene* is what an application owns: the worlds it spawns, the
//! objects in them, the ways they talk. A *run* is how a scene is driven — a
//! window paced by the display, a capture without a window, an interactive
//! session stepped by its caller, an MCP host waiting for commands — and
//! everything a run needs is the same in each: a GPU, a render sub-world, a
//! presenter, a panel target, a frame loop.
//!
//! An application implements `App`: it spawns its scene when the run asks,
//! and builds the render world's pipeline when the device is ready. It never
//! opens a window, spawns a render sub-world, or writes a screenshot; those
//! belong to the run, which picks them from its entry point:
//!
//! - `run` reads the command line — its own switches merged with the
//!   application's arguments into one parser — and picks between the
//!   others, so a binary is one call to it.
//! - `windowed` runs the scene in a window on the display.
//! - `headless` steps the same scene without a window and reads back the
//!   frames and the mix it captured.
//! - `interactive` steps it one frame at a time, driven by its caller.
//! - `serve` runs `interactive` on a host and answers MCP tools over stdio.
//!
//! The worlds the scene hands back are driven by `Stepping`, which owns the
//! step remainders: gameplay ticks once per frame, physics and audio step as
//! their rates come due, so a run's frame rate never changes a rate the
//! scene asked for.
//!
//! # Where the scene's content comes from
//!
//! Nothing about a scene is in this module. The run spawns the render
//! sub-world, the panel target and the input buffer, and hands them over as
//! `ScenePorts`; the scene spawns its own worlds, objects and jobs, and
//! hands back the three handles `Stepping` drives. The render world's
//! pipeline is the scene's too: `App::setup_render` builds it, because
//! only the scene knows what it draws through.

use clap::Args as ClapArgs;
use unlit_ecs::World;
use winit::event_loop::EventLoop;

pub mod headless;
pub mod interactive;
pub mod options;
pub mod windowed;

mod logging;
mod scene;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod write;

#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp")))]
pub mod serve;

pub use headless::{
    AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, HEADLESS_HZ, HeadlessConfig, HeadlessOutput, headless,
};
pub use interactive::{Interactive, Recording, interactive};
pub use logging::init as init_logging;
pub use options::RunOptions;
pub use scene::{Mode, RenderPorts, RenderSetup, ScenePorts, SceneWorlds, Stepping};
pub use windowed::{WindowOptions, windowed};

#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp")))]
pub use serve::serve;

/// The rate a scene's physics world runs at when its application says
/// nothing else, in hertz.
pub const DEFAULT_PHYSICS_HZ: f64 = 60.0;

/// The slot a run publishes its panels into.
///
/// A run draws one panel surface, so every application publishes into the
/// same slot; the drawing side keys its textures by origin and id, so a
/// second slot would need a second source rather than a second index.
pub const PANEL_SLOT: usize = 0;

/// What an application owes a run.
///
/// The two calls are the whole contract: spawn the scene once, and build
/// the render world's pipeline once the device is ready. A run calls each
/// exactly once, and calls them in that order.
pub trait App: 'static {
    /// The title a windowed run opens its window with.
    const TITLE: &'static str = "unlit3dx";

    /// The size a windowed run opens at, and that a run without a window
    /// draws at when its configuration names no other.
    const SIZE: (u32, u32) = (1024, 720);

    /// The rate the scene's physics world runs at, in hertz.
    ///
    /// The run turns it into the period [`Stepping`] steps physics by; a
    /// scene whose physics world runs at another rate states it here rather
    /// than letting the run guess.
    const PHYSICS_HZ: f64 = DEFAULT_PHYSICS_HZ;

    /// The command line arguments the application reads, alongside the
    /// switches the run owns.
    ///
    /// The run flattens this type into its own parser, so the whole
    /// command line is read together: one `--help` describes both, and an
    /// argument neither side declares is refused by name. Derive
    /// [`clap::Args`] on a structure of your own; an application without
    /// arguments uses [`NoArgs`].
    type Args: clap::Args + Default;

    /// Receive the parsed [`App::Args`].
    ///
    /// Returning an error is what arguments that cannot be applied do —
    /// a file named on the command line that cannot be read, say: the run
    /// logs the message and fails.
    ///
    /// # Errors
    ///
    /// Returns the message to show when the arguments cannot be applied.
    fn configure(&mut self, args: Self::Args) -> Result<(), String> {
        let _ = args;
        Ok(())
    }

    /// Spawn the scene's worlds, objects and jobs into `host`.
    ///
    /// `ports` carries what the run holds and the scene draws through; the
    /// returned [`SceneWorlds`] is what the run steps. The runner `R` says
    /// who calls a tick; [`ScenePorts::mode`] says which run is asking,
    /// which is what a scene reads to choose its audio setup.
    fn spawn_scene<R: crate::world::WorldRunner>(
        &mut self,
        host: &mut World,
        ports: ScenePorts<'_>,
    ) -> SceneWorlds;

    /// Build the render world's pipeline: its context, renderer, pass and
    /// the sources the scene draws through.
    ///
    /// A windowed run calls it from the GPU callback, so a device exists by
    /// then and none did before. The returned [`RenderSetup`] is what the
    /// run presents.
    fn setup_render(&mut self, host: &mut World, ports: RenderPorts<'_>) -> RenderSetup;
}

/// The arguments an application without a command line keeps.
///
/// Rust has no associated-type defaults on stable, so an application
/// without arguments declares `type Args = NoArgs` itself: a run parses
/// an empty structure, and nothing the application did not declare
/// reaches it.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct NoArgs {}

/// Parse `argv` into the run's switches and the application's
/// parsed arguments: one parser reads both, so its errors and its `--help`
/// cover the whole command line.
fn read_command<A: clap::Args + Default>(argv: &[String]) -> Result<(RunOptions, A), clap::Error> {
    // One command reads both sides: the run augments a fresh command with
    // its own switches, the application flattens its arguments into it.
    let mut command = <RunOptions as ClapArgs>::augment_args(<A as ClapArgs>::augment_args(
        clap::Command::new("unlit3dx"),
    ));
    let mut matches = command.try_get_matches_from_mut(argv.iter())?;
    // Formatting an error needs the command it came from, for usage and
    // styles; each side reports its own failures that way.
    let options = <RunOptions as clap::FromArgMatches>::from_arg_matches_mut(&mut matches)
        .map_err(|error| error.format(&mut command))?;
    let args = <A as clap::FromArgMatches>::from_arg_matches_mut(&mut matches)
        .map_err(|error| error.format(&mut command))?;
    Ok((options, args))
}

/// Read the command line, pick a run, and drive it until it exits.
///
/// This is the whole of a binary whose scene is an [`App`]: it initializes
/// logging, parses the command line — the run's switches and the
/// application's [`App::Args`] read together by one parser, whose
/// `--help` describes both — and then:
///
/// - `--headless` captures frames and a mix without a window, writing them
///   under `--out`; with `--mcp` it serves the interactive host instead.
/// - otherwise it opens a window, stepped inline unless the platform
///   drives the worlds on threads of their own.
///
/// The windowed path is the one a browser and an Android activity take:
/// neither has a headless run.
///
/// # Panics
///
/// Panics when the platform refuses to build an event loop, which is fatal
/// for a windowed run: there is nothing to fall back to.
pub fn run<A: App>(app: A) -> std::process::ExitCode {
    logging::init();
    let argv: Vec<String> = std::env::args().collect();
    let (options, args) = match read_command::<<A as App>::Args>(&argv) {
        Ok(parsed) => parsed,
        Err(error) => {
            if error.use_stderr() {
                log::error!("the command line could not be read: {error}");
                return std::process::ExitCode::FAILURE;
            }
            // --help prints to stdout and exits successfully.
            log::info!("{}", error.render());
            return std::process::ExitCode::SUCCESS;
        }
    };
    let mut app = app;
    if let Err(message) = app.configure(args) {
        log::error!("the command line could not be applied: {message}");
        return std::process::ExitCode::FAILURE;
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    if options.headless_run() {
        return headless_main(app, &options);
    }
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    let _ = &options;
    let event_loop = EventLoop::<()>::with_user_event()
        .build()
        .expect("the platform builds an event loop");
    let window = WindowOptions {
        stepped: options.stepped || cfg!(target_arch = "wasm32"),
        mcp: options.mcp,
        ..WindowOptions::for_app::<A>()
    };
    windowed(app, window, event_loop);
    std::process::ExitCode::SUCCESS
}

/// The headless entry [`run`] takes when `--headless` was given.
///
/// The GPU is opened here rather than by the caller so the run owns the
/// whole of a capture: it picks the device tier from the environment,
/// steps the frames the command line named, and writes the pictures and
/// the mix beside each other.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn headless_main<A: App>(app: A, options: &RunOptions) -> std::process::ExitCode {
    use crate::gpu::Gpu;
    use unlit_wgpu::capabilities::DeviceTier;
    use unlit_wgpu::context::RenderDevice;

    let frames = options.frames.unwrap_or(RunOptions::DEFAULT_FRAMES);
    let out = options
        .out
        .clone()
        .unwrap_or(std::path::PathBuf::from(RunOptions::DEFAULT_OUT));
    if frames == 0 {
        log::error!("--frames must be at least 1");
        return std::process::ExitCode::FAILURE;
    }
    let gpu = match pollster::block_on(Gpu::headless(
        DeviceTier::from_env(),
        wgpu::PowerPreference::default(),
        A::TITLE,
    )) {
        Ok(gpu) => gpu,
        Err(error) => {
            log::error!("the headless run could not open a GPU: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let device = RenderDevice::from(gpu);
    #[cfg(feature = "mcp")]
    if options.mcp {
        serve(app, device);
        return std::process::ExitCode::SUCCESS;
    }
    let config = HeadlessConfig {
        size: A::SIZE,
        frames,
        capture: vec![frames - 1],
        audio_sample_rate: AUDIO_SAMPLE_RATE,
        ..HeadlessConfig::default()
    };
    let output = pollster::block_on(headless::headless(app, &config, device));
    match write::capture(&output, &config, &out) {
        Ok(written) => {
            for path in written {
                log::info!("the headless run wrote {}", path.display());
            }
            std::process::ExitCode::SUCCESS
        }
        Err(message) => {
            log::error!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}
