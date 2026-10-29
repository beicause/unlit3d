//! The interactive headless host: MCP drives the same scene without a window.
//!
//! The host waits for commands rather than pacing itself. Only the frame-stepping
//! tool advances simulation time, so a client can reproduce a complete sequence
//! of input, rendering and recording without sleeping between commands.
//!
//! The scene itself is the one [`crate::interactive`] builds: the host adds the
//! exit request and serves MCP over it, so a client drives exactly the scene a
//! test drives in process.

use schemars::JsonSchema;
use serde::Deserialize;
use unlit_ecs::mcp::serde_json::{self, Value, json};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolResult, ToolRouter, ToolSource, WorldArg, command, tool,
};
use unlit_ecs::{World, bundle};
use unlit3d::winit::builtin::ExitRequest;
use unlit3dx::mcp::{pump_mcp, serve_mcp};
use unlit3dx::prelude::*;

use crate::interactive::{Interactive, InteractiveConfig, scene_status};

/// Run the interactive headless host until MCP asks it to exit.
pub fn run(soundfont: std::sync::Arc<[u8]>) {
    let gpu = pollster::block_on(Gpu::headless()).expect("the headless run opens a GPU");
    let (mut host, _input, interactive) = crate::interactive::run(InteractiveConfig {
        device: gpu.device,
        queue: gpu.queue,
        capabilities: gpu.capabilities,
        soundfont,
    });
    host.root_scope_mut().spawn(bundle![ExitRequest(false)]);
    host.root_scope_mut().spawn(bundle![interactive]);
    serve_mcp(&mut host);
    log::info!("the interactive headless MCP server is ready");
    loop {
        pump_mcp(&mut host).expect("the MCP command queue applies");
        let exit = host
            .root_scope()
            .query::<&ExitRequest>()
            .any(|request| request.0);
        if exit {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
}

/// Report the scene as the last step left it, plus the ids a client reads its
/// frame and its recording through.
fn status(host: &World, interactive: &Interactive) -> Value {
    let worlds = interactive.worlds_handle();
    let scene = scene_status(host, worlds);
    let buttons = Interactive::panel_buttons(host, worlds)
        .into_iter()
        .map(|(label, rect)| (label.to_owned(), json!([rect.center().x, rect.center().y])))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "ready": true,
        "frame": interactive.frame(),
        "balls": scene.balls,
        "sequence_paused": scene.sequence.map(|state| state.0),
        "sequence_time": scene.sequence.map(|state| state.1),
        "sequence_finished": scene.sequence.map(|state| state.2),
        "buttons": buttons,
        "render_world": { "Handle": interactive.render_world.to_bits() },
        "audio_world": { "Handle": worlds.audio.entity.to_bits() },
        "target": interactive.target,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StepFrames {
    #[serde(default)]
    world: unlit_ecs::mcp::WorldId,
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
        "Report the interactive scene's frame, balls, MIDI sequence, button centres and MCP world ids.",
        scene_status_tool,
    ));
}

async fn step_frames(commands: CommandSender, args: StepFrames) -> ToolResult {
    command(&commands, args.world, move |host| {
        if args.frames == 0 {
            return Err("frames must be at least 1".to_owned());
        }
        let mut interactive = host
            .root_scope()
            .query::<&mut Interactive>()
            .next()
            .ok_or_else(|| "the host is not an interactive headless scene".to_owned())?;
        interactive.step(host, args.frames);
        // The frame is drawn as the steps leave it, so the frame a client reads
        // back afterwards is the one the steps produced. Drawing is cheap here
        // and readback does not draw, so the picture has to be brought up to
        // date before a client asks for it.
        interactive.draw(host);
        Ok(json!({ "frame": interactive.frame() }))
    })
    .await
}

async fn scene_status_tool(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |host| {
        let interactive = host
            .root_scope()
            .query::<&Interactive>()
            .next()
            .ok_or_else(|| "the host is not an interactive headless scene".to_owned())?;
        Ok(status(host, &interactive))
    })
    .await
}
