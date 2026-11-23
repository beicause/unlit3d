//! The interactive headless host: MCP drives a scene without a window.
//!
//! The host advances frames either at its client's request or on its own
//! clock. Only the frame-stepping tool advances simulation time on a
//! client-paced host, so a client reproduces a complete sequence of input,
//! rendering and recording without sleeping between commands.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use unlit_ecs::mcp::serde_json::{Value, json};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolResult, ToolRouter, ToolSource, WorldArg, command, tool,
};
use unlit_ecs::{World, bundle};
use unlit_wgpu::context::RenderDevice;
use unlit3d::winit::builtin::ExitRequest;

use super::App;
use super::capture::Capture;
use super::headless::{
    AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, FIXED_FRAME_SECONDS, UNBOUNDED_MIX_SECONDS,
};
use super::interactive::{Interactive, interactive};
use super::pace::Pacing;
use super::scene::RunFrames;
use super::window_frame::{FrameOutcome, WindowFrames};
use crate::audio::AudioFormat;
use crate::mcp::{pump_mcp, serve_mcp};
use crate::world::StepOutcome;

/// The transports a headless run serves MCP over.
///
/// A run serves any combination of them: a client can speak over the
/// process's pipes, over a TCP port, or both at once.
#[derive(Clone, Copy, Debug, Default)]
pub struct Transports {
    /// Serve the process's standard streams: the client that spawned the
    /// run speaks to it over its pipes.
    pub stdio: bool,
    /// Serve streamable HTTP on the address named, which network clients
    /// connect to. Port 0 leaves the port to the OS; [serve] returns the
    /// address bound.
    pub http: Option<SocketAddr>,
}

/// Serve an application's scene over MCP.
///
/// The host is the interactive one, so a client drives exactly the scene a
/// test drives in process. What the scene holds is the application's: the
/// tools here advance frames and report the run's state, and an
/// application registers tools of its own for the objects it owns.
///
/// [`Pacing`] picks who advances the frames: a client-paced host waits for
/// the client's tools, and a host with a frame count steps them itself at
/// the headless rate, serving MCP between the frames so the client rides
/// along as an observer and an injector. A host bounded by nothing and paced
/// by nobody runs until a client asks it to exit or the process is killed.
///
/// The capture writes the frames it names as the run reaches them, and the
/// mix it played when the run ends.
///
/// The transports named in [Transports] serve in combination; [serve]
/// returns the address the HTTP transport bound, when one was named.
///
/// The tools reach the server through an [`unlit_ecs::mcp::ToolSource`]
/// submitted inside this module. A linker drops the submissions of an
/// object file nothing references, so calling [crate::app::windowed()] with
/// `mcp_stdio` set — never this function — leaves the run's tools unregistered;
/// an application that wants them on the windowed path anchors a
/// submission of its own, as the examples crate does.
///
/// # Errors
///
/// Fails without serving anything when [Transports] names no transport and
/// the pacing leaves the frames to a client that cannot arrive: the host
/// would have nothing to drive it. Fails when the HTTP transport cannot
/// bind its port, carrying the underlying message. Naming an HTTP address on
/// a build without the mcp-http feature fails the same way, and so does a
/// capture that cannot be written.
pub fn serve<A: App>(
    app: A,
    device: RenderDevice,
    transports: Transports,
    pacing: Pacing,
    capture: Option<Capture>,
) -> Result<Option<SocketAddr>, String> {
    if pacing.client && !transports.stdio && transports.http.is_none() {
        return Err(
            "a client-paced host has no driver without a transport; name one with Transports, or let the host pace its own frames".to_owned(),
        );
    }
    let mut host = interactive_host(app, device, pacing)?;
    // The mix is observed from the run's first fill, so the recording opens
    // before any command runs. A client-paced run records a fixed span: the
    // run ends when its client says so, so the length is not the run's to
    // know, and an unbounded recording would grow with the session.
    if let Some(capture) = &capture {
        let scope = host.root_scope();
        if let Some(mut interactive) = scope
            .singleton::<Interactive>()
            .and_then(|entity| scope.try_query_one::<&mut Interactive>(entity).ok())
        {
            let interactive = interactive.get();
            let seconds = match pacing.frames {
                Some(count) => count as f32 * FIXED_FRAME_SECONDS as f32,
                // A client-paced run has no length to read here, so what
                // bounds the mix is the same cap an unbounded run uses.
                None => UNBOUNDED_MIX_SECONDS,
            };
            interactive.capture = capture.clone();
            interactive.start_recording(&host, seconds)?;
        }
    }
    if transports.stdio {
        serve_mcp(&mut host)
            .map_err(|error| format!("the MCP stdio server failed to start: {error}"))?;
    }
    let http = match transports.http {
        Some(addr) => bind_http(&mut host, addr)?,
        None => None,
    };
    match (transports.stdio, http) {
        (true, Some(bound)) => log::info!(
            "the interactive headless MCP server is ready over stdio and on http://{bound}/"
        ),
        (false, Some(bound)) => {
            log::info!("the interactive headless MCP server is ready on http://{bound}/");
        }
        (true, None) => log::info!("the interactive headless MCP server is ready"),
        (false, None) => {}
    }
    if pacing.client {
        pump_until_done(&mut host, pacing)?;
    } else {
        pump_self_paced(&mut host, pacing)?;
    }
    write_capture(&mut host, capture.as_ref())?;
    Ok(http)
}

