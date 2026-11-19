//! The interactive headless host: MCP drives a scene without a window.
//!
//! The host waits for commands rather than pacing itself. Only the
//! frame-stepping tool advances simulation time, so a client reproduces a
//! complete sequence of input, rendering and recording without sleeping
//! between commands.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use unlit_ecs::mcp::serde_json::{Value, json};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolResult, ToolRouter, ToolSource, WorldArg, command, tool,
};
use unlit_ecs::{World, bundle};
use unlit_wgpu::context::RenderDevice;
use unlit3d::input::InputWriter;
use unlit3d::winit::builtin::ExitRequest;

use super::App;
use super::headless::{AUDIO_SAMPLE_RATE, HEADLESS_HZ, HeadlessConfig, HeadlessOutput};
use super::interactive::{Interactive, interactive};
use super::scene::ClientPacing;
use super::write;
use crate::mcp::{pump_mcp, serve_mcp};

/// The longest a served capture records.
///
/// A served run ends when its client asks it to, so the recording cannot
/// know its length in advance; this bounds the memory a client that never
/// stops stepping can spend on the mix.
const BUDGET_SECONDS: f32 = 600.0;

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
/// The frames parameter picks who paces the run. Passing none leaves the
/// frames to the client: the run advances when the client's tools advance
/// them and ends once a client asks it to exit. Naming a count asks for a
/// run that paces itself: it steps the count at the headless rate,
/// serving MCP between the frames, and the client rides along as an
/// observer and an injector. When the output directory is named, the run also
/// writes what it drove: the frame it stopped on and the mix it played.
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
/// Fails without serving anything when `Transports` names no transport
/// while no frame count is named: a client-paced host has no driver without a
/// transport. A run that names a count paces itself and serves fine
/// without one. Fails when the HTTP transport cannot bind its port,
/// carrying the underlying message. Naming an HTTP address on a build
/// without the mcp-http feature fails the same way, and so does a capture
/// that cannot be written, after the run has ended.
pub fn serve<A: App>(
    app: A,
    device: RenderDevice,
    transports: Transports,
    frames: Option<u32>,
    out: Option<PathBuf>,
) -> Result<Option<SocketAddr>, String> {
    if frames.is_none() && !transports.stdio && transports.http.is_none() {
        return Err(
            "a client-paced host has no driver without a transport; name one with Transports, or name a frame count to pace the host itself".to_owned(),
        );
    }
    let (mut host, _input) = interactive_host(app, device);
    if transports.stdio {
        serve_mcp(&mut host);
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
    // The mix is observed from the run's first fill, so the recording opens
    // before any command runs. A client-paced run's window is the wall-clock
    // bound that keeps a client that never stops stepping from growing the
    // mix without end; a self-paced run knows its length exactly.
    if out.is_some() {
        let mut interactive = host.root_scope().query::<&mut Interactive>();
        if let Some(interactive) = interactive.iter().next() {
            let seconds = match frames {
                Some(count) => count as f32 / HEADLESS_HZ as f32,
                None => BUDGET_SECONDS,
            };
            interactive.start_recording(&host, seconds);
        }
    }
    match frames {
        Some(count) => pump_self_paced(&mut host, count),
        None => pump_until_done(&mut host),
    }
    if let Some(out) = out {
        write_capture::<A>(&mut host, &out)?;
    }
    Ok(http)
}

/// Build the interactive host the headless MCP runs serve, carrying the
/// exit request and the interactive state.
///
/// The input writer comes back with the host and has to outlive it: it is
/// the buffer the MCP tools feed.
fn interactive_host<A: App>(app: A, device: RenderDevice) -> (World, InputWriter) {
    let (mut host, input, interactive) = interactive(app, device);
    host.root_scope_mut().spawn(bundle![ExitRequest(false)]);
    host.root_scope_mut().spawn(bundle![interactive]);
    (host, input)
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

/// Pump the host's MCP commands until a client asks the run to exit.
///
/// A served run is client-paced: the frames advance when the client's tools
/// advance them, so no frame budget ends the run underneath a session.
fn pump_until_done(host: &mut World) {
    loop {
        pump_mcp(host).expect("the MCP command queue applies");
        let exit = host
            .root_scope()
            .query::<&ExitRequest>()
            .iter()
            .any(|request| request.0);
        if exit {
            break;
        }
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

/// Pump the host's MCP commands while the run steps its own frames.
///
/// The run advances one frame at a time at the headless rate and serves MCP
/// between the frames, so a client observes and injects but does not pace.
/// The count ends the run, and so does a client's exit request.
fn pump_self_paced(host: &mut World, frames: u32) {
    let step = Duration::from_secs_f64(1.0 / HEADLESS_HZ);
    let mut next = Instant::now() + step;
    for _ in 0..frames {
        {
            let mut interactive = host.root_scope().query::<&mut Interactive>();
            if let Some(interactive) = interactive.iter().next() {
                interactive.step(host, 1);
            }
        }
        pump_mcp(host).expect("the MCP command queue applies");
        let exit = host
            .root_scope()
            .query::<&ExitRequest>()
            .iter()
            .any(|request| request.0);
        if exit {
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
}

/// Write what the run drove: the frame it stopped on and the mix it played.
///
/// A served run captures its last frame rather than a schedule of them, so
/// the capture is one picture named after the frame the run ended on.
fn write_capture<A: App>(host: &mut World, out: &Path) -> Result<(), String> {
    let mut interactive = host.root_scope().query::<&mut Interactive>();
    let Some(interactive) = interactive.iter().next() else {
        return Err("the host is not an interactive headless scene".to_owned());
    };
    let index = interactive.frame();
    let pixels = pollster::block_on(interactive.render_frame(host));
    let audio = interactive
        .stop_recording(host)
        .map(|recording| recording.samples)
        .unwrap_or_default();
    let config = HeadlessConfig {
        size: A::SIZE,
        frames: index,
        capture: vec![index.saturating_sub(1)],
        audio_sample_rate: AUDIO_SAMPLE_RATE,
        ..HeadlessConfig::default()
    };
    let output = HeadlessOutput {
        frames: vec![pixels],
        audio,
    };
    let written = write::capture(&output, &config, out)?;
    for path in written {
        log::info!("the served run wrote {}", path.display());
    }
    Ok(())
}

/// Report the run as the last step left it, plus the ids a client reads its
/// frame through.
fn status(interactive: &Interactive) -> Value {
    json!({
        "ready": true,
        "frame": interactive.frame(),
        "render_world": { "Handle": interactive.render_world.to_bits() },
        "audio_world": { "Handle": interactive.worlds().audio.entity.to_bits() },
        "target": interactive.target.id().to_bits(),
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
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
        "Advance the interactive headless scene by an exact number of fixed frames.",
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
        // The headless host carries the interactive scene; a client-paced
        // windowed run carries the parked stepper instead. Either way the
        // tool advances the scene and reports where it stands.
        let mut interactive = host.root_scope().query::<&mut Interactive>();
        if let Some(interactive) = interactive.iter().next() {
            interactive.step(host, args.frames);
            // The frame is drawn as the steps leave it, so the frame a client
            // reads back afterwards is the one the steps produced. Drawing is
            // cheap here and readback does not draw, so the picture has to be
            // brought up to date before a client asks for it.
            interactive.draw(host);
            return Ok(json!({ "frame": interactive.frame() }));
        }
        drop(interactive);
        let mut parked = host.root_scope().query::<&ClientPacing>();
        let Some(state) = parked.iter().next() else {
            return Err("the host paces its own frames and takes no stepping".to_owned());
        };
        let stepper = Arc::clone(&state.0);
        drop(parked);
        let mut stepper = stepper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for _ in 0..args.frames {
            stepper.step(host, 1.0 / HEADLESS_HZ);
        }
        // The window redraws on its own and shows the scene as the steps
        // left it; there is no frame index to read here.
        Ok(json!({ "stepped": args.frames }))
    })
    .await
}

async fn scene_status(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |host: &mut World| {
        let mut interactive = host.root_scope().query::<&Interactive>();
        let Some(interactive) = interactive.iter().next() else {
            return Err(
                "the host is not an interactive headless scene; a windowed host has no frame index to report, and frame_clock answers its pacing"
                    .to_owned(),
            );
        };
        Ok(status(interactive))
    })
    .await
}
