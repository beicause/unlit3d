//! The MCP service: the tool router, the shared argument types and the transport.
//!
//! Each tool turns its JSON arguments into a [McpCommand] and sends it to the
//! thread that owns the world. The tool then awaits the result: structured JSON
//! for most tools, an image or audio content block for the ones that read
//! pixels or samples back inline, and a written path for the ones that save
//! to a file. A tool that fails returns a tool-level error result rather
//! than a protocol error, so a model sees the message instead of a transport
//! failure.
//!
//! The router starts empty and the engine crates fill it in: a crate submits a
//! [ToolSource] whose `extend` registers the tools that crate defines. The
//! server therefore carries tools this crate knows nothing about, and a tool
//! whose types live elsewhere still runs against the world through the same
//! command channel.

use std::error::Error;
use std::future::Future;
use std::sync::Arc;

use rmcp::handler::server::tool::schema_for_input;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::{ErrorData, ServerHandler, tool_handler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::mcp::host::{McpCommand, Output, WorldId};
use crate::{Entity, World};

pub use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};

/// The channel a tool sends its commands over.
pub type CommandSender = async_channel::Sender<McpCommand>;

/// What a tool handler answers with.
pub type ToolResult = Result<CallToolResult, ErrorData>;

/// A crate's contribution to the server's tool set.
///
/// A crate that defines tools of its own submits one of these from a link-time
/// registration; the server calls every `extend` once, while it is built.
pub struct ToolSource {
    /// Register this crate's tools on the router.
    pub extend: fn(&mut ToolRouter<McpServer>),
}

inventory::collect!(ToolSource);

/// An entity handle from the `u64` bits a JSON argument carries.
pub fn entity(bits: u64) -> Entity {
    Entity::from_bits(bits)
}

/// A tool route from a name, a description and an asynchronous handler.
///
/// The handler receives the command channel it sends its work over and the
/// arguments the client passed. Its argument type derives [JsonSchema], so the
/// advertised input schema is the one this crate's own tools advertise too.
pub fn tool<A, F, Fut>(
    name: &'static str,
    description: &'static str,
    handler: F,
) -> ToolRoute<McpServer>
where
    A: DeserializeOwned + JsonSchema + Send + 'static,
    F: Fn(CommandSender, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ToolResult> + Send + 'static,
{
    let schema =
        schema_for_input::<Parameters<A>>().expect("a tool's arguments describe an object");
    let attribute = Tool::new(name, description, schema);
    let handler = Arc::new(handler);
    ToolRoute::<McpServer>::new_dyn(attribute, move |context| {
        let handler = Arc::clone(&handler);
        let commands = context.service.commands.clone();
        let arguments = context.arguments;
        Box::pin(async move {
            let args = serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
            let result: CallToolResponse = handler(commands, args).await?.into();
            Ok(result)
        })
    })
}

/// The MCP server driving one or more worlds.
#[derive(Debug, Clone)]
pub struct McpServer {
    commands: CommandSender,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// A server that sends its commands over `commands`.
    ///
    /// Every submitted [ToolSource] registers its tools as the server is built.
    pub fn new(commands: CommandSender) -> Self {
        let mut tool_router = ToolRouter::new();
        for source in inventory::iter::<ToolSource> {
            (source.extend)(&mut tool_router);
        }
        Self {
            commands,
            tool_router,
        }
    }
}

/// Send a job to the world `world` names and await what it answered.
///
/// The host - not this crate - turns `world` into a world: it routes the
/// command to the thread that owns that world, which calls
/// [`McpCommand::dispatch`] on it.
pub async fn send(
    commands: &CommandSender,
    world: WorldId,
    job: impl FnOnce(&mut World) -> Result<Output, String> + Send + 'static,
) -> Result<Result<Output, String>, ErrorData> {
    let (command, receiver) = McpCommand::new(job);
    let command = command.with_world(world);
    commands
        .send(command)
        .await
        .map_err(|_| ErrorData::internal_error("the world thread is gone", None))?;
    receiver
        .recv()
        .await
        .map_err(|_| ErrorData::internal_error("the world thread dropped the command", None))
}

/// Send a job to the world `world` names and await its output.
pub async fn run(
    commands: &CommandSender,
    world: WorldId,
    job: impl FnOnce(&mut World) -> Result<Output, String> + Send + 'static,
) -> ToolResult {
    match send(commands, world, job).await? {
        Ok(Output::Json(value)) => Ok(CallToolResult::structured(value)),
        // An image answers with an MCP image content block so the client
        // renders the pixels, and carries its size as structured content so a
        // client reads it as data instead of from the picture.
        Ok(Output::Image(image)) => {
            let mut result =
                CallToolResult::success(vec![ContentBlock::image(image.data, image.mime_type)]);
            result.structured_content = Some(image.structured);
            Ok(result)
        }
        // A sound answers the same way, in an audio content block, so a
        // client decodes the stream itself rather than a base64 field it has
        // to find in the JSON.
        Ok(Output::Audio(audio)) => {
            let mut result =
                CallToolResult::success(vec![ContentBlock::audio(audio.data, audio.mime_type)]);
            result.structured_content = Some(audio.structured);
            Ok(result)
        }
        Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
    }
}

/// Send a job to the world `world` names and await its JSON result.
pub async fn command(
    commands: &CommandSender,
    world: WorldId,
    job: impl FnOnce(&mut World) -> Result<Value, String> + Send + 'static,
) -> ToolResult {
    run(commands, world, move |world| job(world).map(Output::from)).await
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Drives one or more worlds through the public API of the crates that registered their tools: inspect and edit entities and components, inspect a world's resource graph, read or save buffers and textures, send input, and render or screenshot frames. Every tool names its world with a WorldId (\"Host\", or {\"Handle\": bits} for a world a host entity carries), and a tool that needs engine handles names them explicitly.",
        )
    }
}

