//! The world side of the MCP server.
//!
//! A [World] is not [Send], and the work that reads it must happen on the
//! thread that owns it. The MCP transport, on the other hand, runs on a tokio
//! thread. The two meet at a command channel: a tool builds an [McpCommand]
//! holding a closure and the [WorldId] of the world it should run against, the
//! thread that owns that world resolves the id and calls
//! [McpCommand::dispatch], and the [Output] travels back on the command's reply
//! channel.
//!
//! Nothing here assumes how many worlds there are or what a world contains. A
//! crate that defines tools of its own submits a
//! [ToolSource](crate::mcp::ToolSource) and keeps its tool bodies next to the
//! types they read; this module carries only the contract every tool shares.

use std::path::Path;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{Entity, World};

/// What a finished job produced.
///
/// Most tools answer with structured JSON that the transport serialises
/// through the value's reflection. Image tools answer with an encoded image
/// that the transport sends as an MCP image content block, so a client
/// renders the picture instead of decoding JSON.
pub enum Output {
    /// A structured JSON result.
    Json(Value),
    /// An encoded image.
    Image(EncodedImage),
}

impl From<Value> for Output {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

/// An image the transport sends as an image content block.
pub struct EncodedImage {
    /// The encoded bytes, base64-encoded.
    pub data: String,
    /// The format the bytes are in, as a MIME type.
    pub mime_type: String,
    /// The image's metadata — its size — as structured JSON.
    pub structured: Value,
}

/// A unit of work for the thread that owns a world.
///
/// The closure captures plain data (tool arguments) and runs with exclusive
/// access to one world. It must be [Send] because it travels across the
/// channel; the world it touches never does.
pub type Job = Box<dyn FnOnce(&mut World) -> Result<Output, String> + Send + 'static>;

/// Which world a tool or a command runs against.
///
/// A host carries several worlds: the one its own thread holds directly, and
/// any number it reaches through a host entity carrying a [World] component.
/// A [World] is not [Send], so a command
/// never names one directly — it names the entity that carries it, and the
/// thread that owns that entity resolves the id to the world it holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema, facet::Facet)]
#[repr(u8)]
pub enum WorldId {
    /// The world the thread receiving the command owns directly.
    #[default]
    Host,
    /// The world carried by the host entity with these bits.
    Handle(u64),
}

impl WorldId {
    /// The id of the world carried by `entity`.
    #[must_use]
    pub const fn handle(entity: Entity) -> Self {
        Self::Handle(entity.to_bits())
    }

    /// The host entity carrying the world, or `None` for the host world.
    #[must_use]
    pub const fn entity(self) -> Option<Entity> {
        match self {
            Self::Host => None,
            Self::Handle(bits) => Some(Entity::from_bits(bits)),
        }
    }
}

/// One world a host reports, with the name it was spawned under.
///
/// `list_worlds` answers with these: the id is what every other tool names
/// the world by, and the name is what tells a person which world it is. The
/// host supplies both, because only the host knows the worlds it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, facet::Facet)]
pub struct WorldEntry {
    /// The id every other tool names this world by.
    pub world: WorldId,
    /// The name the world was spawned under.
    pub name: &'static str,
}

/// What a command asks the host to do.
enum Request {
    /// Run a job against one world.
    Job(Job),
    /// Answer with the worlds the host carries. Only the host knows them: the
    /// worlds on other threads are not components this crate can recognise, so
    /// the server asks rather than keeping a list of its own.
    Worlds,
}

/// A request together with the channel its result is sent back on.
pub struct McpCommand {
    request: Request,
    reply: async_channel::Sender<Result<Output, String>>,
    /// The world a [`Request::Job`] runs against.
    world: WorldId,
}

impl McpCommand {
    /// Build a command that runs a job, and the receiver its result will
    /// arrive on.
    ///
    /// The command starts out aimed at the world the receiving thread owns
    /// directly; redirect it with [`with_world`](Self::with_world).
    pub fn new(
        job: impl FnOnce(&mut World) -> Result<Output, String> + Send + 'static,
    ) -> (Self, async_channel::Receiver<Result<Output, String>>) {
        Self::build(Request::Job(Box::new(job)))
    }

