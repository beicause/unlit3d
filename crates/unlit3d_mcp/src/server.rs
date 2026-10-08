//! The MCP service.
//!
//! Each tool turns its JSON arguments into a [Command] and sends it to the
//! render thread, which owns the `World`. The tool then awaits the JSON result.
//! A tool that fails returns a tool-level error result rather than a protocol
//! error, so a model sees the message instead of a transport failure.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unlit_ecs::Entity;

use crate::host::{Command, HostContext};

/// The MCP server driving a unlit3d world.
#[derive(Debug, Clone)]
pub struct McpServer {
    commands: async_channel::Sender<Command>,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// A server that sends its commands to the render thread over commands.
    pub fn new(commands: async_channel::Sender<Command>) -> Self {
        Self {
            commands,
            tool_router: Self::tool_router(),
        }
    }

    async fn command(
        &self,
        job: impl FnOnce(&mut HostContext<'_>) -> Result<Value, String> + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        let (command, receiver) = Command::new(job);
        self.commands
            .send(command)
            .await
            .map_err(|_| ErrorData::internal_error("the render thread is gone", None))?;
        match receiver.recv().await {
            Ok(Ok(value)) => Ok(CallToolResult::structured(value)),
            Ok(Err(message)) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
            Err(_) => Err(ErrorData::internal_error(
                "the render thread dropped the command",
                None,
            )),
        }
    }
}

#[tool_router(router = tool_router)]
impl McpServer {
    /// A high-level description of the world.
    #[tool(
        name = "world_summary",
        description = "Count the world's entities and archetypes and list the known component names."
    )]
    pub async fn world_summary(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| Ok(context.world_summary())).await
    }

    /// The component names the server can read and write.
    #[tool(
        name = "list_components",
        description = "List the component names the server knows how to encode and decode."
    )]
    pub async fn list_components(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| Ok(context.list_components())).await
    }

    /// Every archetype with its component names and length.
    #[tool(
        name = "list_archetypes",
        description = "List every archetype with the component names it stores and how many entities it holds."
    )]
    pub async fn list_archetypes(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| Ok(context.list_archetypes())).await
    }

    /// Every spawned entity with its component names.
    #[tool(
        name = "list_entities",
        description = "List spawned entities and their component names, up to a limit."
    )]
    pub async fn list_entities(
        &self,
        Parameters(args): Parameters<ListEntities>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| Ok(context.list_entities(args.limit)))
            .await
    }

    /// One entity's component names and location.
    #[tool(
        name = "get_entity",
        description = "Describe one entity: its components, archetype and row."
    )]
    pub async fn get_entity(
        &self,
        Parameters(args): Parameters<EntityArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.get_entity(entity_from(args.entity)))
            .await
    }

    /// One component of one entity.
    #[tool(
        name = "get_component",
        description = "Read one component of one entity as JSON."
    )]
    pub async fn get_component(
        &self,
        Parameters(args): Parameters<GetComponent>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| {
            context.get_component(entity_from(args.entity), &args.component)
        })
        .await
    }

    /// Overwrite one component of one entity.
    #[tool(
        name = "set_component",
        description = "Overwrite one component of one entity from a JSON value."
    )]
    pub async fn set_component(
        &self,
        Parameters(args): Parameters<SetComponent>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| {
            context.set_component(entity_from(args.entity), &args.component, &args.value)
        })
        .await
    }

    /// Spawn an entity from a map of component name to value.
    #[tool(
        name = "spawn_entity",
        description = "Spawn an entity from an object mapping component names to JSON values."
    )]
    pub async fn spawn_entity(
        &self,
        Parameters(args): Parameters<SpawnEntity>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.spawn_entity(&args.components))
            .await
    }

    /// Despawn one entity.
    #[tool(name = "despawn_entity", description = "Despawn one entity.")]
    pub async fn despawn_entity(
        &self,
        Parameters(args): Parameters<EntityArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.despawn_entity(entity_from(args.entity)))
            .await
    }

    /// A summary of the resource graph.
    #[tool(
        name = "graph_summary",
        description = "Count the resource graph's nodes by kind, and how many are dirty or rebuildable."
    )]
    pub async fn graph_summary(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| context.graph_summary()).await
    }

    /// Every resource slot in the graph.
    #[tool(
        name = "list_resources",
        description = "List every resource slot with its kind, dirtiness and whether it can be rebuilt."
    )]
    pub async fn list_resources(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| context.list_resources()).await
    }

    /// The resources a slot depends on.
    #[tool(
        name = "resource_dependencies",
        description = "List the resource slot indices a resource depends on."
    )]
    pub async fn resource_dependencies(
        &self,
        Parameters(args): Parameters<IndexArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.resource_dependencies(args.index))
            .await
    }

    /// Drop unreferenced resources and rebuild dirty ones.
    #[tool(
        name = "graph_maintain",
        description = "Drop unreferenced resources and rebuild dirty ones, reporting the node count before and after."
    )]
    pub async fn graph_maintain(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| context.graph_maintain()).await
    }

    /// Read bytes out of a buffer resource.
    #[tool(
        name = "read_buffer",
        description = "Read bytes from a buffer resource as base64, with an optional offset and size."
    )]
    pub async fn read_buffer(
        &self,
        Parameters(args): Parameters<ReadBuffer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.read_buffer(args.index, args.offset, args.size))
            .await
    }

    /// Read a texture resource as a PNG.
    #[tool(
        name = "read_texture_as_image",
        description = "Read a texture resource and return it as a base64 PNG image."
    )]
    pub async fn read_texture_as_image(
        &self,
        Parameters(args): Parameters<IndexArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.read_texture_as_image(args.index))
            .await
    }

    /// The first input state.
    #[tool(
        name = "input_state",
        description = "Read the world's first InputState."
    )]
    pub async fn input_state(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| context.input_state()).await
    }

    /// Push input events into the world.
    #[tool(
        name = "send_input",
        description = "Push input events (key, mouse, text, focus) into the world and deliver them."
    )]
    pub async fn send_input(
        &self,
        Parameters(args): Parameters<SendInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.send_input(&Value::Array(args.events)))
            .await
    }

    /// Allocate a mesh and spawn an entity that draws it.
    #[tool(
        name = "create_mesh",
        description = "Allocate an unlit mesh from positions (with optional uvs, colors and indices) and spawn an entity drawing it."
    )]
    pub async fn create_mesh(
        &self,
        Parameters(args): Parameters<CreateMesh>,
    ) -> Result<CallToolResult, ErrorData> {
        let value = serde_json::to_value(&args)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        self.command(move |context| context.create_mesh(&value))
            .await
    }

    /// Remove a mesh entity and release its mesh.
    #[tool(
        name = "remove_mesh",
        description = "Release an entity's mesh and despawn the entity."
    )]
    pub async fn remove_mesh(
        &self,
        Parameters(args): Parameters<EntityArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.remove_mesh(entity_from(args.entity)))
            .await
    }

    /// Render one frame.
    #[tool(
        name = "render_frame",
        description = "Render one frame with the world's renderer."
    )]
    pub async fn render_frame(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| context.render_frame()).await
    }

    /// Render one frame and return the offscreen target as a PNG.
    #[tool(
        name = "screenshot",
        description = "Render one frame and return the host's offscreen target as a base64 PNG image."
    )]
    pub async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        self.command(|context| context.screenshot()).await
    }

    /// Load a glTF file and spawn its default scene.
    #[tool(
        name = "load_gltf",
        description = "Load a glTF file from disk and spawn its default scene, returning the spawned entity ids."
    )]
    pub async fn load_gltf(
        &self,
        Parameters(args): Parameters<LoadGltf>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(move |context| context.load_gltf(&args.path))
            .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Drives a unlit3d world through its public API: inspect and edit entities and \
             components, inspect the resource graph, read buffers and textures, send input, \
             and render or screenshot frames.",
        )
    }
}

