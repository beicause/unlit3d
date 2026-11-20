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

use crate::World;
use crate::mcp::host::{McpCommand, Output, WorldRef, panic_message};

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
        let mut tool_router = ToolRouter::default();
        for source in inventory::iter::<ToolSource> {
            // Each source registers into a router of its own first, so a name
            // two crates both claim is caught here rather than letting the
            // later one quietly displace the earlier one's tool.
            let mut from_source = ToolRouter::default();
            (source.extend)(&mut from_source);
            add_routes(&mut tool_router, from_source);
        }
        Self {
            commands,
            tool_router,
        }
    }
}

/// Move the routes of one tool source into the server's router.
///
/// A name already in use is kept rather than overwritten: the client reaches
/// the tool registered first, and the collision is reported so the crate that
/// registered it can rename its tool. Two crates claiming one name is a
/// registration error no client can recover from on its own - the second tool
/// disappears without a word - so it is never resolved silently.
fn add_routes(into: &mut ToolRouter<McpServer>, from: ToolRouter<McpServer>) {
    for (name, route) in from.map {
        if into.map.contains_key(&name) {
            log::error!(
                "two tool sources both registered a tool named {name:?}; the one registered \
                 first is the one a client reaches"
            );
            continue;
        }
        into.add_route(route);
    }
}

/// Send a job to the world `world` names and await what it answered.
///
/// The host - not this crate - turns `world` into a world: it routes the
/// command to the thread that owns that world, which calls
/// [`McpCommand::dispatch`] on it.
pub async fn send(
    commands: &CommandSender,
    world: WorldRef,
    job: impl FnOnce(&mut World) -> Result<Output, String> + Send + 'static,
) -> Result<Result<Output, String>, ErrorData> {
    let (command, receiver) = McpCommand::new(job);
    exchange(commands, command.with_world(world), receiver).await
}

/// Send one command and await the answer the host gave it.
///
/// The outer error is the transport's: no world thread took the command, or
/// the thread that took it dropped it. The inner error is the tool's own: the
/// job ran against the world and failed, and its message is what the client
/// reads.
///
/// Every request this crate sends goes through here, so a channel that closes
/// is reported the same way no matter which tool noticed.
///
/// The wait itself has no timeout of its own, and none is needed: a client that
/// leaves drops the tool call, which drops this future and with it the
/// receiver, so a host that answers late finds nobody waiting and drops the
/// answer. What a timeout would add is a bound on work this crate cannot see —
/// a world thread may be running a job that legitimately takes as long as it
/// takes — so the deadline belongs to the client, which knows what it asked
/// for and can ask again.
async fn exchange(
    commands: &CommandSender,
    command: McpCommand,
    receiver: async_channel::Receiver<Result<Output, String>>,
) -> Result<Result<Output, String>, ErrorData> {
    commands
        .send(command)
        .await
        .map_err(|_| ErrorData::internal_error("the world thread is gone", None))?;
    receiver
        .recv()
        .await
        .map_err(|_| ErrorData::internal_error("the world thread dropped the command", None))
}

/// Ask the host which worlds it carries, as the JSON listing it answered with.
///
/// Only the host knows: a world on another thread is not a component this
/// crate can recognise, so the question is asked rather than answered from a
/// list the crate keeps.
///
/// The answer stays JSON because a world entry names its world with a static
/// string, which is what the host holds and what the wire carries; a reader
/// that wants the entries typed reads them out of the value itself.
///
/// A host that answers with anything else is a protocol error, not a tool
/// failure: the request has no job whose failure a client could act on, and
/// only a host that failed to answer at all can produce one.
pub async fn worlds(commands: &CommandSender) -> Result<Value, ErrorData> {
    let (request, receiver) = McpCommand::list_worlds();
    match exchange(commands, request, receiver).await? {
        Ok(Output::Json(value)) => Ok(value),
        Ok(_) => Err(ErrorData::internal_error(
            "list_worlds was answered with something other than JSON",
            None,
        )),
        Err(message) => Err(ErrorData::internal_error(message, None)),
    }
}

/// Send a job to the world `world` names and await its output.
pub async fn run(
    commands: &CommandSender,
    world: WorldRef,
    job: impl FnOnce(&mut World) -> Result<Output, String> + Send + 'static,
) -> ToolResult {
    reply(send(commands, world, job).await?)
}