    /// Build the command the `list_worlds` tool sends.
    ///
    /// The host answers it with [`answer_worlds`](Self::answer_worlds); the
    /// server holds no world list of its own.
    #[must_use]
    pub fn list_worlds() -> (Self, async_channel::Receiver<Result<Output, String>>) {
        Self::build(Request::Worlds)
    }

    fn build(request: Request) -> (Self, async_channel::Receiver<Result<Output, String>>) {
        let (reply, receiver) = async_channel::bounded(1);
        (
            Self {
                request,
                reply,
                world: WorldId::Host,
            },
            receiver,
        )
    }

    /// Direct the command at `world`.
    #[must_use]
    pub fn with_world(mut self, world: WorldId) -> Self {
        self.world = world;
        self
    }

    /// The world a job runs against.
    #[must_use]
    pub const fn world(&self) -> WorldId {
        self.world
    }

    /// Whether this is the `list_worlds` request, which the host answers with
    /// [`answer_worlds`](Self::answer_worlds) instead of dispatching a job.
    #[must_use]
    pub const fn wants_worlds(&self) -> bool {
        matches!(self.request, Request::Worlds)
    }

    /// Answer the `list_worlds` request with the worlds the host carries.
    ///
    /// The host names every world a tool can reach — the host world itself and
    /// each world it routes commands to, including ones on other threads —
    /// each with the name it was spawned under.
    pub fn answer_worlds(self, worlds: Vec<WorldEntry>) {
        let result = to_value(&worlds).map(Output::from);
        let _ = self.reply.send_blocking(result);
    }

    /// Answer the command with an error without running it.
    ///
    /// The host calls this when the world cannot be resolved, so the tool call
    /// still gets a readable message rather than a dropped channel.
    pub fn fail(self, message: String) {
        let _ = self.reply.send_blocking(Err(message));
    }