fn entity_from(bits: u64) -> Entity {
    Entity::from_raw(bits as u32, (bits >> 32) as u32)
}

/// Arguments naming one entity.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityArg {
    /// The entity as its u64 bits.
    pub entity: u64,
}

/// Arguments for list_entities.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListEntities {
    /// The maximum number of entities to list.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    256
}

/// Arguments for get_component.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetComponent {
    /// The entity as its u64 bits.
    pub entity: u64,
    /// The component name.
    pub component: String,
}

/// Arguments for set_component.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetComponent {
    /// The entity as its u64 bits.
    pub entity: u64,
    /// The component name.
    pub component: String,
    /// The component's new JSON value.
    pub value: Value,
}

/// Arguments for spawn_entity.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnEntity {
    /// A map of component name to JSON value.
    pub components: Value,
}

/// Arguments naming a resource slot.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexArg {
    /// The resource slot index.
    pub index: usize,
}

/// Arguments for read_buffer.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadBuffer {
    /// The resource slot index.
    pub index: usize,
    /// The byte offset to start reading at.
    #[serde(default)]
    pub offset: u64,
    /// How many bytes to read; the rest of the buffer by default.
    #[serde(default)]
    pub size: Option<u64>,
}

/// Arguments for send_input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendInput {
    /// The input events to deliver.
    pub events: Vec<Value>,
}

/// Arguments for create_mesh.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateMesh {
    /// The mesh positions.
    pub positions: Vec<[f32; 3]>,
    /// Optional per-vertex texture coordinates.
    #[serde(default)]
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Optional per-vertex colors.
    #[serde(default)]
    pub colors: Option<Vec<[u8; 4]>>,
    /// Optional triangle indices.
    #[serde(default)]
    pub indices: Option<Vec<u32>>,
    /// The entity's transform; identity by default.
    #[serde(default)]
    pub transform: Option<MeshTransform>,
    /// The instance color; opaque white by default.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
    /// The alpha cutoff, when the mesh should be cut out.
    #[serde(default)]
    pub cutoff: Option<f32>,
}

/// A transform as JSON.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MeshTransform {
    /// The translation.
    #[serde(default = "zero3")]
    pub translation: [f32; 3],
    /// The rotation quaternion, xyzw.
    #[serde(default = "identity4")]
    pub rotation: [f32; 4],
    /// The scale.
    #[serde(default = "one3")]
    pub scale: [f32; 3],
}

fn zero3() -> [f32; 3] {
    [0.0; 3]
}

fn one3() -> [f32; 3] {
    [1.0; 3]
}

fn identity4() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

/// Arguments for load_gltf.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadGltf {
    /// The path to the glTF or GLB file.
    pub path: String,
}

// Keep the json import meaningful: tool descriptions and arguments use it.
const _: fn() -> Value = || json!({});
