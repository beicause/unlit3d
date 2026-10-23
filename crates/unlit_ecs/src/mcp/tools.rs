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
use crate::{ArchetypeBuilder, Entity, Scope, ScopeMut, World};

/// A high-level description of a scope.
fn summarize_world(scope: &Scope<'_>) -> Result<Value, String> {
    to_value(&scope.info())
}

/// The component names the reflection table holds.
fn component_names(_world: &World) -> Result<Value, String> {
    to_value(&reflect::names().collect::<Vec<_>>())
}

/// Every archetype with the component names it stores.
fn summarize_archetypes(scope: &Scope<'_>) -> Result<Value, String> {
    to_value(&scope.archetype_infos())
}

/// Every spawned entity with its component names.
fn summarize_entities(scope: &Scope<'_>, limit: usize) -> Result<Value, String> {
    to_value(&scope.entity_infos(limit))
}

/// One entity's component names and location.
fn summarize_entity(scope: &Scope<'_>, entity: Entity) -> Result<Value, String> {
    let info = scope
        .entity_info(entity)
        .map_err(|_| format!("no entity {entity:?}"))?;
    to_value(&info)
}

/// One component of one entity, encoded.
fn read_component(scope: &Scope<'_>, entity: Entity, name: &str) -> Result<Value, String> {
    if !scope.contains(entity) {
        return Err(format!("no entity {entity:?}"));
    }
    let text = reflect::encode(scope, entity, name)?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Overwrite one component of one entity.
fn write_component(
    scope: &Scope<'_>,
    entity: Entity,
    name: &str,
    value: &Value,
) -> Result<Value, String> {
    if !scope.contains(entity) {
        return Err(format!("no entity {entity:?}"));
    }
    let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
    reflect::set(scope, entity, name, &patch)?;
    Ok(Value::Null)
}

/// Spawn an entity from a map of component name to value.
fn spawn_from_value(scope: &mut ScopeMut<'_>, components: &Value) -> Result<Value, String> {
    let object = components
        .as_object()
        .ok_or_else(|| "components must be an object".to_string())?;
    let mut builder = ArchetypeBuilder::new();
    for (name, value) in object {
        let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
        reflect::push(&mut builder, name, &patch)?;
    }
    let spawned = scope.entity();
    let name = "mcp spawned";
    let entity = scope.spawn_named(name, builder);
    to_value(&SpawnedEntity {
        entity,
        name,
        scope: spawned,
    })
}

/// Despawn one entity and its subtree.
///
/// An entity the scope cannot remove is an error the caller reads, not a
/// `false` beside a name: what to do about it depends on why.
fn despawn_one(scope: &mut ScopeMut<'_>, entity: Entity) -> Result<Value, String> {
    let name = scope.as_scope().entity_name(entity).unwrap_or("");
    scope
        .despawn(entity)
        .map_err(|error| format!("cannot despawn {entity:?}: {error}"))?;
    to_value(&Despawned {
        despawned: true,
        name,
    })
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
    command(&commands, args.world, move |world| {
        summarize_world(&scoped(world, args.scope)?)
    })
    .await
}

/// The component names the server can read and write.
async fn list_components(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, move |world| component_names(world)).await
}

/// Every archetype with its component names and length.
async fn list_archetypes(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        summarize_archetypes(&scoped(world, args.scope)?)
    })
    .await
}

/// Every spawned entity with its component names.
async fn list_entities(commands: CommandSender, args: ListEntities) -> ToolResult {
    command(&commands, args.world, move |world| {
        summarize_entities(&scoped(world, args.scope)?, args.limit)
    })
    .await
}

/// One entity's component names and location.
async fn get_entity(commands: CommandSender, args: EntityArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        summarize_entity(&scoped(world, args.scope)?, entity(args.entity))
    })
    .await
}

/// One component of one entity.
async fn get_component(commands: CommandSender, args: GetComponent) -> ToolResult {
    command(&commands, args.world, move |world| {
        read_component(
            &scoped(world, args.scope)?,
            entity(args.entity),
            &args.component,
        )
    })
    .await
}

/// Overwrite one component of one entity.
async fn set_component(commands: CommandSender, args: SetComponent) -> ToolResult {
    command(&commands, args.world, move |world| {
        write_component(
            &scoped(world, args.scope)?,
            entity(args.entity),
            &args.component,
            &args.value,
        )
    })
    .await
}

