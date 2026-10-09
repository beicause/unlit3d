//! The MCP service.
//!
//! Each tool turns its JSON arguments into a [Command] and sends it to the
//! thread that owns the world. The tool then awaits the JSON result. A tool
//! that fails returns a tool-level error result rather than a protocol error,
//! so a model sees the message instead of a transport failure.
//!
//! Every tool names the world it should run against: `world` is the bits of a
//! host entity carrying a [World](unlit_ecs::World) component, or `None` for
//! the world the transport thread itself holds. GPU tools additionally name the
//! entity handles they need — a render context, a renderer, a mesh source, a
//! resource — and the server passes them through untouched; the render thread
//! resolves them and reports a readable error when one does not fit.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use unlit_ecs::Entity;
use unlit_wgpu::resources::ResId;

use crate::host::{
    Command, create_mesh, despawn_entity, get_component, get_entity, graph_maintain, graph_summary,
    input_state, list_archetypes, list_components, list_entities, list_resources, load_gltf,
    read_buffer, read_texture_as_image, remove_mesh, render_frame, resource_dependencies,
    screenshot, send_input, set_component, spawn_entity, world_summary,
};

/// An entity handle from the `u64` bits a JSON argument carries.
fn entity(bits: u64) -> Entity {
    Entity::from_bits(bits)
}

/// A resource id from the `u64` bits a JSON argument carries.
fn resource_id(bits: u64) -> ResId {
    ResId::from_bits(bits)
}

/// The MCP server driving one or more unlit3d worlds.
#[derive(Debug, Clone)]
pub struct McpServer {
    commands: async_channel::Sender<Command>,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// A server that sends its commands over `commands`.
    pub fn new(commands: async_channel::Sender<Command>) -> Self {
        Self {
            commands,
            tool_router: Self::tool_router(),
        }
    }

