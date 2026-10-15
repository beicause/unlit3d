//! The [Model Context Protocol](https://modelcontextprotocol.io) server and the
//! tools a world answers through it.
//!
//! A [World](crate::World) is not `Send`, so it stays on the thread that owns
//! it; the protocol runs on its own tokio thread and the two meet at an
//! [McpCommand] carrying the [WorldId] of the
//! world it should run against.
//!
//! The tools themselves live in the crates that define the types they read. A
//! crate submits a [ToolSource], whose `extend` registers that crate's tools;
//! [McpServer] calls every submission while it is built. The server therefore
//! carries the ECS tools from this crate, the rendering tools from
//! [unlit3d](https://docs.rs/unlit3d), and any tool another crate adds, without
//! depending on any of them. Not built on wasm, where a browser drives the world
//! directly rather than through this protocol.

pub mod host;
mod server;
mod tools;

pub use crate::reflect::{ComponentEntry, contains, encode, entries, entry, names, push, set};
pub use host::{
    Despawned, EncodedImage, Job, McpCommand, Output, SavedFile, SpawnedEntity, WorldEntry,
    WorldId, extension_of, saved_name, to_value, with_extension, write_file,
};
pub use inventory;
pub use server::{
    CommandSender, ContextArg, CreateMesh, EntityArg, GetComponent, HandleArg, InputArg,
    ListEntities, LoadGltf, McpServer, MeshTransform, NoArgs, ReadBuffer, RendererArg, SaveBuffer,
    SaveScreenshot, SaveTexture, Screenshot, SendInput, SetComponent, SpawnEntity, ToolResult,
    ToolRouter, ToolSource, WorldArg, command, entity, run, send, serve_stdio_with, serve_with,
    tool,
};

/// The JSON crate a tool uses to build structured output.
pub use serde_json;
