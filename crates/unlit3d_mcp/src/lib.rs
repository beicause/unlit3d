//! A Model Context Protocol server that drives one or more unlit3d worlds.
//!
//! A [World](unlit_ecs::World) is not `Send`, so it lives on the thread that
//! owns it — a window's event loop, or the render thread the standalone binary
//! starts. The MCP transport runs elsewhere, on a tokio runtime. The two meet
//! at a [Command]: a tool packs a closure and the handle of the world it should
//! run against, the transport hands the command to a [sink](serve_stdio_with),
//! and the thread that owns the world resolves the handle and calls [dispatch].
//!
//! Nothing here assumes how many worlds there are, or that a world holds a
//! rendering device: the ECS tools take the world alone, and the GPU tools take
//! the entity handles they need. The host — not this crate — decides which
//! world a handle names.

#![forbid(unsafe_code)]

use std::error::Error;
use std::sync::mpsc;

pub mod host;
pub mod server;

pub use host::{COLOR_FORMAT, Command, Job, dispatch, offscreen_world, request_device};
pub use server::McpServer;
pub use unlit3d::reflect::{
    ComponentEntry, contains, decode_events, encode, entries, entry, names, push, set,
};

/// Serve MCP over stdio, handing every command to `sink`.
///
/// `sink` is how a command reaches the thread that owns a world: the standalone
/// binary and the CLI send it to their own render thread, and a windowed host
/// posts it to its event loop. Returning `false` means the command could not be
/// delivered — the event loop is gone, say — and the waiting tool call is
/// answered with an internal error rather than left hanging.
///
/// # Errors
///
/// Fails when the tokio runtime cannot be built or the transport cannot start.
pub fn serve_stdio_with(
    sink: impl Fn(Command) -> bool + Send + 'static,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (sender, receiver) = async_channel::bounded::<Command>(4);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async move {
        // A pump rather than a task per command: the sink is synchronous, and
        // the transport's own thread would otherwise be blocked on it.
        let pump = tokio::task::spawn_blocking(move || {
            while let Ok(command) = receiver.recv_blocking() {
                if !sink(command) {
                    break;
                }
            }
        });
        let service = rmcp::serve_server(McpServer::new(sender), rmcp::transport::stdio()).await?;
        service.waiting().await?;
        pump.abort();
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    })
}
/// Serve MCP over stdio with a fresh headless world of its own.
///
/// The convenience entry point for a host with no window: it builds an
/// offscreen world on a render thread and serves it, which is what the
/// standalone binary and the CLI's `--mcp` path want. A windowed host calls
/// [`serve_stdio_with`] instead and posts commands to its event loop.
///
/// # Errors
///
/// Fails when no adapter is available, when the render thread cannot start, or
/// when the transport fails.
pub fn serve_stdio_offscreen(
    size: (u32, u32),
    samples: u32,
    depth: bool,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (sender, receiver) = async_channel::bounded::<Command>(4);
    let (ready_sender, ready_receiver) = mpsc::channel::<Result<(), String>>();
    let render = std::thread::Builder::new()
        .name("unlit3d-mcp-render".to_owned())
        .spawn(move || {
            let (device, queue, capabilities) =
                match pollster::block_on(request_device("unlit3d-mcp")) {
                    Ok(parts) => parts,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
            let (mut world, _renderer) =
                offscreen_world(device, queue, capabilities, size, samples, depth);
            let _ = ready_sender.send(Ok(()));
            while let Ok(command) = receiver.recv_blocking() {
                dispatch(command, &mut world);
            }
        })?;
    match ready_receiver.recv() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => return Err("the render thread stopped before building the world".into()),
    }
    let served = serve_stdio_with(move |command| sender.send_blocking(command).is_ok());
    let _ = render.join();
    served
}
