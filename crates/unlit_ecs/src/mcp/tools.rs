//! The ECS tools: reading and writing a world's entities and components.
//!
//! These tools need nothing but the world itself, so they live next to it and
//! register themselves with the MCP server through [ToolSource].

use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use crate::mcp::host::{Despawned, McpCommand, Output, SpawnedEntity, to_value};
use crate::mcp::server::{
    CommandSender, EntityArg, GetComponent, ListEntities, McpServer, NoArgs, SetComponent,
    SpawnEntity, ToolResult, ToolRouter, ToolSource, WorldArg, command, entity, tool,
};
use crate::reflect;
use crate::{ArchetypeBuilder, Entity, World};

/// A high-level description of the world.
pub fn summarize_world(world: &World) -> Result<Value, String> {
    to_value(&world.info())
}

/// The component names the reflection table holds.
pub fn component_names(_world: &World) -> Result<Value, String> {
    to_value(&reflect::names().collect::<Vec<_>>())
}

/// Every archetype with the component names it stores.
pub fn summarize_archetypes(world: &World) -> Result<Value, String> {
    to_value(&world.archetype_infos())
}

/// Every spawned entity with its component names.
pub fn summarize_entities(world: &World, limit: usize) -> Result<Value, String> {
    to_value(&world.entity_infos(limit))
}

/// One entity's component names and location.
pub fn summarize_entity(world: &World, entity: Entity) -> Result<Value, String> {
    let info = world
        .entity_info(entity)
        .ok_or_else(|| format!("no entity with bits {}", entity.to_bits()))?;
    to_value(&info)
}

/// One component of one entity, encoded.
pub fn read_component(world: &World, entity: Entity, name: &str) -> Result<Value, String> {
    let text = reflect::encode(world, entity, name)?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Overwrite one component of one entity.
pub fn write_component(
    world: &World,
    entity: Entity,
    name: &str,
    value: &Value,
) -> Result<Value, String> {
    let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
    reflect::set(world, entity, name, &patch)?;
    Ok(Value::Null)
}

/// Spawn an entity from a map of component name to value.
pub fn spawn_from_value(world: &mut World, components: &Value) -> Result<Value, String> {
    let object = components
        .as_object()
        .ok_or_else(|| "components must be an object".to_string())?;
    let mut builder = ArchetypeBuilder::new();
    for (name, value) in object {
        let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
        reflect::push(&mut builder, name, &patch)?;
    }
    let entity = world.spawn(builder);
    to_value(&SpawnedEntity { entity })
}

/// Despawn one entity.
pub fn despawn_one(world: &mut World, entity: Entity) -> Result<Value, String> {
    let despawned = world.despawn(entity);
    to_value(&Despawned { despawned })
}

/// Every world the host carries.
async fn list_worlds(commands: CommandSender, _args: NoArgs) -> ToolResult {
    let (request, receiver) = McpCommand::list_worlds();
    commands
        .send(request)
        .await
        .map_err(|_| ErrorData::internal_error("the world thread is gone", None))?;
    match receiver.recv().await {
        Ok(Ok(Output::Json(value))) => Ok(CallToolResult::structured(value)),
        Ok(Ok(Output::Image(_))) => Err(ErrorData::internal_error(
            "list_worlds was answered with an image",
            None,
        )),
        Ok(Err(message)) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        Err(_) => Err(ErrorData::internal_error(
            "the world thread dropped the command",
            None,
        )),
    }
}

/// A high-level description of the world.
async fn world_summary(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, move |world| summarize_world(world)).await
}

/// The component names the server can read and write.
async fn list_components(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, move |world| component_names(world)).await
}

/// Every archetype with its component names and length.
async fn list_archetypes(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        summarize_archetypes(world)
    })
    .await
}

/// Every spawned entity with its component names.
async fn list_entities(commands: CommandSender, args: ListEntities) -> ToolResult {
    command(&commands, args.world, move |world| {
        summarize_entities(world, args.limit)
    })
    .await
}

/// One entity's component names and location.
async fn get_entity(commands: CommandSender, args: EntityArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        summarize_entity(world, entity(args.entity))
    })
    .await
}

/// One component of one entity.
async fn get_component(commands: CommandSender, args: GetComponent) -> ToolResult {
    command(&commands, args.world, move |world| {
        read_component(world, entity(args.entity), &args.component)
    })
    .await
}

/// Overwrite one component of one entity.
async fn set_component(commands: CommandSender, args: SetComponent) -> ToolResult {
    command(&commands, args.world, move |world| {
        write_component(world, entity(args.entity), &args.component, &args.value)
    })
    .await
}

/// Spawn an entity from a map of component name to value.
async fn spawn_entity(commands: CommandSender, args: SpawnEntity) -> ToolResult {
    command(&commands, args.world, move |world| {
        spawn_from_value(world, &args.components)
    })
    .await
}

/// Despawn one entity.
async fn despawn_entity(commands: CommandSender, args: EntityArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        despawn_one(world, entity(args.entity))
    })
    .await
}

inventory::submit! {
    ToolSource { extend: register }
}

/// Register every ECS tool on `router`.
fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool(
        "world_summary",
        "Count a world's entities and archetypes and list the component names it has seen. Pass the host entity whose world to inspect, or omit for the host world.",
        world_summary,
    ));
    router.add_route(tool(
        "list_worlds",
        "List the worlds the host carries: its own world and every world it routes commands to, including ones on other threads. Ask this before naming a world in another tool.",
        list_worlds,
    ));
    router.add_route(tool(
        "list_components",
        "List the component names the server knows how to encode and decode.",
        list_components,
    ));
    router.add_route(tool(
        "list_archetypes",
        "List every archetype of a world with the component names it stores and how many entities it holds.",
        list_archetypes,
    ));
    router.add_route(tool(
        "list_entities",
        "List a world's spawned entities and their component names, up to a limit.",
        list_entities,
    ));
    router.add_route(tool(
        "get_entity",
        "Describe one entity of a world: its components, archetype and row.",
        get_entity,
    ));
    router.add_route(tool(
        "get_component",
        "Read one component of one entity of a world as JSON.",
        get_component,
    ));
    router.add_route(tool(
        "set_component",
        "Overwrite one component of one entity of a world from a JSON value.",
        set_component,
    ));
    router.add_route(tool(
        "spawn_entity",
        "Spawn an entity in a world from an object mapping component names to JSON values.",
        spawn_entity,
    ));
    router.add_route(tool(
        "despawn_entity",
        "Despawn one entity of a world.",
        despawn_entity,
    ));
}
