//! A Model Context Protocol server that drives one or more unlit3d worlds.
//!
//! A [World](unlit_ecs::World) is not `Send`, so it lives on the thread that
//! owns it — a window's event loop, or a thread the host starts for it. The MCP
//! transport runs elsewhere, on a tokio runtime. The two meet at an
//! [McpCommand]: a tool packs a closure and the [WorldId] of the world it should
//! run against, the transport hands the command to a [sink](serve_stdio_with),
//! and the thread that owns that world resolves the id and calls
//! [McpCommand::dispatch].
//!
//! Nothing here assumes how many worlds there are, or that a world holds a
//! rendering device: the ECS tools take the world alone, and the GPU tools take
//! the entity handles they need. The host — not this crate — decides which
//! world a handle names.

#![forbid(unsafe_code)]

use std::error::Error;

pub mod host;
pub mod server;

pub use host::{EncodedImage, Job, McpCommand, Output, WorldEntry, WorldId};
pub use server::McpServer;
pub use unlit3d::reflect::{
    ComponentEntry, contains, decode_events, encode, entries, entry, names, push, set,
};

/// Serve MCP over stdio, handing every command to `sink`.
///
/// `sink` is how a command reaches the thread that owns the world its
/// [WorldId] names: a host sends it to its own render thread, or a windowed
/// host posts it to its event loop, where it resolves the id to a world and
/// calls
/// [`McpCommand::dispatch`](host::McpCommand::dispatch). Returning `false`
/// means the command could not be delivered — the event loop is gone, say —
/// and the waiting tool call is answered with an internal error rather than
/// left hanging.
///
/// The host answers the `list_worlds` tool itself: when a command's
/// [`wants_worlds`](host::McpCommand::wants_worlds) is true, it names every
/// world it carries — with the name each was spawned under — through
/// [`answer_worlds`](host::McpCommand::answer_worlds) instead of dispatching
/// a job. This crate keeps no list of its own, because
/// the worlds on other threads are not types it can recognise.
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
/// The transport is whatever rmcp accepts — stdio, or a pair of streams for a
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
