//! The render-thread side of the MCP server.
//!
//! A [World] is not [Send], and the GPU work that reads it must happen on the
//! thread that owns it. The MCP transport, on the other hand, runs on a tokio
//! thread. The two meet at a command channel: a tool builds an [McpCommand]
//! holding a closure and the [WorldId] of the world it should run against, the
//! thread that owns that world resolves the id and calls
//! [McpCommand::dispatch], and the [Output] travels back on the command's reply
//! channel.
//!
//! Nothing here assumes how many worlds there are or what a world contains.
//! ECS tools take the world alone; GPU tools take the entity handles they need
//! and resolve them through the public API, reporting a readable error when a
//! handle does not name what they asked for. Every result is a struct from the
//! original crates — [WorldInfo](unlit_ecs::WorldInfo),
//! [ResourceGraphInfo] and friends — serialised
//! through its own reflection rather than assembled field by field.

use std::path::Path;

use base64::Engine as _;
use glam::Vec3;
use image::ImageEncoder as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use unlit_ecs::{ArchetypeBuilder, Entity, World};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::readback::{readback_buffer, readback_texture};
use unlit_wgpu::resources::{
    ResId, Resource, ResourceGraph, ResourceGraphInfo, ResourceGraphMaintain,
};
use unlit3d::gltf::UnlitGltf;
use unlit3d::prelude::*;
use unlit3d::reflect;

/// What a finished job produced.
///
/// Most tools answer with structured JSON that the transport serialises
/// through the value's reflection. Image tools answer with a PNG that the
/// transport sends as an MCP image content block, so a client renders the
/// picture instead of decoding JSON.
pub enum Output {
    /// A structured JSON result.
    Json(Value),
    /// An encoded PNG.
    Image(EncodedImage),
}

