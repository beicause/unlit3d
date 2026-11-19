//! The rendering tools a world answers through the MCP server.
//!
//! Each tool resolves the entity handles its arguments name and routes the work
//! to the world's thread through the [`command` function](unlit_ecs::mcp::command)
//! or the [`run` function](unlit_ecs::mcp::run). The tools register themselves
//! through the [`ToolSource` struct](unlit_ecs::mcp::ToolSource), so a server built
//! from [unlit_ecs] carries them without depending on this crate.

mod host;

use host::FrameClockReport;

use unlit_ecs::mcp::serde_json::Value;
use unlit_ecs::mcp::{
    CallToolResult, CommandSender, ContextArg, ErrorData, HandleArg, InputArg, McpCommand,
    McpServer, NoArgs, Output, ReadBuffer, SaveBuffer, SaveScreenshot, SaveTexture, Screenshot,
    SendInput, SpawnGltf, SpawnMesh, ToolResult, ToolRouter, ToolSource, WorldArg, WorldRef,
    command, entity, from_value, run, to_value, tool,
};
use unlit_wgpu::resources::ResId;

/// A resource id from the u64 bits a JSON argument carries.
fn resource_id(bits: u64) -> ResId {
    ResId::from_bits(bits)
}

/// A summary of the resource graph.
async fn graph_summary(commands: CommandSender, args: ContextArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::graph_summary(world, args.context.map(entity))
    })
    .await
}

/// Every resource slot in the graph.
async fn list_resources(commands: CommandSender, args: ContextArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::list_resources(world, args.context.map(entity))
    })
    .await
}

/// The resources a resource depends on.
async fn resource_dependencies(commands: CommandSender, args: HandleArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::resource_dependencies(world, args.context.map(entity), resource_id(args.id))
    })
    .await
}

/// Drop unreferenced resources and rebuild dirty ones.
async fn graph_maintain(commands: CommandSender, args: ContextArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::graph_maintain(world, args.context.map(entity))
    })
    .await
}

/// Read bytes out of a buffer resource.
async fn read_buffer(commands: CommandSender, args: ReadBuffer) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::read_buffer(
            world,
            args.context.map(entity),
            resource_id(args.id),
            args.offset,
            args.size,
        )
    })
    .await
}

/// Read bytes out of a buffer resource and write them to a file.
async fn save_buffer(commands: CommandSender, args: SaveBuffer) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::save_buffer(
            world,
            args.context.map(entity),
            resource_id(args.id),
            args.offset,
            args.size,
            &args.directory,
            args.file_name.as_deref(),
        )
    })
    .await
}

/// Read a texture resource as an inline image.
async fn read_texture_as_image(commands: CommandSender, args: HandleArg) -> ToolResult {
    run(&commands, args.world, move |world| {
        host::read_texture_as_image(
            world,
            args.context.map(entity),
            resource_id(args.id),
            args.format.as_deref(),
        )
    })
    .await
}

/// Read a texture resource and write it to a file as an image.
async fn save_texture_as_image(commands: CommandSender, args: SaveTexture) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::save_texture_as_image(
            world,
            args.context.map(entity),
            resource_id(args.id),
            &args.directory,
            args.file_name.as_deref(),
        )
    })
    .await
}

/// The input state, encoded.
async fn input_state(commands: CommandSender, args: InputArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::input_state(world, args.entity.map(entity))
    })
    .await
}

/// Push input events into the world.
async fn send_input(commands: CommandSender, args: SendInput) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::send_input(world, args.entity.map(entity), &Value::Array(args.events))
    })
    .await
}

/// Allocate a mesh and spawn an entity that draws it.
async fn spawn_mesh(commands: CommandSender, args: SpawnMesh) -> ToolResult {
    let source = args.source.map(entity);
    command(&commands, args.world, move |world| {
        host::spawn_mesh(world, source, args)
    })
    .await
}

/// Report a world's frame ordinal, frame duration and frame rate.
async fn frame_clock(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::frame_clock(world).and_then(|report| to_value(&report))
    })
    .await
}