/// Spawn an entity from a map of component name to value.
async fn spawn_entity(commands: CommandSender, args: SpawnEntity) -> ToolResult {
    command(&commands, args.world, move |world| {
        let mut scope = scoped_mut(world, args.scope)?;
        spawn_from_value(&mut scope, &args.components)
    })
    .await
}

/// Despawn one entity.
async fn despawn_entity(commands: CommandSender, args: EntityArg) -> ToolResult {
    command(&commands, args.world, move |world| {
        let mut scope = scoped_mut(world, args.scope)?;
        despawn_one(&mut scope, entity(args.entity))
    })
    .await
}

/// The scope an ECS tool runs in: the scope the arguments name, or the root.
fn scoped(world: &World, scope: Option<u64>) -> Result<Scope<'_>, String> {
    match scope {
        None => Ok(world.root_scope()),
        Some(bits) => {
            let scope = Entity::from_bits(bits);
            world
                .scope(scope)
                .map_err(|_| format!("no scope {scope:?}"))
        }
    }
}

/// The mutable scope an ECS tool runs in.
fn scoped_mut(world: &mut World, scope: Option<u64>) -> Result<ScopeMut<'_>, String> {
    match scope {
        None => Ok(world.root_scope_mut()),
        Some(bits) => {
            let scope = Entity::from_bits(bits);

            world
                .scope_mut(scope)
                .map_err(|_| format!("no scope {scope:?}"))
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle;

    /// A component the tests read and write through the tools.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, facet::Facet)]
    struct Marker(u32);

    inventory::submit! { crate::reflect::ComponentEntry::default::<Marker>() }

    /// A world with a scene holding one child, and a sibling scene of its own.
    fn fixture() -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        let scene = world.root_scope_mut().spawn(bundle![0u32]);
        let child = world.scope_mut(scene).unwrap().spawn(bundle![Marker(1)]);
        let sibling = world.root_scope_mut().spawn(bundle![Marker(2)]);
        (world, scene, child, sibling)
    }

    #[test]
    fn an_absent_scope_means_the_root() {
        let (world, _, child, sibling) = fixture();
        let Ok(scope) = scoped(&world, None) else {
            panic!("the root needs no handle");
        };
        assert_eq!(scope.entity(), world.root());
        assert!(scope.contains(child));
        assert!(scope.contains(sibling));
    }

    #[test]
    fn a_scope_handle_narrows_the_view() {
        let (world, scene, child, sibling) = fixture();
        let Ok(scope) = scoped(&world, Some(scene.to_bits())) else {
            panic!("a live scope");
        };
        assert_eq!(scope.entity(), scene);
        assert!(scope.contains(child));
        assert!(!scope.contains(sibling), "the sibling is beside it");
    }

    #[test]
    fn a_dead_scope_handle_is_reported_by_its_bits() {
        let (mut world, scene, _, _) = fixture();
        let bits = scene.to_bits();
        let restored = Entity::from_bits(bits);
        world.root_scope_mut().despawn(scene).unwrap();
        let error = scoped(&world, Some(bits)).expect_err("the scope is gone");
        assert_eq!(
            error,
            format!("no scope {restored:?}"),
            "the bits it came as"
        );
    }

    #[test]
    fn an_unknown_scope_reads_the_same_as_a_dead_one() {
        // A handle that never named anything must not be told apart from one
        // that used to: the difference is a probing oracle.
        let world = World::new();
        let bits = 0x1234_5678_9abc_def0u64;
        let restored = Entity::from_bits(bits);
        let error = scoped(&world, Some(bits)).expect_err("no such scope");
        assert_eq!(
            error,
            format!("no scope {restored:?}"),
            "the bits it came as"
        );
    }

    #[test]
    fn a_scoped_write_refuses_an_entity_the_scope_cannot_see() {
        let (world, scene, _, sibling) = fixture();
        let scope = scoped(&world, Some(scene.to_bits())).unwrap();
        let error = write_component(&scope, sibling, "Marker", &serde_json::json!({ "0": 9 }))
            .expect_err("the sibling is out of reach");
        assert!(error.contains(&sibling.to_bits().to_string()), "{error}");
        assert_eq!(
            world.root_scope().query_one::<&Marker>(sibling).unwrap().0,
            2
        );
    }

    #[test]
    fn a_scoped_read_refuses_an_entity_the_scope_cannot_see() {
        let (world, scene, _, sibling) = fixture();
        let scope = scoped(&world, Some(scene.to_bits())).unwrap();
        let error = read_component(&scope, sibling, "Marker").expect_err("out of reach");
        assert!(error.contains(&sibling.to_bits().to_string()), "{error}");
    }

    #[test]
    fn a_scoped_spawn_lands_in_that_scope_and_reports_it() {
        let (mut world, scene, _, _) = fixture();
        let mut scope = scoped_mut(&mut world, Some(scene.to_bits())).unwrap();
        let spawned = spawn_from_value(&mut scope, &serde_json::json!({ "Marker": { "0": 7 } }))
            .expect("a marker is spawnable");
        let entity = spawned["entity"].clone();
        let reported = spawned["scope"].clone();
        let entity = Entity::from_bits(entity.as_u64().expect("bits"));
        assert_eq!(world.parent_scope(entity).unwrap(), scene);
        assert_eq!(
            Entity::from_bits(reported.as_u64().expect("bits")),
            scene,
            "the reply says which scope it went in"
        );
    }

    #[test]
    fn a_scoped_despawn_refuses_what_the_scope_does_not_own() {
        let (mut world, scene, _, sibling) = fixture();
        let mut scope = scoped_mut(&mut world, Some(scene.to_bits())).unwrap();
        let answer = despawn_one(&mut scope, sibling).expect("refusing is not an error");
        assert_eq!(answer["despawned"], serde_json::json!(false));
        assert_eq!(
            answer["name"],
            serde_json::json!(""),
            "an entity the scope cannot see has no name to report"
        );
        assert!(world.root_scope().contains(sibling));
    }

    #[test]
    fn a_scoped_despawn_takes_the_subtree() {
        let (mut world, scene, child, _) = fixture();
        let name = world.root_scope().entity_name(child).unwrap();
        assert!(!name.is_empty(), "the row names the entity");
        let mut scope = scoped_mut(&mut world, Some(scene.to_bits())).unwrap();
        let answer = despawn_one(&mut scope, child).expect("its own descendant");
        assert_eq!(answer["despawned"], serde_json::json!(true));
        assert_eq!(
            answer["name"],
            serde_json::json!(name),
            "the reply carries the name the row stores"
        );
        assert!(!world.root_scope().contains(child));
    }

    #[test]
    fn the_summaries_report_the_scope_not_the_whole_world() {
        let (world, scene, child, sibling) = fixture();
        let scope = scoped(&world, Some(scene.to_bits())).unwrap();
        let world_info = summarize_world(&scope).unwrap();
        assert_eq!(
            world_info["entities"],
            serde_json::json!(2),
            "the scene and its child"
        );
        assert_eq!(
            world_info["root"],
            serde_json::json!(world.root().to_bits())
        );
        let entities = summarize_entities(&scope, usize::MAX).unwrap();
        let listed: Vec<u64> = entities
            .as_array()
            .unwrap()
            .iter()
            .map(|info| info["entity"].as_u64().unwrap())
            .collect();
        assert!(listed.contains(&child.to_bits()));
        assert!(!listed.contains(&sibling.to_bits()));
        let archetypes = summarize_archetypes(&scope).unwrap();
        let sum: u64 = archetypes
            .as_array()
            .unwrap()
            .iter()
            .map(|info| info["len"].as_u64().unwrap())
            .sum();
        assert_eq!(sum, 2, "the archetype lengths add up to the count");
    }

    #[test]
    fn a_scope_is_listed_like_any_other_component() {
        let (world, _, child, _) = fixture();
        let scope = scoped(&world, None).unwrap();
        let component = summarize_entity(&scope, child).unwrap();
        let names: Vec<&str> = component["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert!(
            names.iter().any(|name| name.ends_with("ScopeOf")),
            "{names:?}"
        );
    }

    #[test]
    fn a_scope_component_cannot_be_written_through_the_tools() {
        // The world writes the scope; a client that changed it would move the
        // entity without moving its index.
        let (world, scene, child, _) = fixture();
        let scope = scoped(&world, None).unwrap();
        let error = write_component(
            &scope,
            child,
            "ScopeOf",
            &serde_json::json!({ "0": scene.to_bits() }),
        )
        .expect_err("the scope is not writable");
        assert!(error.contains("written by the world"), "{error}");
    }

    #[test]
    fn a_lifecycle_component_is_not_spawnable_through_the_tools() {
        let (mut world, _, _, _) = fixture();
        let mut scope = scoped_mut(&mut world, None).unwrap();
        let error = spawn_from_value(&mut scope, &serde_json::json!({ "OnDespawn": {} }))
            .expect_err("a callback has no wire form");
        assert!(!error.is_empty());
    }
}