/// Build the interactive host the headless MCP runs serve, carrying the
/// exit request and the interactive state.
fn interactive_host<A: App>(app: A, device: RenderDevice, pacing: Pacing) -> Result<World, String> {
    let (mut host, interactive) = interactive(app, device)?;
    host.root_scope_mut().spawn(bundle![ExitRequest(false)]);
    // The pacing is part of the host rather than of the loop that reads it:
    // the stepping tool answers how far the run may still go, and it reaches
    // the host through the same world the client does.
    host.root_scope_mut()
        .spawn(bundle![pacing].singleton::<Pacing>());
    // One scene per served host: a second interactive scene would be a second
    // driver of the same frames, so the key makes that a panic rather than a
    // silently ignored second scene.
    host.root_scope_mut()
        .spawn(bundle![interactive].singleton::<Interactive>());
    Ok(host)
}

/// Bind the HTTP transport against the host, when the build serves it.
///
/// A build without the mcp-http feature refuses the address rather than
/// silently serving nothing.
#[cfg(feature = "mcp-http")]
fn bind_http(host: &mut World, addr: SocketAddr) -> Result<Option<SocketAddr>, String> {
    crate::mcp::serve_http_mcp(host, addr).map(Some)
}

/// Refuse the HTTP transport: this build does not serve it.
#[cfg(not(feature = "mcp-http"))]
fn bind_http(_host: &mut World, _addr: SocketAddr) -> Result<Option<SocketAddr>, String> {
    Err("the HTTP transport needs the mcp-http feature".to_owned())
}

/// How long a client-paced turn waits for a command before looking at the
/// world again.
///
/// The transports ring the turn when they post, so this is not the polling
/// rate: it is the longest a change nobody posted about — an exit request
/// written by a behaviour — can go unnoticed. A second is short enough that
/// the run ends when told to, and long enough that waiting costs nothing.
const POLL_SECONDS: f64 = 1.0;

/// Serve the host's MCP commands until a client asks the run to exit, or
/// until the client has advanced the run as far as its budget allows.
///
/// A client-paced run advances when the client's tools advance it, so
/// nothing here steps the scene: the turn exists to answer the commands, to
/// notice the exit request, and to notice that the budget is spent. The
/// budget is checked here rather than in [step_frames] alone so that a run
/// whose steps came from anywhere reaches its last frame the same way.
fn pump_until_done(host: &mut World, pacing: Pacing) -> Result<(), String> {
    // The transports ring this handle when they post, so the turn sleeps
    // until there is something to do instead of spinning a core while the
    // client thinks. The timeout is the exit request's own alarm: a client
    // asking the run to exit posts through the same transports, so the ring
    // covers it, and the timeout only bounds how long an exit request
    // written from anywhere else can sit unseen.
    let wake = crate::mcp::inbox_wake(host);
    let poll = Duration::from_secs_f64(POLL_SECONDS);
    loop {
        pump(host)?;
        if exit_requested(host) || pacing.is_spent(advanced(host)) {
            break;
        }
        match &wake {
            Some(wake) => wake.wait(poll),
            None => std::thread::sleep(poll),
        }
    }
    Ok(())
}