impl From<Value> for Output {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

/// A PNG the transport sends as an image content block.
pub struct EncodedImage {
    /// The PNG bytes, base64-encoded.
    pub data: String,
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
    /// each world it routes commands to, including ones on other threads.
    pub fn answer_worlds(self, worlds: Vec<WorldId>) {
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
fn to_value<T: facet::Facet<'static>>(value: &T) -> Result<Value, String> {
    let text = facet_json::to_string(value).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// The render context the world holds, found from `handle` or by query.
///
/// `handle` names any one of the entities the context is stored on — its
/// device, queue, graph or capabilities. Without one the world must hold
/// exactly one context: several are ambiguous, and none is an error rather than
/// an assumption.
fn render_context(world: &World, handle: Option<Entity>) -> Result<RenderContext, String> {
    if let Some(entity) = handle {
        return RenderContext::of(world, entity)
            .ok_or_else(|| format!("entity {} carries no render context", entity.to_bits()));
    }
    let mut found: Option<RenderContext> = None;
    for (_, context) in world.query::<&RenderContext>() {
        match found {
            None => found = Some(*context),
            Some(existing) if existing == *context => {}
            Some(_) => {
                return Err(
                    "the world has more than one render context; pass `context`".to_string()
                );
            }
        }
    }
    found.ok_or_else(|| "the world has no render context".to_string())
}

/// The entity carrying the world's renderer, from `handle` or by query.
fn renderer_entity(world: &World, handle: Option<Entity>) -> Result<Entity, String> {
    if let Some(entity) = handle {
        return world
            .has::<Renderer>(entity)
            .then_some(entity)
            .ok_or_else(|| format!("entity {} carries no renderer", entity.to_bits()));
    }
    let mut found: Option<Entity> = None;
    for (entity, _) in world.query::<&Renderer>() {
        if found.is_some() {
            return Err("the world has more than one renderer; pass `renderer`".to_string());
        }
        found = Some(entity);
    }
    found.ok_or_else(|| "the world has no renderer".to_string())
}

/// The entity carrying the world's mesh source, from `handle` or by query.
fn mesh_source_entity(world: &World, handle: Option<Entity>) -> Result<Entity, String> {
    if let Some(entity) = handle {
        let is_mesh_source = world
            .get::<Source>(entity)
            .is_some_and(|source| source.as_ref::<MeshSource>().is_some());
        return is_mesh_source
            .then_some(entity)
            .ok_or_else(|| format!("entity {} carries no mesh source", entity.to_bits()));
    }
    let mut found: Option<Entity> = None;
    for (entity, source) in world.query::<&Source>() {
        if source.as_ref::<MeshSource>().is_some() {
            if found.is_some() {
                return Err("the world has more than one mesh source; pass `source`".to_string());
            }
            found = Some(entity);
        }
    }
    found.ok_or_else(|| "the world has no mesh source".to_string())
}

/// Run `f` with the mesh source `handle` names, or the world's only one.
fn with_mesh_source<R>(
    world: &World,
    handle: Option<Entity>,
    f: impl FnOnce(&mut MeshSource) -> R,
) -> Result<R, String> {
    let entity = mesh_source_entity(world, handle)?;
    let mut source = world
        .get_mut::<Source>(entity)
        .ok_or_else(|| "the mesh source entity is gone".to_string())?;
    let mesh_source = source
        .as_mut::<MeshSource>()
        .ok_or_else(|| "the source entity is not a mesh source".to_string())?;
    Ok(f(mesh_source))
}

/// The device the context names, cloned out of the world.
fn device_of(world: &World, context: RenderContext) -> Result<wgpu::Device, String> {
    world
        .get::<wgpu::Device>(context.device)
        .map(|device| device.clone())
        .ok_or_else(|| "the render context has no device".to_string())
}

/// The queue the context names, cloned out of the world.
fn queue_of(world: &World, context: RenderContext) -> Result<wgpu::Queue, String> {
    world
        .get::<wgpu::Queue>(context.queue)
        .map(|queue| queue.clone())
        .ok_or_else(|| "the render context has no queue".to_string())
}

/// The graph the context names, borrowed from the world.
fn graph_of<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::CellRef<'w, ResourceGraph>, String> {
    world
        .get::<ResourceGraph>(context.graph)
        .ok_or_else(|| "the render context has no resource graph".to_string())
}

/// The graph the context names, mutably borrowed from the world.
fn graph_of_mut<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::CellRefMut<'w, ResourceGraph>, String> {
    world
        .get_mut::<ResourceGraph>(context.graph)
        .ok_or_else(|| "the render context has no resource graph".to_string())
}

/// A spawned entity, as the tools report it.
#[derive(facet::Facet)]
struct SpawnedEntity {
    /// The spawned entity.
    #[facet(opaque, proxy = unlit_ecs::EntityProxy)]
    entity: Entity,
}

/// Whether a despawn removed anything.
#[derive(facet::Facet)]
struct Despawned {
    /// Whether the entity was alive.
    despawned: bool,
}

/// Whether input events were delivered.
#[derive(facet::Facet)]
struct Delivered {
    /// Whether a handler consumed anything.
    delivered: bool,
}

/// The entities a glTF scene spawned.
#[derive(facet::Facet)]
struct SpawnedEntities {
    /// The spawned entities.
    #[facet(opaque, proxy = unlit_ecs::EntityVecProxy)]
    entities: Vec<Entity>,
}

/// The bytes read out of a buffer.
#[derive(facet::Facet)]
struct BufferRead {
    /// The offset the read started at.
    offset: u64,
    /// How many bytes were read.
    size: usize,
    /// The bytes, base64-encoded.
    data: String,
}

/// A high-level description of the world.
pub fn world_summary(world: &World) -> Result<Value, String> {
    to_value(&world.info())
}

/// The component names the reflection table holds.
pub fn list_components(_world: &World) -> Result<Value, String> {
    to_value(&reflect::names().collect::<Vec<_>>())
}

/// Every archetype with the component names it stores.
pub fn list_archetypes(world: &World) -> Result<Value, String> {
    to_value(&world.archetype_infos())
}

/// Every spawned entity with its component names.
pub fn list_entities(world: &World, limit: usize) -> Result<Value, String> {
    to_value(&world.entity_infos(limit))
}

/// One entity's component names and location.
pub fn get_entity(world: &World, entity: Entity) -> Result<Value, String> {
    let info = world
        .entity_info(entity)
        .ok_or_else(|| format!("no entity with bits {}", entity.to_bits()))?;
    to_value(&info)
}

/// One component of one entity, encoded.
pub fn get_component(world: &World, entity: Entity, name: &str) -> Result<Value, String> {
    let text = reflect::encode(world, entity, name)?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Overwrite one component of one entity.
pub fn set_component(
    world: &World,
    entity: Entity,
    name: &str,
    value: &Value,
) -> Result<Value, String> {
    let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
    reflect::set(world, entity, name, &patch)?;
    Ok(Value::Null)
}

/// Spawn an entity from a map of component name to value.
pub fn spawn_entity(world: &mut World, components: &Value) -> Result<Value, String> {
    let object = components
        .as_object()
        .ok_or_else(|| "components must be an object".to_string())?;
    let mut builder = ArchetypeBuilder::new();
    for (name, value) in object {
        let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
        reflect::push(&mut builder, name, &patch)?;
    }
    let entity = world.spawn(builder);
    to_value(&SpawnedEntity { entity })
}

/// Despawn one entity.
pub fn despawn_entity(world: &mut World, entity: Entity) -> Result<Value, String> {
    let despawned = world.despawn(entity);
    to_value(&Despawned { despawned })
}

/// A summary of the resource graph.
pub fn graph_summary(world: &World, context: Option<Entity>) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let graph = graph_of(world, context)?;
    let info: ResourceGraphInfo = graph.info();
    to_value(&info)
}

/// Every resource slot in the graph.
pub fn list_resources(world: &World, context: Option<Entity>) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let graph = graph_of(world, context)?;
    to_value(&graph.resource_infos())
}