    /// Send a job to the world `world` names and await its JSON result.
    async fn command(
        &self,
        world: Option<Entity>,
        job: impl FnOnce(&mut unlit_ecs::World) -> Result<Value, String> + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        let (mut command, receiver) = Command::new(job);
        if let Some(entity) = world {
            command = command.world(entity);
        }
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
        description = "Count a world's entities and archetypes and list the component names it has seen. Pass the host entity whose world to inspect, or omit for the host world."
    )]
    pub async fn world_summary(
        &self,
        Parameters(args): Parameters<WorldArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| world_summary(world))
            .await
    }

    /// The component names the server can read and write.
    #[tool(
        name = "list_components",
        description = "List the component names the server knows how to encode and decode."
    )]
    pub async fn list_components(
        &self,
        Parameters(args): Parameters<WorldArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| list_components(world))
            .await
    }

    /// Every archetype with its component names and length.
    #[tool(
        name = "list_archetypes",
        description = "List every archetype of a world with the component names it stores and how many entities it holds."
    )]
    pub async fn list_archetypes(
        &self,
        Parameters(args): Parameters<WorldArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| list_archetypes(world))
            .await
    }

    /// Every spawned entity with its component names.
    #[tool(
        name = "list_entities",
        description = "List a world's spawned entities and their component names, up to a limit."
    )]
    pub async fn list_entities(
        &self,
        Parameters(args): Parameters<ListEntities>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            list_entities(world, args.limit)
        })
        .await
    }

    /// One entity's component names and location.
    #[tool(
        name = "get_entity",
        description = "Describe one entity of a world: its components, archetype and row."
    )]
    pub async fn get_entity(
        &self,
        Parameters(args): Parameters<EntityArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            get_entity(world, entity(args.entity))
        })
        .await
    }

    /// One component of one entity.
    #[tool(
        name = "get_component",
        description = "Read one component of one entity of a world as JSON."
    )]
    pub async fn get_component(
        &self,
        Parameters(args): Parameters<GetComponent>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            get_component(world, entity(args.entity), &args.component)
        })
        .await
    }

    /// Overwrite one component of one entity.
    #[tool(
        name = "set_component",
        description = "Overwrite one component of one entity of a world from a JSON value."
    )]
    pub async fn set_component(
        &self,
        Parameters(args): Parameters<SetComponent>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            set_component(world, entity(args.entity), &args.component, &args.value)
        })
        .await
    }

    /// Spawn an entity from a map of component name to value.
    #[tool(
        name = "spawn_entity",
        description = "Spawn an entity in a world from an object mapping component names to JSON values."
    )]
    pub async fn spawn_entity(
        &self,
        Parameters(args): Parameters<SpawnEntity>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            spawn_entity(world, &args.components)
        })
        .await
    }

    /// Despawn one entity.
    #[tool(
        name = "despawn_entity",
        description = "Despawn one entity of a world."
    )]
    pub async fn despawn_entity(
        &self,
        Parameters(args): Parameters<EntityArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            despawn_entity(world, entity(args.entity))
        })
        .await
    }

    /// A summary of the resource graph.
    #[tool(
        name = "graph_summary",
        description = "Count a world's resource graph nodes by kind, and how many are dirty or rebuildable. Pass the render context entity whose graph to inspect, or omit for the world's only context."
    )]
    pub async fn graph_summary(
        &self,
        Parameters(args): Parameters<ContextArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            graph_summary(world, args.context.map(entity))
        })
        .await
    }

    /// Every resource slot in the graph.
    #[tool(
        name = "list_resources",
        description = "List every resource slot of a world's graph with its handle, kind, dirtiness and whether it can be rebuilt."
    )]
    pub async fn list_resources(
        &self,
        Parameters(args): Parameters<ContextArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            list_resources(world, args.context.map(entity))
        })
        .await
    }

    /// The resources a resource depends on.
    #[tool(
        name = "resource_dependencies",
        description = "List the resource handles a resource depends on, given its handle."
    )]
    pub async fn resource_dependencies(
        &self,
        Parameters(args): Parameters<HandleArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            resource_dependencies(world, args.context.map(entity), resource_id(args.id))
        })
        .await
    }

    /// Drop unreferenced resources and rebuild dirty ones.
    #[tool(
        name = "graph_maintain",
        description = "Drop a world's unreferenced resources and rebuild its dirty ones, reporting the node count before and after."
    )]
    pub async fn graph_maintain(
        &self,
        Parameters(args): Parameters<ContextArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            graph_maintain(world, args.context.map(entity))
        })
        .await
    }

    /// Read bytes out of a buffer resource.
    #[tool(
        name = "read_buffer",
        description = "Read bytes from a buffer resource as base64, given its handle, with an optional offset and size."
    )]
    pub async fn read_buffer(
        &self,
        Parameters(args): Parameters<ReadBuffer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            read_buffer(
                world,
                args.context.map(entity),
                resource_id(args.id),
                args.offset,
                args.size,
            )
        })
        .await
    }

    /// Read a texture resource as a PNG.
    #[tool(
        name = "read_texture_as_image",
        description = "Read a texture resource as a PNG image, given its handle."
    )]
    pub async fn read_texture_as_image(
        &self,
        Parameters(args): Parameters<HandleArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            read_texture_as_image(world, args.context.map(entity), resource_id(args.id))
        })
        .await
    }

    /// The input state, encoded.
    #[tool(
        name = "input_state",
        description = "Read a world's input state as JSON. Pass the entity carrying it, or omit for the world's only one."
    )]
    pub async fn input_state(
        &self,
        Parameters(args): Parameters<InputArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            input_state(world, args.entity.map(entity))
        })
        .await
    }

    /// Push input events into the world.
    #[tool(
        name = "send_input",
        description = "Push input events into a world's input state and deliver them."
    )]
    pub async fn send_input(
        &self,
        Parameters(args): Parameters<SendInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            send_input(world, args.entity.map(entity), &Value::Array(args.events))
        })
        .await
    }

    /// Allocate a mesh and spawn an entity that draws it.
    #[tool(
        name = "create_mesh",
        description = "Allocate a mesh in a world's mesh source and spawn an entity that draws it."
    )]
    pub async fn create_mesh(
        &self,
        Parameters(args): Parameters<CreateMesh>,
    ) -> Result<CallToolResult, ErrorData> {
        let source = args.source.map(entity);
        self.command(args.world.map(entity), move |world| {
            create_mesh(world, source, args)
        })
        .await
    }

    /// Release a mesh and despawn the entity that drew it.
    #[tool(
        name = "remove_mesh",
        description = "Release the mesh an entity draws and despawn the entity."
    )]
    pub async fn remove_mesh(
        &self,
        Parameters(args): Parameters<EntityArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            remove_mesh(world, args.source.map(entity), entity(args.entity))
        })
        .await
    }

    /// Render one frame from the world.
    #[tool(
        name = "render_frame",
        description = "Render one frame of a world through its renderer. Pass the renderer entity, or omit for the world's only one."
    )]
    pub async fn render_frame(
        &self,
        Parameters(args): Parameters<RendererArg>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            render_frame(world, args.renderer.map(entity))
        })
        .await
    }

    /// Render a frame and read it back as a PNG.
    #[tool(
        name = "screenshot",
        description = "Render one frame of a world and read its offscreen target back as a PNG. Pass the target's resource handle to choose it, or omit for the world's only readable texture."
    )]
    pub async fn screenshot(
        &self,
        Parameters(args): Parameters<Screenshot>,
    ) -> Result<CallToolResult, ErrorData> {
        self.command(args.world.map(entity), move |world| {
            screenshot(
                world,
                args.renderer.map(entity),
                args.target.map(resource_id),
            )
        })
        .await
    }

    /// Load a glTF file and spawn its default scene.
    #[tool(
        name = "load_gltf",
        description = "Load a glTF file from disk into a world's mesh source and spawn its default scene, returning the spawned entity ids."
    )]
    pub async fn load_gltf(
        &self,
        Parameters(args): Parameters<LoadGltf>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = args.path;
        self.command(args.world.map(entity), move |world| {
            load_gltf(world, args.source.map(entity), &path)
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Drives one or more unlit3d worlds through the public API: inspect and edit entities \
             and components, inspect a world's resource graph, read buffers and textures, send \
             input, and render or screenshot frames. Every tool names its world by host entity \
             bits (omit it for the host world), and GPU tools name the entity handles they need.",
        )
    }
}

