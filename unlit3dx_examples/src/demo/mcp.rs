//! The demo's scene-specific MCP tools, registered beside the run's own.
//!
//! The library's [`unlit3dx::app::serve()`] registers `step_frames` and
//! `scene_status`, which report the run's frame and world ids. This module
//! adds `scene_extra_status`, which reports what only this scene knows: the
//! balls the gameplay world holds, the MIDI sequence's transport, and the
//! panel buttons a client clicks through.

use unlit_ecs::World;
use unlit_ecs::mcp::serde_json::{Map, json};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolError, ToolResult, ToolRouter, WorldArg, command, tool,
};
use unlit3dx::app::Interactive;

use crate::scene::{panel_buttons, scene_status};

/// Register the demo's tools on the router the server builds its menu from.
///
/// Called by the inventory submission in `demo.rs`; kept public because the
/// submission lives one module up, where the linker cannot drop it.
pub fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool(
        "scene_extra_status",
        "Report the demo scene's balls, MIDI sequence and panel button centres.",
        scene_extra_status,
    ));
}

async fn scene_extra_status(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |host: &mut World| {
        let mut interactive = host.root_scope().query::<&Interactive>();
        let Some(interactive) = interactive.iter().next() else {
            return Err(ToolError::rejected(
                "the host is not an interactive headless scene",
            ));
        };
        let worlds = interactive.worlds();
        let scene = scene_status(host, worlds);
        let buttons = panel_buttons(host, worlds)
            .into_iter()
            .map(|(label, rect)| (label.to_owned(), json!([rect.center().x, rect.center().y])))
            .collect::<Map<_, _>>();
        Ok(json!({
            "balls": scene.balls,
            "sequence_paused": scene.sequence.map(|state| state.0),
            "sequence_time": scene.sequence.map(|state| state.1),
            "sequence_finished": scene.sequence.map(|state| state.2),
            "buttons": buttons,
        }))
    })
    .await
}
