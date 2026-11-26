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
//! - `serve` runs `interactive` on a host and answers MCP tools over the
//!   transports it is given: stdio, streamable HTTP, or both.
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

mod error;
mod frames;
mod logging;
mod offscreen;
mod scene;
mod shape;
mod stepped;
mod window_frame;

pub mod capture;
pub mod pace;

#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp")))]
pub mod serve;

pub use capture::{Capture, CaptureError, CaptureWriteError, WriteWhat};
pub use error::{AppConfigError, ConfigError, RunError};
pub use frames::RunFrames;
pub use headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, HeadlessConfig, HeadlessOutput, headless};
pub use interactive::{Interactive, Recording, interactive};
pub use logging::init as init_logging;
pub use offscreen::OffscreenError;
pub use options::RunOptions;
pub use pace::Pacing;
pub use scene::{AudioPull, Mode, RenderPorts, RenderSetup, ScenePorts, SceneWorlds, Stepping};
pub use shape::{DrivingMode, RunShape, TransportConfig, WorldOwnership};
pub use windowed::{WindowOptions, windowed};

#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp")))]
pub use serve::serve;

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
    const PHYSICS_HZ: f64 = crate::time::DEFAULT_PHYSICS_HZ;

    /// The command line arguments the application reads, alongside the
    /// switches the run owns.
    ///
    /// The run flattens this type into its own parser, so the whole
    /// command line is read together: one `--help` describes both, and an
    /// argument neither side declares is refused by name. Derive
    /// [`clap::Args`] on a structure of your own; an application without
    /// arguments uses [`NoArgs`].
    type Args: clap::Args;

    /// Receive the parsed [`App::Args`].
    ///
    /// Returning an error is what arguments that cannot be applied do —
    /// a file named on the command line that cannot be read, say: the run
    /// logs the cause and fails.
    ///
    /// # Errors
    ///
    /// Returns why the arguments cannot be applied. A reason the application
    /// alone knows is [`AppConfigError::Rejected`]; the variants for causes
    /// every application shares are there for the ones it does not have to
    /// invent.
    fn configure(&mut self, args: Self::Args) -> Result<(), AppConfigError> {
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
fn read_command<A: clap::Args>(argv: &[String]) -> Result<(RunOptions, A), clap::Error> {
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
/// - `--headless` runs without a window: with `--mcp-stdio` or `--mcp-http`
///   it serves the interactive host; without MCP it steps frames itself.
/// - otherwise it opens a window, stepped inline unless the platform
///   drives the worlds on threads of their own; `--mcp-stdio` serves the
///   scene to a client over standard input and output and `--mcp-http`
///   serves it to network clients while the window stays up.
///
/// Pacing and length are separate switches, and neither implies the other.
/// `--interactive` hands the frame clock to a connected client, so it needs
/// a transport to have any effect; every other run advances its own frames.
/// `--max-frames` ends the run — windowed or not — once that many frames have
/// been advanced, and a `--capture-frames` list bounds it in turn by the last
/// frame it names. A run with neither an end nor a client is a valid one, so
/// it is only warned about rather than refused. `--capture-out` writes the
/// named frames as they are drawn, plus the mix when the run played one.
///
/// The windowed path is the one a browser and an Android activity take:
/// neither has a headless run.
///
/// A run that cannot build its event loop exits with a failure status and a
/// logged error rather than panicking: the platform has nothing to fall back
/// to, so the exit is the whole report.
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
    if let Err(error) = app.configure(args) {
        log::error!("the command line could not be applied: {error}");
        return std::process::ExitCode::FAILURE;
    }
    // The run is put together once, here: the switches are read into one
    // shape and every entry below consumes that shape rather than deriving
    // its own combination. A switch this build cannot honour is refused by
    // the parse, so nothing downstream has to check a feature gate again.
    let shape = match RunShape::parse(&options) {
        Ok(shape) => shape,
        Err(error) => {
            log::error!("{error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    // A client-paced run has no driver without a transport. The run still
    // starts — the scene is a valid one to look at — but the switch changes
    // nothing, so say so before the run blocks on a frame nothing advances.
    if options.interactive && !shape.transports.any() {
        warn_reaching_no_client();
    }
    // With no transport and no length, nothing outside the process can end
    // it: no client asks it to stop and no budget runs out. That is a
    // supported way to run a scene for as long as someone watches it, so it
    // is announced rather than refused.
    if !shape.transports.any() && shape.frames.is_none() {
        warn_reaching_no_end();
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    if options.headless {
        return headless_main(app, &shape);
    }
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    let _ = &shape;
    let event_loop = match EventLoop::<()>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            log::error!("the event loop could not be created: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let window = WindowOptions {
        shape,
        ..WindowOptions::for_app::<A>()
    };
    match windowed(app, window, event_loop) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            log::error!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Warn that the switches pacing a run reach no client.
///
/// The two warnings name the switches and the feature that would make them
/// take effect, so the switch is findable from the message.
fn warn_reaching_no_client() {
    log::warn!(
        "--interactive needs --mcp-stdio or --mcp-http: without a transport nothing reaches the client, so the run paces its own frames instead"
    );
}

/// Warn that a run has neither a client to end it nor a length of its own.
///
/// Such a run is a supported one — a scene on a display, watched from
/// outside — so the message says how to end it rather than refusing it.
fn warn_reaching_no_end() {
    log::warn!(
        "the run has no --max-frames and no transport, so nothing ends it but the window closing or the process being killed (Ctrl-C)"
    );
}

/// The headless entry [`run`] takes when a headless run is named.
///
/// The GPU is opened here rather than by the caller so the run owns the
/// whole of a capture: it picks the device tier from the environment, and
/// steps the frames the command line named. A client-paced run leaves
/// those frames to its client instead, and a capture writes the frames it
/// names as it reaches them.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn headless_main<A: App>(app: A, shape: &RunShape) -> std::process::ExitCode {
    use crate::gpu::Gpu;
    use unlit_wgpu::capabilities::DeviceTier;
    use unlit_wgpu::context::RenderDevice;

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
    if shape.transports.any() {
        // A served run shares the shape a windowed one reads: the capture
        // names the frames it keeps, and the driving mode says who advances
        // them.
        match serve(app, device, shape) {
            Ok(_) => return std::process::ExitCode::SUCCESS,
            Err(error) => {
                log::error!("the MCP server did not start: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let config = HeadlessConfig {
        size: A::SIZE,
        shape: RunShape {
            ownership: WorldOwnership::Hosted,
            ..shape.clone()
        },
        audio_sample_rate: AUDIO_SAMPLE_RATE,
        ..HeadlessConfig::default()
    };
    let output = pollster::block_on(headless::headless(app, &config, device));
    match output.and_then(|output| write_mix(&config, &output)) {
        Ok(written) => {
            for path in written {
                log::info!("the headless run wrote {}", path.display());
            }
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            log::error!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Write the mix a run played, and report any frame the capture never
/// reached.
///
/// The frames are written as the run reaches them, so what is left is the
/// mix. A capture that ended short — its frames ran out, or its client asked
/// it to exit first — names the frame it never got to, rather than passing
/// an incomplete capture off as a finished one.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn write_mix(
    config: &HeadlessConfig,
    output: &HeadlessOutput,
) -> Result<Vec<std::path::PathBuf>, RunError> {
    let format = crate::audio::AudioFormat {
        sample_rate: config.audio_sample_rate,
        channels: AUDIO_CHANNELS as u16,
    };
    let capture = config.shape.capture.clone().unwrap_or_default();
    let mut written = Vec::new();
    if let Some(path) = capture.write_audio(format, &output.audio)? {
        written.push(path);
    }
    if let Some(frame) = capture.missing(output.advanced) {
        log::warn!(
            "the run advanced {} frames and never reached frame {frame}, so the capture is incomplete",
            output.advanced
        );
    }
    Ok(written)
}