/// Serve the host's MCP commands while the run steps its own frames.
///
/// The run advances one frame at a time at the headless rate and answers
/// MCP between the frames, so a client observes and injects but does not
/// pace. The count ends the run, and so does a client's exit request; a
/// host with no count runs until one of those happens.
fn pump_self_paced(host: &mut World, pacing: Pacing) -> Result<(), String> {
    let step = Duration::from_secs_f64(FIXED_FRAME_SECONDS);
    let mut next = Instant::now() + step;
    while !pacing.is_spent(advanced(host)) {
        {
            let scope = host.root_scope();
            if let Some(mut interactive) = scope
                .singleton::<Interactive>()
                .and_then(|entity| scope.try_query_one::<&mut Interactive>(entity).ok())
            {
                match interactive.get().step(host, 1)? {
                    StepOutcome::Ran => {}
                    StepOutcome::Finished | StepOutcome::Stopped => break,
                    StepOutcome::Failed => return Err("the interactive scene failed".to_owned()),
                }
            }
        }
        pump(host)?;
        if exit_requested(host) {
            break;
        }
        // The frames are paced in wall-clock time, the way a windowed run
        // is: a transport takes a moment to come up, and a run that raced
        // through its count would end before its client ever connected.
        // The scene steps at the fixed rate either way, so what a client
        // sees is the same run, later.
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next += step;
        let now = Instant::now();
        if next <= now {
            next = now + step;
        }
    }
    Ok(())
}

fn pump(host: &mut World) -> Result<(), String> {
    let failed =
        pump_mcp(host).map_err(|error| format!("the MCP command queue failed: {error}"))?;
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("world jobs failed: {}", failed.join(", ")))
    }
}

/// How many frames the host has advanced.
///
/// The count is a component of the host world, so the budget, the stepping
/// tool and the report of a capture read one number rather than each asking
/// the driver it happens to know about: a client-paced windowed host has no
/// [Interactive] to ask, and a reader that asked it would see every frame as
/// the first one.
fn advanced(host: &World) -> u32 {
    host.root_scope()
        .singleton::<RunFrames>()
        .and_then(|entity| host.root_scope().try_query_one::<&RunFrames>(entity).ok())
        .map_or(0, |mut frames| frames.get().advanced())
}

/// How many of the `asked` frames the run may still advance before its
/// budget is spent.
///
/// Clamping the step rather than refusing it is what makes a budget a length
/// of the run instead of a condition on its client: stepping past the last
/// frame advances to exactly it, and the run ends there the way a self-paced
/// one does. A run that named no budget takes every frame asked of it. The
/// answer names what the call stepped, so a clamped step reads as a shorter
/// answer rather than as a lost one.
fn allowed(host: &World, asked: u32) -> u32 {
    let scope = host.root_scope();
    let Some(mut pacing) = scope
        .singleton::<Pacing>()
        .and_then(|entity| scope.try_query_one::<&Pacing>(entity).ok())
    else {
        return asked;
    };
    let pacing = pacing.get();
    match pacing.frames {
        Some(budget) => asked.min(budget.saturating_sub(advanced(host))),
        None => asked,
    }
}

/// Whether the host was asked to exit.
fn exit_requested(host: &World) -> bool {
    host.root_scope()
        .query::<&ExitRequest>()
        .iter()
        .any(|request| request.0)
}