    /// Run the command's job against `world` and send its result back.
    ///
    /// A [`list_worlds`](Self::list_worlds) request has no job; the host is
    /// expected to answer it with [`answer_worlds`](Self::answer_worlds)
    /// after checking [`wants_worlds`](Self::wants_worlds).
    ///
    /// The send is best-effort: a tool call that has already been cancelled has
    /// no receiver left, and the result is simply dropped.
    pub fn dispatch(self, world: &mut World) {
        let result = match self.request {
            Request::Job(job) => job(world),
            Request::Worlds => Err("the host did not answer list_worlds".to_owned()),
        };
        let _ = self.reply.send_blocking(result);
    }
}

/// Serialise a value through the reflection it derives.
///
/// The information structs the original crates expose carry their own
/// reflection, so the JSON this transport speaks is derived from their fields
/// rather than written down a second time here.
pub fn to_value<T: facet::Facet<'static>>(value: &T) -> Result<Value, String> {
    let text = facet_json::to_string(value).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// A spawned entity, as the tools report it.
#[derive(facet::Facet)]
pub struct SpawnedEntity {
    /// The spawned entity.
    #[facet(opaque, proxy = crate::EntityProxy)]
    pub entity: Entity,
}

/// Whether a despawn removed anything.
#[derive(facet::Facet)]
pub struct Despawned {
    /// Whether the entity was alive.
    pub despawned: bool,
}

/// A file a save tool wrote.
#[derive(facet::Facet)]
pub struct SavedFile {
    /// The absolute path of the file that was written.
    pub path: String,
    /// How many bytes were written.
    pub size: usize,
}

/// The extension `file_name` carries, unless it is empty.
pub fn extension_of(file_name: &str) -> Option<&str> {
    Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
}

/// `file_name` with `extension` appended when it carries none, or `None`
/// when there is no file name to extend.
pub fn with_extension(file_name: Option<&str>, extension: &str) -> Option<String> {
    let file_name = file_name?;
    Some(match extension_of(file_name) {
        Some(_) => file_name.to_string(),
        None => format!("{file_name}.{extension}"),
    })
}

/// The name a save writes when its caller named none: `stem` with a fresh
/// random id, so a save never overwrites the file an earlier one wrote.
fn generated_name(stem: &str, extension: &str) -> String {
    format!("{stem}-{}.{extension}", uuid::Uuid::new_v4().simple())
}

/// The name a save writes: the caller's own, given `extension` when it
/// carries none, or a fresh `stem` name when the caller named none.
pub fn saved_name(file_name: Option<&str>, stem: &str, extension: &str) -> String {
    with_extension(file_name, extension).unwrap_or_else(|| generated_name(stem, extension))
}

/// Write `bytes` to `directory/file_name`, creating the directory.
pub fn write_file(directory: &str, file_name: &str, bytes: &[u8]) -> Result<SavedFile, String> {
    let path = Path::new(directory).join(file_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    // Report an absolute path, so a caller does not have to know the process's
    // working directory to find what was written.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    Ok(SavedFile {
        path: path.display().to_string(),
        size: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON a tool argument carries for a world.
    #[derive(Debug, Deserialize)]
    struct Arg {
        #[serde(default)]
        world: WorldId,
    }

    #[test]
    fn a_world_id_parses_from_json() {
        let host: Arg = serde_json::from_str("{}").unwrap();
        assert_eq!(host.world, WorldId::Host);
        let host: Arg = serde_json::from_str(r#"{"world": "Host"}"#).unwrap();
        assert_eq!(host.world, WorldId::Host);
        let handle: Arg = serde_json::from_str(r#"{"world": {"Handle": 7}}"#).unwrap();
        assert_eq!(handle.world, WorldId::Handle(7));
    }

    #[test]
    fn a_world_id_maps_to_and_from_an_entity() {
        let entity = Entity::from_bits(0x0000_0002_0000_0001);
        assert_eq!(
            WorldId::handle(entity),
            WorldId::Handle(0x0000_0002_0000_0001)
        );
        assert_eq!(WorldId::handle(entity).entity(), Some(entity));
        assert_eq!(WorldId::Host.entity(), None);
        assert_eq!(WorldId::default(), WorldId::Host);
    }

    #[test]
    fn a_list_worlds_command_carries_no_job() {
        let (command, receiver) = McpCommand::list_worlds();
        assert!(command.wants_worlds());
        assert_eq!(command.world(), WorldId::Host);
        command.answer_worlds(vec![
            WorldEntry {
                world: WorldId::Host,
                name: "host",
            },
            WorldEntry {
                world: WorldId::Handle(3),
                name: "render",
            },
        ]);
        let answer = receiver.recv_blocking().unwrap().unwrap();
        let Output::Json(value) = answer else {
            panic!("list_worlds answers with JSON");
        };
        assert_eq!(
            value,
            serde_json::json!([
                {"world": "Host", "name": "host"},
                {"world": {"Handle": 3}, "name": "render"}
            ])
        );
    }

    #[test]
    fn a_job_command_dispatches_against_a_world() {
        let (command, receiver) =
            McpCommand::new(|world| Ok(Output::Json(serde_json::json!(world.len()))));
        assert!(!command.wants_worlds());
        let mut world = World::new();
        world.spawn((1u32,));
        command.dispatch(&mut world);
        let answer = receiver.recv_blocking().unwrap().unwrap();
        let Output::Json(value) = answer else {
            panic!("a job answers with JSON");
        };
        assert_eq!(value, serde_json::json!(1));
    }

    #[test]
    fn an_extension_is_only_read_when_it_is_present() {
        assert_eq!(extension_of("a.png"), Some("png"));
        assert_eq!(extension_of("dir/a.webp"), Some("webp"));
        assert_eq!(extension_of("a"), None);
        assert_eq!(extension_of("a."), None);
    }

    #[test]
    fn a_missing_extension_is_appended() {
        assert_eq!(with_extension(None, "webp"), None);
        assert_eq!(with_extension(Some("a"), "webp").as_deref(), Some("a.webp"));
        assert_eq!(
            with_extension(Some("a.png"), "webp").as_deref(),
            Some("a.png")
        );
    }

    #[test]
    fn a_generated_name_carries_a_fresh_id() {
        let name = generated_name("texture", "webp");
        assert!(name.starts_with("texture-"), "{name}");
        assert!(name.ends_with(".webp"), "{name}");
        assert_ne!(name, generated_name("texture", "webp"));
    }

    #[test]
    fn a_saved_name_prefers_the_callers_own() {
        assert_eq!(saved_name(Some("a"), "buffer", "bin"), "a.bin");
        assert_eq!(saved_name(Some("a.png"), "buffer", "bin"), "a.png");
        let generated = saved_name(None, "screenshot", "webp");
        assert!(generated.starts_with("screenshot-"), "{generated}");
        assert!(generated.ends_with(".webp"), "{generated}");
    }
}