/// Report a frame clock for every world the host carries.
async fn frame_clocks(commands: CommandSender, _args: NoArgs) -> ToolResult {
    let worlds = request_worlds(&commands).await?;
    let mut reports = Vec::with_capacity(worlds.len());
    for entry in worlds {
        reports.push(FrameClockEntry {
            world: entry.world,
            name: entry.name,
            clock: world_frame_clock(&commands, entry.world).await?,
        });
    }
    to_value(&reports)
        .map(CallToolResult::structured)
        .map_err(|error| ErrorData::internal_error(error, None))
}

/// One world as this crate reads it back from a listing.
///
/// The listing's `WorldEntry` names its world with a `&'static str`, which is
/// what the host has rather than what a reply can carry, so the reply is read
/// into an owned name here.
#[derive(facet::Facet)]
struct ListedWorld {
    world: WorldRef,
    name: String,
}

/// Ask the host which worlds it carries.
async fn request_worlds(commands: &CommandSender) -> Result<Vec<ListedWorld>, ErrorData> {
    let (request, receiver) = McpCommand::list_worlds();
    commands
        .send(request)
        .await
        .map_err(|_| ErrorData::internal_error("the world thread is gone", None))?;
    match receiver.recv().await {
        Ok(Ok(Output::Json(value))) => from_value(&value).map_err(|error| {
            ErrorData::internal_error(format!("list_worlds answered unreadably: {error}"), None)
        }),
        Ok(Ok(_)) => Err(ErrorData::internal_error(
            "list_worlds was answered with something other than JSON",
            None,
        )),
        Ok(Err(message)) => Err(ErrorData::internal_error(message, None)),
        Err(_) => Err(ErrorData::internal_error(
            "the world thread dropped the command",
            None,
        )),
    }
}

/// Ask one world for its frame clock.
async fn world_frame_clock(
    commands: &CommandSender,
    world: WorldRef,
) -> Result<FrameClockReport, ErrorData> {
    let (command, receiver) = McpCommand::new(move |world| {
        host::frame_clock(world)
            .and_then(|report| to_value(&report))
            .map(Output::from)
    });
    commands
        .send(command.with_world(world))
        .await
        .map_err(|_| ErrorData::internal_error("the world thread is gone", None))?;
    match receiver.recv().await {
        Ok(Ok(Output::Json(value))) => from_value(&value).map_err(|error| {
            ErrorData::internal_error(format!("a frame clock answered unreadably: {error}"), None)
        }),
        Ok(Ok(_)) => Err(ErrorData::internal_error(
            "a frame clock was answered with something other than JSON",
            None,
        )),
        Ok(Err(message)) => Err(ErrorData::internal_error(message, None)),
        Err(_) => Err(ErrorData::internal_error(
            "the world thread dropped the command",
            None,
        )),
    }
}

/// One world's line in the frame-clock listing.
#[derive(facet::Facet)]
struct FrameClockEntry {
    /// The reference every other tool names this world by.
    world: WorldRef,
    /// The name the world was spawned under.
    name: String,
    /// The world's frame clock.
    clock: FrameClockReport,
}

/// Render a frame and read it back as an inline image.
async fn read_screenshot(commands: CommandSender, args: Screenshot) -> ToolResult {
    run(&commands, args.world, move |world| {
        host::read_screenshot(
            world,
            args.renderer.map(entity),
            args.target.map(resource_id),
            args.format.as_deref(),
        )
    })
    .await
}

/// Render a frame and write it to a file as an image.
async fn save_screenshot(commands: CommandSender, args: SaveScreenshot) -> ToolResult {
    command(&commands, args.world, move |world| {
        host::save_screenshot(
            world,
            args.renderer.map(entity),
            args.target.map(resource_id),
            &args.directory,
            args.file_name.as_deref(),
        )
    })
    .await
}

/// Load a glTF file and spawn its default scene.
async fn spawn_gltf(commands: CommandSender, args: SpawnGltf) -> ToolResult {
    let path = args.path;
    let scope = args.scope.map(entity);
    command(&commands, args.world, move |world| {
        host::spawn_gltf(world, args.source.map(entity), scope, &path)
    })
    .await
}

inventory::submit! {
    ToolSource { extend: register }
}

