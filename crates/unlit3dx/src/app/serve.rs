//! The interactive headless host: MCP drives a scene without a window.
//!
//! The host waits for commands rather than pacing itself. Only the
//! frame-stepping tool advances simulation time, so a client reproduces a
//! complete sequence of input, rendering and recording without sleeping
//! between commands.

use std::time::Duration;

use unlit_ecs::mcp::serde_json::{Value, json};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolResult, ToolRouter, ToolSource, WorldArg, command, tool,
};
use unlit_ecs::{World, bundle};
use unlit_wgpu::context::RenderDevice;
use unlit3d::winit::builtin::ExitRequest;

use super::App;
use super::interactive::{Interactive, interactive};
use crate::mcp::{pump_mcp, serve_mcp};

/// Serve an application's scene over MCP until a client asks it to exit.
///
/// The host is the interactive one, so a client drives exactly the scene a
/// test drives in process. What the scene holds is the application's: the
/// tools here advance frames and report the run's state, and an
/// application registers tools of its own for the objects it owns.
///
/// The tools reach the server through an [`unlit_ecs::mcp::ToolSource`]
/// submitted inside this module. A linker drops the submissions of an
/// object file nothing references, so calling [crate::app::windowed()] with
/// `mcp` set — never this function — leaves the run's tools unregistered;
/// an application that wants them on the windowed path anchors a
/// submission of its own, as the examples crate does.
pub fn serve<A: App>(app: A, device: RenderDevice) {
    let (mut host, _input, interactive) = interactive(app, device);
    host.root_scope_mut().spawn(bundle![ExitRequest(false)]);
    host.root_scope_mut().spawn(bundle![interactive]);
    serve_mcp(&mut host);
    log::info!("the interactive headless MCP server is ready");
    loop {
        pump_mcp(&mut host).expect("the MCP command queue applies");
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
        let mut interactive = host.root_scope().query::<&mut Interactive>();
        let Some(interactive) = interactive.iter().next() else {
            return Err("the host is not an interactive headless scene".to_owned());
        };
        interactive.step(host, args.frames);
        // The frame is drawn as the steps leave it, so the frame a client
        // reads back afterwards is the one the steps produced. Drawing is
        // cheap here and readback does not draw, so the picture has to be
        // brought up to date before a client asks for it.
        interactive.draw(host);
        Ok(json!({ "frame": interactive.frame() }))
    })
    .await
}

async fn scene_status(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |host: &mut World| {
        let mut interactive = host.root_scope().query::<&Interactive>();
        let Some(interactive) = interactive.iter().next() else {
            return Err("the host is not an interactive headless scene".to_owned());
        };
        Ok(status(interactive))
    })
    .await
}
