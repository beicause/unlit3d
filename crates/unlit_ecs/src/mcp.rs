//! The [Model Context Protocol](https://modelcontextprotocol.io) server and the
//! tools a world answers through it.
//!
//! A [World](crate::World) is not `Send`, so it stays on the thread that owns
//! it; the protocol runs on its own tokio thread and the two meet at an
//! [McpCommand] carrying the [WorldRef] of the world it should run against.
//!
//! The tools themselves live in the crates that define the types they read. A
//! crate submits a [ToolSource], whose `extend` registers that crate's tools;
//! [McpServer] calls every submission while it is built. The server therefore
//! carries the ECS tools from this crate, the rendering tools from
//! [unlit3d](https://docs.rs/unlit3d), and any tool another crate adds, without
//! depending on any of them. Not built on wasm, where a browser drives the world
//! directly rather than through this protocol.

pub mod host;
#[cfg(feature = "mcp-http")]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp-http")))]
pub mod http;
mod server;
mod tools;

pub use crate::reflect::{
    ComponentEntry, ComponentError, contains, current_partial, default_partial, encode_json,
    entries, entry, names, push, push_json, set, set_json,
};
pub use host::{
    Despawned, EncodedAudio, EncodedImage, Job, McpCommand, Output, SavedFile, SpawnedEntity,
    WorldEntry, WorldRef, extension_of, from_value, saved_name, to_value, with_extension,
    write_file,
};
#[cfg(feature = "mcp-http")]
pub use http::{McpHttpServer, serve_http_with};
pub use inventory;
/// The reply types a tool answers with, re-exported from the protocol crate so
/// a crate that registers tools builds its replies without depending on the
/// protocol itself.
pub use rmcp::ErrorData;
pub use rmcp::model::{CallToolResult, ContentBlock};
pub use server::{
    CommandSender, ContextArg, EntityArg, GetComponent, HandleArg, InputArg, ListEntities,
    McpServer, NoArgs, ReadBuffer, SaveBuffer, SaveScreenshot, SaveTexture, Screenshot, SendInput,
    SetComponent, SpawnEntity, SpawnGltf, ToolResult, ToolRouter,
    ToolSource, WorldArg, command, entity, run, send, serve_stdio_with, serve_with, tool,
};

/// The JSON crate a tool uses to build structured output.
pub use serde_json;