/// Write the mix the run played, and report a capture it never completed.
fn write_capture(host: &mut World, capture: Option<&Capture>) -> Result<(), String> {
    let Some(capture) = capture else {
        return Ok(());
    };
    let scope = host.root_scope();
    let Some(mut interactive) = scope
        .singleton::<Interactive>()
        .and_then(|entity| scope.try_query_one::<&mut Interactive>(entity).ok())
    else {
        return Err("the host is not an interactive headless scene".to_owned());
    };
    let interactive = interactive.get();
    let advanced = interactive.frame();
    let audio = interactive
        .stop_recording(host)?
        .map(|recording| recording.samples)
        .unwrap_or_default();
    let _ = interactive;
    let format = AudioFormat {
        sample_rate: AUDIO_SAMPLE_RATE,
        channels: AUDIO_CHANNELS as u16,
    };
    if let Some(path) = capture.write_audio(format, &audio)? {
        log::info!("the served run wrote {}", path.display());
    }
    if let Some(frame) = capture.missing(advanced) {
        log::warn!(
            "the run advanced {advanced} frames and never reached frame {frame}, so the capture is incomplete"
        );
    }
    Ok(())
}

/// Report the run as the last step left it, plus the ids a client reads its
/// frame through.
fn status(interactive: &Interactive) -> Value {
    json!({
        "frame": interactive.frame(),
        "render_world": { "Handle": interactive.render_world().to_bits() },
        "audio_world": { "Handle": interactive.worlds().audio.entity.to_bits() },
        "target": interactive.target.id().to_bits(),
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct StepFrames {
    #[serde(default)]
    world: unlit_ecs::mcp::WorldRef,
    /// How many fixed-length frames to advance.
    frames: u32,
}

unlit_ecs::mcp::inventory::submit! {
    ToolSource { extend: register }
}

fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool(
        "step_frames",
        "Advance the interactive scene by up to the number of fixed frames asked for, stopping at the run's last frame. Answers the frame the run stands on and how many frames this call stepped.",
        step_frames,
    ));
    router.add_route(tool(
        "scene_status",
        "Report the interactive run's frame and the MCP world ids a client reads its picture through.",
        scene_status,
    ));
}

async fn step_frames(commands: CommandSender, args: StepFrames) -> ToolResult {
    command(&commands, args.world, move |host| {
        if args.frames == 0 {
            return Err("frames must be at least 1".to_owned());
        }
        // A self-paced host owns its clock. Letting this tool step it as well
        // would advance the same scene twice and violate the pacing contract.
        let scope = host.root_scope();
        let self_paced = scope
            .singleton::<Pacing>()
            .and_then(|entity| scope.try_query_one::<&Pacing>(entity).ok())
            .is_some_and(|mut pacing| !pacing.get().client);
        if self_paced {
            return Err("the host paces its own frames and takes no stepping".to_owned());
        }
        let frames = allowed(host, args.frames);
        if let Some(mut interactive) = scope
            .singleton::<Interactive>()
            .and_then(|entity| scope.try_query_one::<&mut Interactive>(entity).ok())
        {
            let interactive = interactive.get();
            let before = interactive.frame();
            let outcome = interactive.step(host, frames)?;
            if outcome != StepOutcome::Ran {
                // The flag is per entity and the host exits when any of them
                // is set, so a refused step has to mark every request it can
                // reach: writing one would leave the other holders of the
                // flag with nothing to notice.
                for request in host.root_scope().query::<&mut ExitRequest>().iter() {
                    request.0 = true;
                }
                if outcome == StepOutcome::Failed {
                    return Err("the interactive scene failed".to_owned());
                }
            }
            // The frame is drawn as the steps leave it, so the frame a client
            // reads back afterwards is the one the steps produced: readback
            // does not draw, so the picture has to be brought up to date before
            // a client asks for it.
            interactive.draw(host)?;
            let frame = interactive.frame();
            return Ok(json!({ "frame": frame, "stepped": frame - before }));
        }
        let mut windows = host.root_scope().query::<&mut WindowFrames>();
        let Some(window) = windows.iter().next() else {
            return Err("the host has no client-paced frame driver".to_owned());
        };
        let mut stepped = 0;
        while stepped < frames {
            match window.submit(host, FIXED_FRAME_SECONDS as f32) {
                // A frame that drew, and one the surface had no image for,
                // both count as stepped: the run moved on either way, and the
                // client asked for frames of the run, not of the display.
                FrameOutcome::Presented | FrameOutcome::Skipped => stepped += 1,
                FrameOutcome::Ended => break,
                FrameOutcome::Failed(message) => return Err(message),
            }
        }
        Ok(json!({ "frame": advanced(host), "stepped": stepped }))
    })
    .await
}

async fn scene_status(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |host: &mut World| {
        let scope = host.root_scope();
        // A windowed host has the frame count but not the scene this
        // reports: its picture, its worlds and its target are the window's,
        // and a client reads those through the resource graph.
        let Some(mut interactive) = scope
            .singleton::<Interactive>()
            .and_then(|entity| scope.try_query_one::<&Interactive>(entity).ok())
        else {
            return Err(
                "the host is not an interactive headless scene; a windowed host reports its frame through step_frames and its picture through list_resources"
                    .to_owned(),
            );
        };
        Ok(status(interactive.get()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{advanced, allowed};
    use crate::app::pace::Pacing;
    use crate::app::scene::RunFrames;
    use unlit_ecs::{World, bundle};

    /// A host carrying a frame count and, when named, a budget.
    ///
    /// The pacing claims its singleton key, the way `interactive_host` spawns
    /// it, so the lookup under test finds it.
    fn host(pacing: Option<Pacing>) -> (World, RunFrames) {
        let mut host = World::new();
        let frames = RunFrames::default();
        if let Some(pacing) = pacing {
            host.root_scope_mut()
                .spawn(bundle![pacing].singleton::<Pacing>());
        }
        host.root_scope_mut()
            .spawn(bundle![frames.clone()].singleton::<RunFrames>());
        (host, frames)
    }

    #[test]
    fn the_frame_count_is_read_from_the_host_not_from_a_driver() {
        // A client-paced windowed host has no interactive scene to ask, so a
        // reader that asked one would see every frame as the first.
        let (host, frames) = host(None);
        assert_eq!(advanced(&host), 0);
        frames.advance();
        frames.advance();
        assert_eq!(advanced(&host), 2);
    }

    #[test]
    fn a_step_is_clamped_to_what_the_budget_leaves() {
        let (host, frames) = host(Some(Pacing {
            client: true,
            frames: Some(4),
        }));
        assert_eq!(
            allowed(&host, 3),
            3,
            "a budget the run has not reached takes the whole step"
        );
        for _ in 0..3 {
            frames.advance();
        }
        assert_eq!(
            allowed(&host, 3),
            1,
            "the step stops at the run's last frame"
        );
        frames.advance();
        assert_eq!(allowed(&host, 3), 0, "a spent budget steps nothing");
    }

    #[test]
    fn an_unbounded_host_takes_every_frame_it_is_asked_for() {
        let (host, _frames) = host(Some(Pacing {
            client: true,
            frames: None,
        }));
        assert_eq!(allowed(&host, 1_000), 1_000);
    }

    #[test]
    fn a_run_whose_budget_is_spent_pumps_nothing() {
        // The budget ends a client-paced run the way it ends a self-paced
        // one, so a host that has drawn all its frames answers its client and
        // stops rather than waiting for another step that cannot come.
        let (mut host, frames) = host(Some(Pacing {
            client: true,
            frames: Some(2),
        }));
        frames.advance();
        frames.advance();
        super::pump_until_done(
            &mut host,
            Pacing {
                client: true,
                frames: Some(2),
            },
        )
        .expect("the MCP turn succeeds");
    }

    #[test]
    fn a_run_told_to_exit_pumps_nothing() {
        let (mut host, _frames) = host(Some(Pacing {
            client: true,
            frames: None,
        }));
        host.root_scope_mut()
            .spawn(bundle![unlit3d::winit::builtin::ExitRequest(true)]);
        super::pump_until_done(
            &mut host,
            Pacing {
                client: true,
                frames: None,
            },
        )
        .expect("the MCP turn succeeds");
    }
    #[test]
    fn a_host_that_names_no_pacing_takes_every_frame_it_is_asked_for() {
        let (host, _frames) = host(None);
        assert_eq!(allowed(&host, 7), 7);
    }
}