/// Which world a tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorldArg {
    /// The host entity carrying the world to drive, as its `u64` bits; omit for the
    /// host world the transport thread itself holds.
    #[serde(default)]
    pub world: Option<u64>,
}

/// Which world and render context a GPU tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextArg {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// Any entity carrying the world's render context, as its `u64` bits —
    /// its device, queue, graph or capabilities. Omit when the world holds
    /// exactly one.
    #[serde(default)]
    pub context: Option<u64>,
}

/// Which world and resource a handle tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HandleArg {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The resource's id, as `list_resources` reports it.
    pub id: u64,
}

/// Arguments for one entity in a world.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityArg {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity's bits.
    pub entity: u64,
    /// The entity carrying the world's mesh source, as its `u64` bits, for the tools
    /// that need one. Omit for the world's only mesh source.
    #[serde(default)]
    pub source: Option<u64>,
}

/// Arguments for listing entities.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListEntities {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// How many entities to report.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    256
}

/// Arguments for reading one component.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetComponent {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity's bits.
    pub entity: u64,
    /// The component's name.
    pub component: String,
}

/// Arguments for writing one component.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetComponent {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity's bits.
    pub entity: u64,
    /// The component's name.
    pub component: String,
    /// The new value.
    pub value: Value,
}

/// Arguments for spawning an entity.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnEntity {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The components, as an object of name to value.
    pub components: Value,
}

/// Arguments for reading a buffer.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadBuffer {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The buffer's id, as `list_resources` reports it.
    pub id: u64,
    /// The byte offset to read from.
    #[serde(default)]
    pub offset: u64,
    /// How many bytes to read; the rest of the buffer when omitted.
    #[serde(default)]
    pub size: Option<u64>,
}

/// Arguments for reading or sending input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InputArg {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity carrying the input state, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub entity: Option<u64>,
}

/// Arguments for sending input events.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendInput {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity carrying the input state, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub entity: Option<u64>,
    /// The events to deliver.
    pub events: Vec<Value>,
}

/// Arguments for rendering a frame.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RendererArg {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
}

/// Arguments for taking a screenshot.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Screenshot {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
    /// The id of the texture to read, as `list_resources` reports it. Omit
    /// for the world's only readable texture.
    #[serde(default)]
    pub target: Option<u64>,
}

/// Arguments for creating a mesh.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMesh {
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity carrying the world's mesh source, as its `u64` bits. Omit
    /// for the world's only one.
    #[serde(default)]
    pub source: Option<u64>,
    /// Vertex positions, three floats each.
    pub positions: Vec<[f32; 3]>,
    /// Vertex UVs, two floats each.
    #[serde(default)]
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Vertex colours, RGBA bytes each.
    #[serde(default)]
    pub colors: Option<Vec<[u8; 4]>>,
    /// Triangle indices.
    #[serde(default)]
    pub indices: Option<Vec<u32>>,
    /// The entity's transform.
    #[serde(default)]
    pub transform: Option<MeshTransform>,
    /// The instance colour; opaque white by default.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
    /// The alpha cutoff, when the mesh should be cut out.
    #[serde(default)]
    pub cutoff: Option<f32>,
}

/// A transform as JSON.
#[derive(Debug, Deserialize, JsonSchema)]
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
    /// The host entity carrying the world to drive, as its `u64` bits; omit
    /// for the host world.
    #[serde(default)]
    pub world: Option<u64>,
    /// The entity carrying the world's mesh source, as its `u64` bits. Omit
    /// for the world's only one.
    #[serde(default)]
    pub source: Option<u64>,
    /// The path to the glTF or GLB file.
    pub path: String,
}
