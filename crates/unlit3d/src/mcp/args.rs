//! The arguments the rendering and input tools take.
//!
//! A tool's arguments are the tool's own contract with the client, so they
//! live beside the tools that read them rather than in the crate that serves
//! the protocol. What they share is the [`WorldRef`] every tool names its
//! world with.

use schemars::JsonSchema;
use serde::Deserialize;

use unlit_ecs::mcp::WorldRef;
use unlit_ecs::mcp::serde_json::Value;
/// Which world and render context a GPU tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextArg {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// Any entity carrying the world's render context, as its `u64` bits —
    /// its device, queue, graph or capabilities. Omit when the world holds
    /// exactly one.
    #[serde(default)]
    pub context: Option<u64>,
}

/// Which world and resource a handle tool runs against.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandleArg {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The resource's id, as `list_resources` reports it. A texture view is
    /// read through the texture it views.
    pub id: u64,
    /// The image format to answer with: `"webp"` (lossless, the default),
    /// `"png"`, or `"jpeg"`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Arguments for reading a buffer.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadBuffer {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
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
#[serde(deny_unknown_fields)]
pub struct SaveBuffer {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
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
#[serde(deny_unknown_fields)]
pub struct SaveTexture {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The resource's id, as `list_resources` reports it. A texture view is
    /// read through the texture it views.
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
#[serde(deny_unknown_fields)]
pub struct SaveScreenshot {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
    /// The id of the texture to read back, as `list_resources` reports it, or the
    /// id of any view of it.
    ///
    /// A target built by a presenter or an offscreen scene carries a `label` in
    /// that listing — `"scene color"` for the texture a frame lands in — so the
    /// client names the one it means instead of trying each `Texture` in turn.
    pub target: u64,
    /// The directory to write the file into; created when it does not exist.
    pub directory: String,
    /// The file name to write; its extension picks the image format, and it is
    /// given a `.webp` one when it carries none.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Arguments for reading or sending input.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputArg {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// The entity carrying the input state, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub entity: Option<u64>,
}

/// Arguments for sending input events.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendInput {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// The entity carrying the input state, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub entity: Option<u64>,
    /// The events to deliver.
    pub events: Vec<Value>,
}

/// Arguments for taking a screenshot.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Screenshot {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// Any entity carrying the world's render context, as its `u64` bits.
    /// Omit when the world holds exactly one.
    #[serde(default)]
    pub context: Option<u64>,
    /// The entity carrying the renderer, as its `u64` bits. Omit for the
    /// world's only one.
    #[serde(default)]
    pub renderer: Option<u64>,
    /// The id of the texture to read back, as `list_resources` reports it, or the
    /// id of any view of it.
    ///
    /// A target built by a presenter or an offscreen scene carries a `label` in
    /// that listing — `"scene color"` for the texture a frame lands in — so the
    /// client names the one it means instead of trying each `Texture` in turn.
    pub target: u64,
    /// The image format to answer with: `"webp"` (lossless, the default),
    /// `"png"`, or `"jpeg"`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Arguments for spawn_gltf.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnGltf {
    /// The world to drive, as a world reference names it.
    #[serde(default)]
    pub world: WorldRef,
    /// The scope to load into, as its `u64` bits. Omit for the entity
    /// carrying the mesh source.
    #[serde(default)]
    pub scope: Option<u64>,
    /// The entity carrying the world's mesh source, as its `u64` bits. Omit
    /// for the world's only one.
    #[serde(default)]
    pub source: Option<u64>,
    /// The path of a glTF or GLB file to read.
    #[serde(default)]
    pub path: Option<String>,
    /// The document itself, instead of a path: glTF JSON text, or a GLB's
    /// bytes encoded as base64. Pass exactly one of this field and `path`.
    #[serde(default)]
    pub document: Option<String>,
}