/// The resources a resource depends on, as handles.
pub fn resource_dependencies(
    world: &World,
    context: Option<Entity>,
    id: ResId,
) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let graph = graph_of(world, context)?;
    let handle = graph
        .resolve(id)
        .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
    to_value(&graph.dependency_ids(&handle))
}

/// Drop unreferenced resources and rebuild dirty ones.
pub fn graph_maintain(world: &World, context: Option<Entity>) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let mut graph = graph_of_mut(world, context)?;
    let before = graph.len();
    graph.maintain();
    let after = graph.len();
    drop(graph);
    to_value(&ResourceGraphMaintain { before, after })
}

/// Read bytes out of a buffer resource.
pub fn read_buffer(
    world: &World,
    context: Option<Entity>,
    id: ResId,
    offset: u64,
    size: Option<u64>,
) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let buffer = {
        let graph = graph_of(world, context)?;
        let handle = graph
            .resolve(id)
            .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
        match graph.get::<Resource>(&handle) {
            Some(Resource::Buffer(buffer)) => buffer.clone(),
            _ => return Err(format!("resource id {} is not a buffer", id.to_bits())),
        }
    };
    if !buffer.usage().contains(wgpu::BufferUsages::COPY_SRC) {
        return Err(format!(
            "the buffer behind id {} was not created with COPY_SRC, so it cannot be read back",
            id.to_bits()
        ));
    }
    let total = buffer.size();
    if offset > total {
        return Err(format!(
            "offset {offset} is past the buffer's {total} bytes"
        ));
    }
    let size = size.unwrap_or(total - offset).min(total - offset);
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = readback_buffer(&device, &queue, &buffer, offset, size);
    to_value(&BufferRead {
        offset,
        size: bytes.len(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Read a texture resource and encode it as a PNG.
pub fn read_texture_as_image(
    world: &World,
    context: Option<Entity>,
    id: ResId,
) -> Result<Output, String> {
    let context = render_context(world, context)?;
    let texture = {
        let graph = graph_of(world, context)?;
        let handle = graph
            .resolve(id)
            .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
        match graph.get::<Resource>(&handle) {
            Some(Resource::Texture(texture)) => texture.clone(),
            Some(Resource::TextureView(view)) => view.texture().clone(),
            _ => return Err(format!("resource id {} is not a texture", id.to_bits())),
        }
    };
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = readback_texture(&device, &queue, &texture);
    let png = encode_png(&bytes, texture.width(), texture.height(), texture.format())?;
    image_value(&png, texture.width(), texture.height())
}

/// The input state `handle` names, or the world's only one.
fn input_state_entity(world: &World, handle: Option<Entity>) -> Result<Entity, String> {
    if let Some(entity) = handle {
        return world
            .has::<InputHandle>(entity)
            .then_some(entity)
            .ok_or_else(|| format!("entity {} carries no input state", entity.to_bits()));
    }
    let mut found: Option<Entity> = None;
    for (entity, _) in world.query::<&InputHandle>() {
        if found.is_some() {
            return Err("the world has more than one input state; pass `entity`".to_string());
        }
        found = Some(entity);
    }
    found.ok_or_else(|| "the world has no input state".to_string())
}

/// The input state, encoded, events included.
pub fn input_state(world: &World, entity: Option<Entity>) -> Result<Value, String> {
    let entity = input_state_entity(world, entity)?;
    let text = reflect::encode(world, entity, "InputHandle")?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Push input events into the world and deliver them.
pub fn send_input(world: &World, entity: Option<Entity>, events: &Value) -> Result<Value, String> {
    let text = serde_json::to_string(events).map_err(|error| error.to_string())?;
    let decoded = reflect::decode_events(&text)?;
    let entity = input_state_entity(world, entity)?;
    let _ = world.with_mut::<InputHandle, _>(entity, |handle| {
        let mut state = handle.write();
        for event in decoded {
            state.push(event);
        }
    });
    let delivered = dispatch_input(world);
    to_value(&Delivered { delivered })
}

/// Allocate a mesh and spawn an entity that draws it.
pub fn create_mesh(
    world: &mut World,
    source: Option<Entity>,
    args: crate::server::CreateMesh,
) -> Result<Value, String> {
    if args.positions.is_empty() {
        return Err("positions must not be empty".to_string());
    }
    let desc = UnlitMeshDesc {
        positions: &args.positions,
        uvs: args.uvs.as_deref(),
        colors: args.colors.as_deref(),
        indices: args.indices.as_deref(),
        joints: None,
        weights: None,
        morph_deltas: None,
    };
    let (key, mesh) = with_mesh_source(world, source, |source| {
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&source.device(world)));
        let mesh = source.allocate_unlit_mesh(world, &key, desc);
        (key, mesh)
    })?;
    let transform = match args.transform {
        Some(transform) => Transform {
            translation: Vec3::from_array(transform.translation),
            rotation: glam::Quat::from_xyzw(
                transform.rotation[0],
                transform.rotation[1],
                transform.rotation[2],
                transform.rotation[3],
            ),
            scale: Vec3::from_array(transform.scale),
        },
        None => Transform::default(),
    };
    let color = args.color.map_or(glam::Vec4::ONE, glam::Vec4::from_array);
    let mut builder = ArchetypeBuilder::new();
    builder.push(transform);
    builder.push(mesh);
    builder.push(UnlitPipeline::new(key));
    builder.push(InstanceColor::new(color));
    if let Some(cutoff) = args.cutoff {
        builder.push(InstanceCutoff::new(cutoff));
    }
    let entity = world.spawn(builder);
    to_value(&SpawnedEntity { entity })
}

/// Release a mesh and despawn the entity that drew it.
pub fn remove_mesh(
    world: &mut World,
    source: Option<Entity>,
    entity: Entity,
) -> Result<Value, String> {
    let mesh = world
        .get::<GpuMesh>(entity)
        .map(|mesh| mesh.clone())
        .ok_or_else(|| format!("entity {} has no GpuMesh", entity.to_bits()))?;
    with_mesh_source(world, source, |source| source.remove_mesh(mesh))?;
    let despawned = world.despawn(entity);
    to_value(&Despawned { despawned })
}

/// Render one frame from the world.
pub fn render_frame(world: &mut World, renderer: Option<Entity>) -> Result<Value, String> {
    let entity = renderer_entity(world, renderer)?;
    let world_ref: &World = &*world;
    let _ = world_ref.with_mut::<Renderer, _>(entity, |renderer| renderer.render(world_ref));
    Ok(Value::Null)
}

/// The offscreen texture a screenshot reads, from `handle` or by query.
fn screenshot_target(
    world: &World,
    context: RenderContext,
    id: Option<ResId>,
) -> Result<wgpu::Texture, String> {
    let graph = graph_of(world, context)?;
    if let Some(id) = id {
        let handle = graph
            .resolve(id)
            .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
        return match graph.get::<Resource>(&handle) {
            Some(Resource::Texture(texture)) => Ok(texture.clone()),
            _ => Err(format!("resource id {} is not a texture", id.to_bits())),
        };
    }
    let mut found: Option<wgpu::Texture> = None;
    for node in graph.nodes() {
        if let Resource::Texture(texture) = node.resource {
            if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
                continue;
            }
            if found.is_some() {
                return Err(
                    "the world has more than one readable texture; pass `target`".to_string(),
                );
            }
            found = Some(texture.clone());
        }
    }
    found
        .ok_or_else(|| "the world has no readable texture to screenshot; pass `target`".to_string())
}

/// Render one frame and read the offscreen target back as a PNG.
pub fn screenshot(
    world: &mut World,
    renderer: Option<Entity>,
    target: Option<ResId>,
) -> Result<Output, String> {
    let context = render_context(world, None)?;
    let texture = screenshot_target(world, context, target)?;
    render_frame(world, renderer)?;
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = readback_texture(&device, &queue, &texture);
    let png = encode_png(&bytes, texture.width(), texture.height(), texture.format())?;
    image_value(&png, texture.width(), texture.height())
}

/// Load a glTF file and spawn its default scene.
pub fn load_gltf(world: &mut World, source: Option<Entity>, path: &str) -> Result<Value, String> {
    let gltf = UnlitGltf::load(Path::new(path)).map_err(|error| error.to_string())?;
    let resources = with_mesh_source(world, source, |source| gltf.insert_resources(source, world))?;
    let nodes = gltf.spawn_default_scene(world, &resources);
    let entities: Vec<Entity> = nodes
        .iter()
        .flat_map(|node| node.entities.iter())
        .copied()
        .collect();
    to_value(&SpawnedEntities { entities })
}

/// An image's size, as structured output.
#[derive(facet::Facet)]
struct ImageSize {
    /// The image's width in pixels.
    width: u32,
    /// The image's height in pixels.
    height: u32,
}

/// A PNG result the transport sends as an image content block.
fn image_value(png: &[u8], width: u32, height: u32) -> Result<Output, String> {
    Ok(Output::Image(EncodedImage {
        data: base64::engine::general_purpose::STANDARD.encode(png),
        structured: to_value(&ImageSize { width, height })?,
    }))
}

/// Encode raw RGBA bytes as a PNG.
fn encode_png(
    bytes: &[u8],
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Result<Vec<u8>, String> {
    let color = match format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
            image::ExtendedColorType::Rgba8
        }
        other => return Err(format!("cannot encode a {other:?} texture as a PNG")),
    };
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(bytes, width, height, color)
        .map_err(|error| error.to_string())?;
    Ok(png)
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
        command.answer_worlds(vec![WorldId::Host, WorldId::Handle(3)]);
        let answer = receiver.recv_blocking().unwrap().unwrap();
        let Output::Json(value) = answer else {
            panic!("list_worlds answers with JSON");
        };
        assert_eq!(value, serde_json::json!(["Host", {"Handle": 3}]));
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
}