/// No arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoArgs {}

/// Serve MCP over stdio, handing every command to `sink`.
///
/// `sink` is how a command reaches the thread that owns the world its
/// [WorldId] names: a host sends it to its own render thread, or a windowed host
/// posts it to its event loop, where it resolves the id to a world and calls
/// [`McpCommand::dispatch`]. Returning `false` means the command could not be
/// delivered - the event loop is gone, say - and the waiting tool call is
/// answered with an internal error rather than left hanging.
///
/// The host answers the `list_worlds` tool itself: when a command's
/// [`wants_worlds`](McpCommand::wants_worlds) is true, it names every world it
/// carries - with the name each was spawned under - through
/// [`answer_worlds`](McpCommand::answer_worlds) instead of dispatching a job.
/// This crate keeps no list of its own, because the worlds on other threads are
/// not types it can recognise.
///
/// # Errors
///
/// Fails when the tokio runtime cannot be built or the transport cannot start.
pub fn serve_stdio_with(
    sink: impl Fn(McpCommand) -> bool + Send + 'static,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async move {
        let service = serve_with(rmcp::transport::stdio(), sink)
            .await
            .map_err(|error| -> Box<dyn Error + Send + Sync> { error })?;
        service.waiting().await?;
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    })
}

/// Serve MCP over any transport, handing every command to `sink`.
///
/// The transport is whatever rmcp accepts - stdio, or a pair of streams for a
/// test that wants the server in its own process. The returned service is
/// running; await [`waiting`](rmcp::service::RunningService::waiting) to serve
/// until the peer leaves.
///
/// # Errors
///
/// Fails when the service cannot complete its handshake over `transport`.
pub async fn serve_with<T, E, A>(
    transport: T,
    sink: impl Fn(McpCommand) -> bool + Send + 'static,
) -> Result<
    rmcp::service::RunningService<rmcp::RoleServer, McpServer>,
    // Boxed because the error is large and this is the only place it is
    // constructed: a caller reports it, it does not match on it.
    Box<rmcp::service::ServerInitializeError>,
>
where
    T: rmcp::transport::IntoTransport<rmcp::RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let (sender, receiver) = async_channel::bounded::<McpCommand>(4);
    // A pump rather than a task per command: the sink is synchronous, and the
    // service would otherwise be blocked on it.
    tokio::task::spawn_blocking(move || {
        while let Ok(command) = receiver.recv_blocking() {
            if !sink(command) {
                break;
            }
        }
    });
    rmcp::serve_server(McpServer::new(sender), transport)
        .await
        .map_err(Box::new)
}

/// Which world a tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorldArg {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
}

/// Which world and render context a GPU tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextArg {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// Any entity carrying the world's render context, as its `u64` bits —
    /// its device, queue, graph or capabilities. Omit when the world holds
    /// exactly one.
    #[serde(default)]
    pub context: Option<u64>,
}

/// Which world and resource a handle tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HandleArg {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The resource's id, as `list_resources` reports it.
    pub id: u64,
    /// The image format to answer with: `"webp"` (lossless, the default),
    /// `"png"`, or `"jpeg"`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Arguments for one entity in a world.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityArg {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
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
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
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
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
    /// The entity's bits.
    pub entity: u64,
    /// The component's name.
    pub component: String,
}

/// Arguments for writing one component.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetComponent {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
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
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
    /// The components, as an object of name to value.
    pub components: Value,
}

/// Arguments for reading a buffer.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadBuffer {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
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

/// Arguments for saving a buffer to a file.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveBuffer {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
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
    /// The directory to write the file into; created when it does not exist.
    pub directory: String,
    /// The file name to write; it is given a `.bin` extension when it carries
    /// none.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Arguments for saving a texture as an image.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveTexture {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The resource's id, as `list_resources` reports it.
    pub id: u64,
    /// The directory to write the file into; created when it does not exist.
    pub directory: String,
    /// The file name to write; its extension picks the image format, and it is
    /// given a `.webp` one when it carries none.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Arguments for saving a screenshot as an image.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveScreenshot {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
    /// The id of the texture to read, as `list_resources` reports it. Omit
    /// for the world's only readable texture.
    #[serde(default)]
    pub target: Option<u64>,
    /// The directory to write the file into; created when it does not exist.
    pub directory: String,
    /// The file name to write; its extension picks the image format, and it is
    /// given a `.webp` one when it carries none.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Arguments for reading or sending input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InputArg {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity carrying the input state, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub entity: Option<u64>,
}

/// Arguments for sending input events.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendInput {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
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
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
}

/// Arguments for taking a screenshot.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Screenshot {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
    /// The id of the texture to read, as `list_resources` reports it. Omit
    /// for the world's only readable texture.
    #[serde(default)]
    pub target: Option<u64>,
    /// The image format to answer with: `"webp"` (lossless, the default),
    /// `"png"`, or `"jpeg"`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Arguments for creating a mesh.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMesh {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The scope to spawn into, as its `u64` bits. Omit for the entity
    /// carrying the mesh source.
    #[serde(default)]
    pub scope: Option<u64>,
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
    /// Vertex normals, three floats each. Omit to shade the surface by the
    /// normals its positions give it.
    #[serde(default)]
    pub normals: Option<Vec<[f32; 3]>>,
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
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: WorldId,
    /// The scope to load into, as its `u64` bits. Omit for the entity
    /// carrying the mesh source.
    #[serde(default)]
    pub scope: Option<u64>,
    /// The entity carrying the world's mesh source, as its `u64` bits. Omit
    /// for the world's only one.
    #[serde(default)]
    pub source: Option<u64>,
    /// The path to the glTF or GLB file.
    pub path: String,
}
