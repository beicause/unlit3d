//! A Model Context Protocol server that drives a unlit3d world.
//!
//! The server exposes the world, its components and the resource graph as MCP
//! tools. It has no privileged path into the engine: every tool calls the same
//! public API a normal application would, so anything the server can do, a
//! caller of the crates can do too.
//!
//! The world lives on a render thread; the MCP transport runs on a tokio
//! runtime. A tool turns its arguments into a [Command] and sends it to the
//! render thread, which owns the [unlit_ecs::World].

#![forbid(unsafe_code)]

pub mod host;
pub mod server;

pub use host::{Command, Host, HostContext, dispatch, request_device};
pub use server::McpServer;
pub use unlit3d::reflect::{
    ComponentEntry, contains, decode_events, encode, entries, entry, names, push, set,
};

use std::error::Error;

/// Serve the MCP protocol over stdio, driving the [Host] host builds.
///
/// The host is built on the render thread — the world it owns is not [Send] —
/// and its tools run there. The protocol itself runs on a current-thread tokio
/// runtime. This call blocks until the peer disconnects.
///
/// # Errors
///
/// Fails when the render thread cannot be started, the host cannot be built, or
/// the protocol transport fails.
pub fn serve_stdio(
    host: impl FnOnce() -> Result<Host, String> + Send + 'static,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (sender, receiver) = async_channel::bounded::<Command>(4);
    let (ready_sender, ready_receiver) = std::sync::mpsc::channel::<Result<(), String>>();
    let render = std::thread::Builder::new()
        .name("unlit3d-mcp-render".to_owned())
        .spawn(move || {
            let mut host = match host() {
                Ok(host) => {
                    let _ = ready_sender.send(Ok(()));
                    host
                }
                Err(message) => {
                    let _ = ready_sender.send(Err(message));
                    return;
                }
            };
            while let Ok(command) = receiver.recv_blocking() {
                host.dispatch(command);
            }
        })?;

    // Do not start speaking the protocol before the world exists: a tool that
    // arrived first would have nowhere to run.
    match ready_receiver.recv() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return Err(message.into()),
        Err(_) => return Err("the render thread stopped before building the host".into()),
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let served = runtime.block_on(async move {
        let service = rmcp::serve_server(McpServer::new(sender), rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    });

    let _ = render.join();
    served
}