/// Turn the answer a host gave into the result the protocol carries.
///
/// Every output shape becomes the block a client renders it in, and a failed
/// job becomes a tool-level error rather than a transport one, so a model
/// reads the message instead of seeing the call fail.
fn reply(answer: Result<Output, String>) -> ToolResult {
    match answer {
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
    world: WorldRef,
    job: impl FnOnce(&mut World) -> Result<Value, String> + Send + 'static,
) -> ToolResult {
    run(commands, world, move |world| job(world).map(Output::from)).await
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Drives one or more worlds through the public API of the crates that registered their tools: inspect and edit entities and components, inspect a world's resource graph, read or save buffers and textures, send input, and render or screenshot frames. Every tool names its world with a WorldRef (\"Host\", or {\"Handle\": bits} for a world a host entity carries), and a tool that needs engine handles names them explicitly.",
        )
    }
}

/// No arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoArgs {}

/// Serve MCP over stdio, handing every command to `sink`.
///
/// `sink` is how a command reaches the thread that owns the world its
/// [WorldRef] names: a host sends it to its own render thread, or a windowed host
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
    // Every driver, not just the timer: a transport reads and writes pipes,
    // which panics on a runtime without the IO driver.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
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
        // A sink that panics unwinds the pump, which would end the one thread
        // every tool call goes through. The panic is caught so the calls still
        // queued are answered with what happened instead of being dropped as
        // if the world thread had gone missing: they never reached a world.
        //
        // The call the sink was delivering when it panicked has no answer:
        // the sink unwound mid-way, so what it guards is not promised to be
        // usable again, and the pump stops rather than handing it more work.
        let mut stopped = None;
        while let Ok(command) = receiver.recv_blocking() {
            let delivered =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(command)));
            match delivered {
                Ok(true) => {}
                Ok(false) => {
                    stopped = Some("the transport that delivers commands stopped".to_owned());
                    break;
                }
                Err(payload) => {
                    let message = format!(
                        "the transport that delivers commands panicked: {}",
                        panic_message(payload.as_ref())
                    );
                    log::error!("{message}");
                    stopped = Some(message);
                    break;
                }
            }
        }
        if let Some(message) = stopped {
            while let Ok(command) = receiver.try_recv() {
                command.fail(message.clone());
            }
        }
    });
    rmcp::serve_server(McpServer::new(sender), transport)
        .await
        .map_err(Box::new)
}

/// Which world a tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorldArg {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
}

/// Arguments for one entity in a world.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityArg {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
    /// The entity's bits.
    pub entity: u64,
}

/// Arguments for listing entities.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListEntities {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
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
#[serde(deny_unknown_fields)]
pub struct GetComponent {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
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
#[serde(deny_unknown_fields)]
pub struct SetComponent {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
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
#[serde(deny_unknown_fields)]
pub struct SpawnEntity {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// The entity whose scope the tool runs in, as its `u64` bits. Omit for the
    /// world's root scope, which sees everything.
    #[serde(default)]
    pub scope: Option<u64>,
    /// The components, as an object of name to value.
    pub components: Value,
}

#[cfg(test)]
mod tests {
    use rmcp::model::Tool;

    use super::{McpServer, ToolRouter, add_routes};

    /// A router carrying one tool with the given name.
    fn with_tool(name: &'static str) -> ToolRouter<McpServer> {
        let mut router = ToolRouter::default();
        let route = super::tool(name, "a tool", |_commands, _args: super::NoArgs| async {
            unreachable!("the test never calls the tool")
        });
        router.add_route(route);
        router
    }

    #[test]
    fn a_name_two_sources_claim_reaches_the_one_registered_first() {
        // The name a client sends has to reach exactly one tool. Letting the
        // later registration displace the earlier one would make a crate's
        // tools vanish without a word depending on link order, so the first
        // one wins and the collision is reported.
        let mut into = with_tool("read_texture");
        add_routes(&mut into, with_tool("read_texture"));
        assert_eq!(into.map.len(), 1, "the second registration was dropped");
        let routed: &Tool = &into.map["read_texture"].attr;
        assert_eq!(routed.description.as_deref(), Some("a tool"));
    }

    #[test]
    fn a_name_no_one_else_claims_is_added() {
        let mut into = with_tool("read_texture");
        add_routes(&mut into, with_tool("save_texture"));
        assert_eq!(into.map.len(), 2);
    }
}
