//! The render-thread side of the MCP server.
//!
//! A [World] is not [Send], and the GPU work that reads it must happen on the
//! thread that owns it. The MCP transport, on the other hand, runs on a tokio
//! thread. The two meet at a command channel: a tool builds a [Command] holding
//! a closure and the handle of the world it should run against, the thread that
//! owns that world resolves the handle and calls [dispatch], and the JSON
//! result travels back on the command's reply channel.
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
use serde_json::Value;
use unlit_ecs::{ArchetypeBuilder, Entity, World};
use unlit_wgpu::capabilities::{DeviceCapabilities, DeviceTier};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::readback::{readback_buffer, readback_texture};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{
    ResId, Resource, ResourceGraph, ResourceGraphInfo, ResourceGraphMaintain, TextureExt,
};
use unlit3d::gltf::UnlitGltf;
use unlit3d::prelude::*;
use unlit3d::reflect;

/// The format of the offscreen target the standalone host renders into.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A unit of work for the thread that owns a world.
///
/// The closure captures plain data (tool arguments) and runs with exclusive
/// access to one world. It must be [Send] because it travels across the
/// channel; the world it touches never does.
pub type Job = Box<dyn FnOnce(&mut World) -> Result<Value, String> + Send + 'static>;

/// A job together with the channel its result is sent back on.
pub struct Command {
    job: Job,
    reply: async_channel::Sender<Result<Value, String>>,
    /// The host entity whose world the job runs against; `None` is the world
    /// that receives the command itself.
    world: Option<Entity>,
}

impl Command {
    /// Build a command and the receiver its result will arrive on.
    pub fn new(
        job: impl FnOnce(&mut World) -> Result<Value, String> + Send + 'static,
    ) -> (Self, async_channel::Receiver<Result<Value, String>>) {
        let (reply, receiver) = async_channel::bounded(1);
        (
            Self {
                job: Box::new(job),
                reply,
                world: None,
            },
            receiver,
        )
    }

    /// Direct the command at the world carried by `entity`.
    #[must_use]
    pub fn world(mut self, entity: Entity) -> Self {
        self.world = Some(entity);
        self
    }

    /// The host entity whose world the command asks for, or `None` for the
    /// world the host itself dispatches into.
    #[must_use]
    pub fn world_handle(&self) -> Option<Entity> {
        self.world
    }

    /// Answer the command with an error without running it.
    ///
    /// The host calls this when the world handle cannot be resolved, so the
    /// tool call still gets a readable message rather than a dropped channel.
    pub fn fail(self, message: String) {
        let _ = self.reply.send_blocking(Err(message));
    }
}

/// Run a command against `world` and send its result back.
///
/// The send is best-effort: a tool call that has already been cancelled has no
/// receiver left, and the result is simply dropped.
pub fn dispatch(command: Command, world: &mut World) {
    let result = (command.job)(world);
    let _ = command.reply.send_blocking(result);
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

/// An encoded image.
#[derive(facet::Facet)]
struct ImageValue {
    /// The image's media type.
    mime_type: &'static str,
    /// The image's width in pixels.
    width: u32,
    /// The image's height in pixels.
    height: u32,
    /// The image bytes, base64-encoded.
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
) -> Result<Value, String> {
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
) -> Result<Value, String> {
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

/// Build an offscreen world for a host with no window.
///
/// The world holds a render context, a renderer, a mesh source, an input state
/// and a default camera, and an offscreen color target registered in the
/// graph — both as a texture a screenshot can read back and as the view the
/// renderer draws into. Returns the world and the renderer entity.
#[must_use]
pub fn offscreen_world(
    device: wgpu::Device,
    queue: wgpu::Queue,
    capabilities: DeviceCapabilities,
    size: (u32, u32),
    samples: u32,
    depth: bool,
) -> (World, Entity) {
    let mut world = World::new();
    let context = spawn_context(
        &mut world,
        device.clone(),
        queue.clone(),
        ResourceGraph::new(),
        capabilities,
    );
    let renderer = world.spawn((Renderer::new(context),));
    world.spawn((RenderLoadOps::default(),));
    let mut source = MeshSource::new(&world, context);
    source.register_unlit_family(&world);
    world.spawn_source(source);
    world.spawn((InputHandle::new(),));

    let color = create_color_target(&device, COLOR_FORMAT, size.0, size.1);
    let color_view = {
        let mut graph = world
            .get_mut::<ResourceGraph>(context.graph)
            .expect("spawn_context spawned the graph");
        // The texture is registered in its own right so a screenshot can name
        // it, and the view depends on it so the graph keeps it alive.
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments = FrameAttachments::new(&world, context, COLOR_FORMAT, size, samples, depth);
    let _ = world.with_mut::<Renderer, _>(renderer, |renderer| {
        attachments.bind(&world, renderer, color_view);
    });
    world.spawn((default_camera(size),));
    (world, renderer)
}

/// Ask the backend for a device and a queue.
///
/// The standalone binary and the CLI's windowless `--mcp` path both build
/// their own world from this; the crate itself never owns one.
pub async fn request_device(
    label: &str,
) -> Result<(wgpu::Device, wgpu::Queue, DeviceCapabilities), String> {
    let tier = DeviceTier::from_env();
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: None,
            ..Default::default()
        })
        .await
        .map_err(|error| error.to_string())?;
    let capabilities = tier.capabilities_of(&adapter);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some(label),
            required_features: wgpu::Features::empty(),
            required_limits: tier.limits(&adapter.limits()),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok((device, queue, capabilities))
}

/// The camera the standalone host starts with.
fn default_camera(size: (u32, u32)) -> Camera {
    let aspect = size.0.max(1) as f32 / size.1.max(1) as f32;
    let view_from_world =
        glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 1.0, 3.0), Vec3::ZERO, Vec3::Y);
    let clip_from_view = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60.0f32.to_radians(),
        aspect,
        0.1,
    );
    Camera {
        view_from_world,
        clip_from_view,
        active: true,
    }
}

/// An image result as JSON.
fn image_value(png: &[u8], width: u32, height: u32) -> Result<Value, String> {
    to_value(&ImageValue {
        mime_type: "image/png",
        width,
        height,
        data: base64::engine::general_purpose::STANDARD.encode(png),
    })
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