/// Register every rendering tool on `router`.
fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool(
        "graph_summary",
        "Count a world's resource graph nodes by kind, and how many are dirty or rebuildable. Pass the render context entity whose graph to inspect, or omit for the world's only context.",
        graph_summary,
    ));
    router.add_route(tool(
        "list_resources",
        "List every resource slot of a world's graph with its handle, kind, dirtiness and whether it can be rebuilt.",
        list_resources,
    ));
    router.add_route(tool(
        "resource_dependencies",
        "List the resource handles a resource depends on, given its handle.",
        resource_dependencies,
    ));
    router.add_route(tool(
        "graph_maintain",
        "Drop a world's unreferenced resources and rebuild its dirty ones, reporting the node count before and after.",
        graph_maintain,
    ));
    router.add_route(tool(
        "read_buffer",
        "Read bytes from a buffer resource as base64, given its handle, with an optional offset and size.",
        read_buffer,
    ));
    router.add_route(tool(
        "save_buffer",
        "Read bytes from a buffer resource and write them to a file in a directory, returning the written path and size. Prefer this over read_buffer, which returns the bytes base64-encoded inline.",
        save_buffer,
    ));
    router.add_route(tool(
        "read_texture_as_image",
        "Read a texture resource as an image content block, given its handle. The format argument picks lossless webp, the default, png, or jpeg. Prefer save_texture_as_image to write the image to a file instead of returning it inline.",
        read_texture_as_image,
    ));
    router.add_route(tool(
        "save_texture_as_image",
        "Read a texture resource and write it to a file in a directory, returning the written path and size. The file name's extension picks the format: .webp (lossless, the default when it has none), .png, or .jpg/.jpeg. Prefer this over read_texture_as_image, which returns the image inline.",
        save_texture_as_image,
    ));
    router.add_route(tool(
        "input_state",
        "Read a world's input state as JSON. Pass the entity carrying it, or omit for the world's only one.",
        input_state,
    ));
    router.add_route(tool(
        "send_input",
        "Push input events into a world's input state and deliver them.",
        send_input,
    ));
    router.add_route(tool(
        "spawn_mesh",
        "Allocate a mesh in a world's mesh source and spawn an entity that draws it. Pass hull_width together with the normals to spawn the inverted-hull variant, which draws the back faces spread along the normals. Free a spawned mesh by despawning its entity with despawn_entity.",
        spawn_mesh,
    ));
    router.add_route(tool(
        "frame_clock",
        "Report a world's frame ordinal, last frame duration, elapsed time, instantaneous fps, and the average fps over the last 120 frames. Pass the world whose frame pacing to inspect: every world paces its own frames, so a scene, a physics world and an audio world each answer for themselves. Fields that come from the world's own clock are null for a world that never dispatched a frame. The average is measured by a monitor spawned on the first call, and covers only the frames since.",
        frame_clock,
    ));
    router.add_route(tool(
        "frame_clocks",
        "Report the frame clock of every world the host carries, as list_worlds names them, in one call. Each entry carries the world's reference, its name, and the same clock frame_clock reports; a client comparing the pacing of a scene, its physics world and its audio world reads them side by side instead of sampling each one at a different moment.",
        frame_clocks,
    ));
    router.add_route(tool(
        "read_screenshot",
        "Render one frame of a world and read its offscreen target back as an image content block. Pass the target's resource handle, or the handle of any view of it, to choose it, or omit for the world's only readable texture; the format argument picks lossless webp, the default, png, or jpeg. Prefer save_screenshot to write the image to a file instead of returning it inline.",
        read_screenshot,
    ));
    router.add_route(tool(
        "save_screenshot",
        "Render one frame of a world, read its offscreen target back, and write it to a file in a directory, returning the written path and size. The file name's extension picks the format: .webp (lossless, the default when it has none), .png, or .jpg/.jpeg. Prefer this over read_screenshot, which returns the image inline.",
        save_screenshot,
    ));
    router.add_route(tool(
        "spawn_gltf",
        "Load a glTF file from disk into a world's mesh source and spawn its default scene, returning the spawned entity ids.",
        spawn_gltf,
    ));
}
