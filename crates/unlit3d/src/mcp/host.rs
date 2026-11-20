//! The rendering tools: the resource graph, buffers, textures, input, meshes
//! and frames.
//!
//! A tool names the entity handles it needs and this module resolves them
//! through the public API, reporting a readable error when a handle does not
//! name what the tool asked for. The tools themselves live one level up, in
//! [crate::mcp].

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use image::ImageEncoder as _;

use unlit_ecs::mcp::host::{SavedFile, extension_of, saved_name, to_value, write_file};
use unlit_ecs::mcp::serde_json::{self, Value};
use unlit_ecs::mcp::{EncodedImage, Output};
use unlit_ecs::{Entity, Scope, World};
use unlit_wgpu::readback::{try_readback_buffer, try_readback_texture};
use unlit_wgpu::resources::{
    ResId, Resource, ResourceGraph, ResourceGraphInfo, ResourceGraphMaintain,
};

use crate::gltf::LitGltf;
use crate::prelude::*;
use crate::reflect;

/// The render context the world holds, found from `handle` or by query.
///
/// `handle` names any one of the entities the context is stored on — its
/// device, queue, graph or capabilities. Without one the world must hold
/// exactly one context: several are ambiguous, and none is an error rather than
/// an assumption.
fn render_context(world: &World, handle: Option<Entity>) -> Result<RenderContext, String> {
    if let Some(entity) = handle {
        return RenderContext::of(world, entity)
            .map_err(|_| format!("entity {entity:?} carries no render context"));
    }
    let mut found: Option<RenderContext> = None;
    for render_context in world.root_scope().query::<&RenderContext>().iter() {
        let context = *render_context;
        match found {
            None => found = Some(context),
            Some(existing) if existing == context => {}
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
            .root_scope()
            .try_query_one::<Has<Renderer>>(entity)
            .is_ok_and(|mut holder| holder.get())
            .then_some(entity)
            .ok_or_else(|| format!("entity {entity:?} carries no renderer"));
    }
    let mut found: Option<Entity> = None;
    for (_renderer, entity) in world.root_scope().query::<(&Renderer, Entity)>().iter() {
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
            .root_scope()
            .try_query_one::<&Source>(entity)
            .is_ok_and(|mut source| source.get().as_ref::<MeshSource>().is_ok());
        return is_mesh_source
            .then_some(entity)
            .ok_or_else(|| format!("entity {entity:?} carries no mesh source"));
    }
    let mut found: Option<Entity> = None;
    for (source, entity) in world.root_scope().query::<(&Source, Entity)>().iter() {
        if source.as_ref::<MeshSource>().is_ok() {
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
    f: impl FnOnce(&mut MeshSource, &Scope<'_>) -> R,
) -> Result<R, String> {
    let entity = mesh_source_entity(world, handle)?;
    // The scope is the source's own: a source entity is its own scope, and a
    // source only ever sees the subtree it draws.
    let scope = world
        .try_scope(entity)
        .map_err(|_| "the mesh source entity is gone".to_string())?;
    let mut source = scope
        .try_query_one::<&mut Source>(entity)
        .map_err(|_| "the mesh source entity is gone".to_string())?;
    let mesh_source = source
        .get()
        .as_mut::<MeshSource>()
        .map_err(|error| error.to_string())?;
    Ok(f(mesh_source, &scope))
}

/// The device the context names, cloned out of the world.
fn device_of(world: &World, context: RenderContext) -> Result<wgpu::Device, String> {
    context
        .device(&world.root_scope())
        .map_err(|_| "the render context has no device".to_string())
}

/// The queue the context names, cloned out of the world.
fn queue_of(world: &World, context: RenderContext) -> Result<wgpu::Queue, String> {
    context
        .queue(&world.root_scope())
        .map_err(|_| "the render context has no queue".to_string())
}

/// The graph the context names, borrowed from the world.
fn graph_of<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::QueryOne<'w, &'w ResourceGraph>, String> {
    world
        .root_scope()
        .try_query_one::<&ResourceGraph>(context.graph)
        .map_err(|_| "the render context has no resource graph".to_string())
}

/// The graph the context names, mutably borrowed from the world.
fn graph_of_mut<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::QueryOne<'w, &'w mut ResourceGraph>, String> {
    world
        .root_scope()
        .try_query_one::<&mut ResourceGraph>(context.graph)
        .map_err(|_| "the render context has no resource graph".to_string())
}

/// Whether input events were delivered.
#[derive(facet::Facet)]
struct Delivered {
    /// Whether the world had events to dispatch. This is the batch's own size,
    /// not a count of the handlers that ran: an event nothing watches still
    /// counts, because the world did receive it.
    delivered: bool,
}

/// The entities a glTF scene spawned.
#[derive(facet::Facet)]
struct SpawnedEntities {
    /// The spawned entities, as bare bits.
    #[facet(opaque, proxy = unlit_ecs::EntityVecProxy)]
    entities: Vec<Entity>,
    /// The names of those entities, in the same order.
    names: Vec<&'static str>,
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

pub(crate) fn graph_summary(world: &World, context: Option<Entity>) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    let info: ResourceGraphInfo = graph.info();
    to_value(&info)
}

/// Every resource slot in the graph.
pub(crate) fn list_resources(world: &World, context: Option<Entity>) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    to_value(&graph.resource_infos())
}

/// The resources a resource depends on, as handles.
pub(crate) fn resource_dependencies(
    world: &World,
    context: Option<Entity>,
    id: ResId,
) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    let handle = graph
        .resolve(id)
        .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
    let ids = graph
        .dependencies(&handle)
        .map(|dependency| dependency.id())
        .collect::<Vec<_>>();
    to_value(&ids)
}

/// Drop unreferenced resources and rebuild dirty ones.
pub(crate) fn graph_maintain(world: &World, context: Option<Entity>) -> Result<Value, String> {
    let context = render_context(world, context)?;
    let mut graph_row = graph_of_mut(world, context)?;
    let graph = graph_row.get();
    let before = graph.len();
    graph.maintain();
    let after = graph.len();
    drop(graph_row);
    to_value(&ResourceGraphMaintain { before, after })
}

/// The bytes a buffer read produced, with the offset it started at.
fn read_buffer_bytes(
    world: &World,
    context: Option<Entity>,
    id: ResId,
    offset: u64,
    size: Option<u64>,
) -> Result<(u64, Vec<u8>), String> {
    let context = render_context(world, context)?;
    let buffer = {
        let mut graph_row = graph_of(world, context)?;
        let graph = graph_row.get();
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
    // A copy reads whole aligned runs: wgpu rejects anything else outright, and
    // a caller that named the edges of a field cannot know the alignment, so
    // the misalignment is reported here instead of as a copy wgpu refused.
    let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
    if !offset.is_multiple_of(alignment) {
        return Err(format!(
            "offset {offset} is not a multiple of the {alignment} bytes a buffer copy reads in"
        ));
    }
    let size = size.unwrap_or(total - offset).min(total - offset);
    if !size.is_multiple_of(alignment) {
        return Err(format!(
            "size {size} is not a multiple of the {alignment} bytes a buffer copy reads in"
        ));
    }
    if size == 0 {
        // Nothing to copy, and a staging buffer of no bytes is not a buffer
        // wgpu will create.
        return Ok((offset, Vec::new()));
    }
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    // The tool runs inside a synchronous closure on the world's thread, so the
    // readback's future is driven to completion here rather than awaited.
    let bytes = pollster::block_on(try_readback_buffer(&device, &queue, &buffer, offset, size))
        .map_err(|error| format!("failed to read the buffer back: {error}"))?;
    Ok((offset, bytes))
}

/// Read bytes out of a buffer resource.
pub(crate) fn read_buffer(
    world: &World,
    context: Option<Entity>,
    id: ResId,
    offset: u64,
    size: Option<u64>,
) -> Result<Value, String> {
    let (offset, bytes) = read_buffer_bytes(world, context, id, offset, size)?;
    to_value(&BufferRead {
        offset,
        size: bytes.len(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Read bytes out of a buffer resource and write them to a file.
pub(crate) fn save_buffer(
    world: &World,
    context: Option<Entity>,
    id: ResId,
    offset: u64,
    size: Option<u64>,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, String> {
    let (_, bytes) = read_buffer_bytes(world, context, id, offset, size)?;
    let file_name = saved_name(file_name, "buffer", "bin");
    to_value(&write_file(directory, &file_name, &bytes)?)
}

struct Frame {
    /// The pixel bytes.
    bytes: Vec<u8>,
    /// The frame's width in pixels.
    width: u32,
    /// The frame's height in pixels.
    height: u32,
    /// The format the bytes are in.
    format: wgpu::TextureFormat,
}

impl Frame {
    /// The color type an encoder reads these bytes as.
    fn color(&self) -> Result<image::ExtendedColorType, String> {
        match self.format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
                Ok(image::ExtendedColorType::Rgba8)
            }
            other => Err(format!("cannot encode a {other:?} texture")),
        }
    }

    /// The frame's pixels with their alpha channel dropped, for an encoder
    /// that takes no alpha.
    fn rgb8(&self) -> Result<Vec<u8>, String> {
        self.color()?;
        let mut rgb = Vec::with_capacity(self.bytes.len() / 4 * 3);
        for pixel in self.bytes.as_chunks::<4>().0 {
            rgb.extend_from_slice(&pixel[..3]);
        }
        Ok(rgb)
    }
}

/// The image format an image tool writes or sends, named by a file's
/// extension or by a format argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFormat {
    /// PNG.
    Png,
    /// Lossless WebP.
    Webp,
    /// JPEG, which is lossy and carries no alpha channel.
    Jpeg,
}

impl ImageFormat {
    /// The format an image tool uses when it is asked for none.
    const DEFAULT: Self = Self::Webp;

    /// The format `name` names, or an error when it names none this server
    /// can write.
    fn from_name(name: &str) -> Result<Self, String> {
        match name.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::Webp),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            other => Err(format!(
                "cannot write a {other:?} image; use .png, .webp, .jpg or .jpeg"
            )),
        }
    }

    /// The format the file named `file_name` asks for.
    fn of(file_name: &str) -> Result<Self, String> {
        extension_of(file_name).map_or_else(
            || Err(format!("the file name {file_name:?} has no extension")),
            Self::from_name,
        )
    }

    /// The MIME type an image content block carries.
    fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Jpeg => "image/jpeg",
        }
    }

    /// Encode a frame read back from a texture.
    fn encode(self, frame: &Frame) -> Result<Vec<u8>, String> {
        let color = frame.color()?;
        let mut bytes = Vec::new();
        match self {
            Self::Png => image::codecs::png::PngEncoder::new(&mut bytes).write_image(
                &frame.bytes,
                frame.width,
                frame.height,
                color,
            ),
            Self::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut bytes).write_image(
                &frame.bytes,
                frame.width,
                frame.height,
                color,
            ),
            Self::Jpeg => image::codecs::jpeg::JpegEncoder::new(&mut bytes).write_image(
                &frame.rgb8()?,
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgb8,
            ),
        }
        .map_err(|error| error.to_string())?;
        Ok(bytes)
    }
}

/// Encode a frame into `directory`, as the image format `file_name` names.
fn save_image(directory: &str, file_name: &str, frame: &Frame) -> Result<SavedFile, String> {
    let bytes = ImageFormat::of(file_name)?.encode(frame)?;
    write_file(directory, file_name, &bytes)
}

/// The frame a texture read produced.
fn read_texture_frame(world: &World, context: Option<Entity>, id: ResId) -> Result<Frame, String> {
    let context = render_context(world, context)?;
    let texture = {
        let mut graph_row = graph_of(world, context)?;
        let graph = graph_row.get();
        let handle = graph
            .resolve(id)
            .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
        match graph.get::<Resource>(&handle) {
            Some(Resource::Texture(texture)) => texture.clone(),
            Some(Resource::TextureView(view)) => view.texture().clone(),
            _ => return Err(format!("resource id {} is not a texture", id.to_bits())),
        }
    };
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return Err(format!(
            "the texture behind id {} was not created with COPY_SRC, so it cannot be read back",
            id.to_bits()
        ));
    }
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = pollster::block_on(try_readback_texture(&device, &queue, &texture))
        .map_err(|error| format!("failed to read the texture back: {error}"))?;
    Ok(Frame {
        bytes,
        width: texture.width(),
        height: texture.height(),
        format: texture.format(),
    })
}

/// Read a texture resource and encode it as an inline image.
pub(crate) fn read_texture_as_image(
    world: &World,
    context: Option<Entity>,
    id: ResId,
    format: Option<&str>,
) -> Result<Output, String> {
    let format = match format {
        Some(name) => ImageFormat::from_name(name)?,
        None => ImageFormat::DEFAULT,
    };
    let frame = read_texture_frame(world, context, id)?;
    image_value(&format.encode(&frame)?, frame.width, frame.height, format)
}

/// Read a texture resource and write it to a file as an image.
pub(crate) fn save_texture_as_image(
    world: &World,
    context: Option<Entity>,
    id: ResId,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, String> {
    let frame = read_texture_frame(world, context, id)?;
    let file_name = saved_name(file_name, "texture", "webp");
    to_value(&save_image(directory, &file_name, &frame)?)
}

/// The entity carrying `T` that `handle` names, or the world's only one.
fn single_entity<T: 'static>(
    world: &World,
    handle: Option<Entity>,
    what: &str,
) -> Result<Entity, String> {
    if let Some(entity) = handle {
        return world
            .root_scope()
            .try_query_one::<Has<T>>(entity)
            .is_ok_and(|mut holder| holder.get())
            .then_some(entity)
            .ok_or_else(|| format!("entity {entity:?} carries no {what}"));
    }
    let mut found: Option<Entity> = None;
    for (_t, entity) in world.root_scope().query::<(&T, Entity)>().iter() {
        if found.is_some() {
            return Err(format!("the world has more than one {what}; pass `entity`"));
        }
        found = Some(entity);
    }
    found.ok_or_else(|| format!("the world has no {what}"))
}

/// A world's input state, encoded: how its devices are.
pub(crate) fn input_state(world: &World, entity: Option<Entity>) -> Result<Value, String> {
    let entity = single_entity::<InputState>(world, entity, "input state")?;
    let text = reflect::encode_json(&world.root_scope(), entity, "InputState")
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Feed input events to the world and deliver them.
///
/// The events go to the world's [`InputWriter`], which reaches every world
/// subscribed to it; the dispatch that follows is this world's own tick.
pub(crate) fn send_input(
    world: &World,
    entity: Option<Entity>,
    events: &Value,
) -> Result<Value, String> {
    let text = serde_json::to_string(events).map_err(|error| error.to_string())?;
    let decoded = reflect::decode_events(&text).map_err(|error| error.to_string())?;
    let entity = single_entity::<InputWriter>(world, entity, "input writer")?;
    world
        .root_scope()
        .try_query_one::<&mut InputWriter>(entity)
        .map(|mut writer| {
            let writer = writer.get();
            writer.push_all(decoded)
        })
        .map_err(|error| format!("entity {entity:?} carries no input writer: {error}"))?;
    let delivered = dispatch_input(world);
    to_value(&Delivered { delivered })
}

/// Render one frame from the world.
pub(crate) fn render_frame(world: &mut World, renderer: Option<Entity>) -> Result<Value, String> {
    let entity = renderer_entity(world, renderer)?;
    let world_ref: &World = &*world;
    world_ref
        .root_scope()
        .try_query_one::<&mut Renderer>(entity)
        .map(|mut renderer| renderer.get().render(world_ref))
        .map_err(|error| format!("entity {entity:?} carries no renderer: {error}"))?;
    Ok(Value::Null)
}

/// The offscreen texture a screenshot reads, from `handle` or by query.
///
/// `id` may name the texture or any view of it, since a caller that draws
/// into an offscreen target knows the view its pass binds.
fn screenshot_target(
    world: &World,
    context: RenderContext,
    id: Option<ResId>,
) -> Result<wgpu::Texture, String> {
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    if let Some(id) = id {
        let handle = graph
            .resolve(id)
            .ok_or_else(|| format!("stale resource id {}", id.to_bits()))?;
        let texture = match graph.get::<Resource>(&handle) {
            Some(Resource::Texture(texture)) => texture.clone(),
            Some(Resource::TextureView(view)) => view.texture().clone(),
            _ => return Err(format!("resource id {} is not a texture", id.to_bits())),
        };
        if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
            return Err(format!(
                "resource id {} is a texture no one can read back: it lacks COPY_SRC",
                id.to_bits()
            ));
        }
        return Ok(texture);
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

/// Render one frame and read the offscreen target back.
fn screenshot_frame(
    world: &mut World,
    context: Option<Entity>,
    renderer: Option<Entity>,
    target: Option<ResId>,
) -> Result<Frame, String> {
    let context = render_context(world, context)?;
    let texture = screenshot_target(world, context, target)?;
    render_frame(world, renderer)?;
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = pollster::block_on(try_readback_texture(&device, &queue, &texture))
        .map_err(|error| format!("failed to read the texture back: {error}"))?;
    Ok(Frame {
        bytes,
        width: texture.width(),
        height: texture.height(),
        format: texture.format(),
    })
}

/// Render one frame and read the offscreen target back as an inline image.
pub(crate) fn read_screenshot(
    world: &mut World,
    context: Option<Entity>,
    renderer: Option<Entity>,
    target: Option<ResId>,
    format: Option<&str>,
) -> Result<Output, String> {
    let format = match format {
        Some(name) => ImageFormat::from_name(name)?,
        None => ImageFormat::DEFAULT,
    };
    let frame = screenshot_frame(world, context, renderer, target)?;
    image_value(&format.encode(&frame)?, frame.width, frame.height, format)
}

/// Render one frame and write the offscreen target to a file as an image.
pub(crate) fn save_screenshot(
    world: &mut World,
    context: Option<Entity>,
    renderer: Option<Entity>,
    target: Option<ResId>,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, String> {
    let frame = screenshot_frame(world, context, renderer, target)?;
    let file_name = saved_name(file_name, "screenshot", "webp");
    to_value(&save_image(directory, &file_name, &frame)?)
}

/// Spawn the default scene of a glTF document read from a file or inline.
///
/// Exactly one of `path` and `document` must be given. A document is either
/// glTF JSON text or a GLB whose bytes are base64-encoded, since a JSON tool
/// argument cannot carry raw binary.
pub(crate) fn spawn_gltf(
    world: &mut World,
    source: Option<Entity>,
    scope: Option<Entity>,
    path: Option<&str>,
    document: Option<&str>,
) -> Result<Value, String> {
    let gltf = match (path, document) {
        (Some(path), None) => LitGltf::load(Path::new(path)),
        (None, Some(document)) => LitGltf::from_bytes(&gltf_document_bytes(document)?),
        (Some(_), Some(_)) => {
            return Err("pass either `path` or `document`, not both".to_string());
        }
        (None, None) => return Err("pass one of `path` and `document`".to_string()),
    }
    .map_err(|error| error.to_string())?;
    // Content goes into the requested scope, which defaults to the source
    // entity itself, so the frame that draws the source also draws the scene.
    let scope = match scope {
        Some(scope) => scope,
        None => mesh_source_entity(world, source)?,
    };
    // Asked before anything uploads: a scope that names nothing is answered
    // with its own error rather than with a document's worth of resources the
    // caller never gets a handle to.
    world
        .try_scope(scope)
        .map_err(|_| format!("no scope {scope:?}"))?;
    let resources = with_mesh_source(world, source, |source, scope| {
        gltf.insert_resources(source, scope)
    })?;
    let mut scope = world
        .try_scope_mut(scope)
        .map_err(|_| format!("no scope {scope:?}"))?;
    let nodes = gltf
        .spawn_default_scene(&mut scope, &resources)
        .map_err(|error| error.to_string())?;
    let entities: Vec<Entity> = nodes
        .iter()
        .flat_map(|node| {
            let pose = node.pose.iter();
            let weights = node.weights.iter();
            node.entities.iter().chain(pose).chain(weights)
        })
        .copied()
        .collect();
    let names = entities.iter().map(|entity| entity.name()).collect();
    to_value(&SpawnedEntities { entities, names })
}

/// The bytes of an inline glTF document: JSON text as written, or a GLB's
/// bytes decoded from base64.
fn gltf_document_bytes(document: &str) -> Result<Vec<u8>, String> {
    // A JSON document is the one form a caller can paste; anything else is
    // taken as base64, so the leading brace decides without guessing at the
    // alphabet.
    if document.trim_start().starts_with('{') {
        return Ok(document.as_bytes().to_vec());
    }
    base64::engine::general_purpose::STANDARD
        .decode(document.trim())
        .map_err(|error| format!("the document is not base64: {error}"))
}

/// An image's size, as structured output.
#[derive(facet::Facet)]
struct ImageSize {
    /// The image's width in pixels.
    width: u32,
    /// The image's height in pixels.
    height: u32,
}

/// An encoded image the transport sends as an image content block.
fn image_value(
    bytes: &[u8],
    width: u32,
    height: u32,
    format: ImageFormat,
) -> Result<Output, String> {
    Ok(Output::Image(EncodedImage {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime_type: format.mime_type().to_string(),
        structured: to_value(&ImageSize { width, height })?,
    }))
}

/// Shared frame statistics a monitor behavior feeds and the frame-clock tool
/// reads.
#[derive(Default)]
struct FrameMonitor {
    /// Durations of the most recent frames, front = oldest.
    window: VecDeque<f32>,
}

/// How many recent frame durations the rolling average covers.
const FRAME_WINDOW: usize = 120;

impl FrameMonitor {
    /// Fold one frame's duration into the statistics.
    fn record(&mut self, delta_time: f32) {
        self.window.push_back(delta_time);
        if self.window.len() > FRAME_WINDOW {
            self.window.pop_front();
        }
    }

    /// Mean duration over the rolling window, if any frame was recorded.
    fn average_delta(&self) -> Option<f32> {
        let total: f32 = self.window.iter().sum();
        (!self.window.is_empty()).then(|| total / self.window.len() as f32)
    }
}

/// The statistics the monitor behavior shares with the frame-clock tool.
struct FrameMonitorState {
    monitor: Arc<Mutex<FrameMonitor>>,
}

/// The monitor behavior spawned on the first frame-clock query: it feeds the
/// shared statistics with each frame the world's driver dispatches.
fn frame_monitor(monitor: Arc<Mutex<FrameMonitor>>) -> OnFrame {
    OnFrame::new(move |_world, _entity, frame| {
        // A frame dispatched with a zero duration would drive the average to
        // an infinite fps, so it is left out of the window rather than
        // folded in. The clock still counts it, being what the driver states.
        // A panic elsewhere poisons the lock but not the window it guards: a
        // queue of durations has no invariant a half-finished update breaks, so
        // the monitor keeps the mean rather than losing the whole statistic.
        if frame.delta_time > 0.0 {
            monitor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(frame.delta_time);
        }
    })
}

/// What the frame-clock tools report about one world.
///
/// Every field but the averages is the world's own [`FrameClock`], so a world
/// that never dispatched a frame reports none of them: a client can tell a
/// world with no pacing of its own from one standing at its first frame.
#[derive(Debug, Clone, Copy, PartialEq, facet::Facet)]
pub(crate) struct FrameClockReport {
    /// The world's frame ordinal, or none when it never dispatched a frame.
    index: Option<u32>,
    /// How long the world's last frame took, in seconds.
    delta_time: Option<f32>,
    /// The time the world's frames have covered, in seconds.
    time: Option<f32>,
    /// The rate the last frame ran at, in frames per second.
    fps: Option<f32>,
    /// The average rate over the measured window, or none before the monitor
    /// has seen a frame.
    average_fps: Option<f32>,
    /// How many frames the average covers.
    average_window_frames: Option<usize>,
}

/// Report a world's frame ordinal, last frame duration, elapsed time and
/// frame rate, spawning the monitor that measures the average on first call.
///
/// The frame state is the world's own [`FrameClock`], which a world that
/// dispatches frames spawns when it initialises. A world that never dispatched
/// a frame — the host world of a windowed run, which carries no scene — reports
/// no frame rather than a made-up zero, so a client can tell the two apart.
/// The average covers the last [`FRAME_WINDOW`] frames, which is finer than a
/// client can sample for itself and survives the frames between two calls.
pub(crate) fn frame_clock(world: &mut World) -> Result<FrameClockReport, String> {
    let existing = world
        .root_scope()
        .query::<&FrameMonitorState>()
        .iter()
        .next()
        .map(|state| state.monitor.clone());
    let monitor = match existing {
        Some(monitor) => monitor,
        None => {
            let monitor = Arc::new(Mutex::new(FrameMonitor::default()));
            let state = FrameMonitorState {
                monitor: monitor.clone(),
            };
            let behavior = frame_monitor(monitor.clone());
            world.root_scope_mut().spawn(bundle![behavior, state]);
            monitor
        }
    };
    let monitor = monitor
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let clock = world
        .root_scope()
        .query::<&FrameClock>()
        .iter()
        .next()
        .copied();
    // A zero-duration frame would report an infinite rate; leave the
    // instantaneous fps out instead.
    let fps = clock.and_then(|clock| (clock.delta_time > 0.0).then(|| 1.0 / clock.delta_time));
    let average_fps = monitor
        .average_delta()
        .filter(|delta| *delta > 0.0)
        .map(|delta| 1.0 / delta);
    let window_frames = monitor.window.len();
    Ok(FrameClockReport {
        index: clock.map(|clock| clock.index),
        delta_time: clock.map(|clock| clock.delta_time),
        time: clock.map(|clock| clock.time),
        fps,
        average_fps,
        average_window_frames: average_fps.map(|_| window_frames),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_names_the_image_format() {
        assert_eq!(ImageFormat::of("a.png"), Ok(ImageFormat::Png));
        assert_eq!(ImageFormat::of("a.webp"), Ok(ImageFormat::Webp));
        assert_eq!(ImageFormat::of("a.jpg"), Ok(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::of("a.jpeg"), Ok(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::of("a.JPG"), Ok(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::of("a.PNG"), Ok(ImageFormat::Png));
        assert_eq!(ImageFormat::from_name("jpeg"), Ok(ImageFormat::Jpeg));
        assert!(ImageFormat::of("a").is_err());
        assert!(ImageFormat::of("a.gif").is_err());
    }

    #[test]
    fn the_frame_clock_averages_the_frames_after_its_first_call() {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![FrameClock::default()]);
        for index in 0..3u32 {
            let mut frame = crate::frame::Frame {
                delta_time: 1.0 / 60.0,
                index,
            };
            dispatch_frame(&mut world, &mut frame);
        }

        // The first call spawns the monitor, which has seen no frame yet.
        let clock = frame_clock(&mut world).expect("the clock reports");
        assert_eq!(clock.index, Some(2));
        assert_eq!(clock.average_fps, None);

        // The frames after it fold into the rolling window.
        for index in 3..5u32 {
            let mut frame = crate::frame::Frame {
                delta_time: 1.0 / 30.0,
                index,
            };
            dispatch_frame(&mut world, &mut frame);
        }
        let clock = frame_clock(&mut world).expect("the clock reports");
        assert_eq!(clock.index, Some(4));
        assert_eq!(clock.average_window_frames, Some(2));
        let fps = clock.fps.expect("the last frame states a rate");
        assert!(
            (fps - 30.0).abs() < 0.1,
            "the instantaneous rate is the last frame's: {clock:?}"
        );
        let average = clock.average_fps.expect("an average exists");
        assert!(
            (average - 30.0).abs() < 0.1,
            "the window is the two 30 Hz frames: {clock:?}"
        );
    }

    #[test]
    fn a_world_that_never_dispatched_a_frame_reports_none() {
        let mut world = World::new();
        let clock = frame_clock(&mut world).expect("the clock reports");
        assert_eq!(clock.index, None);
        assert_eq!(clock.delta_time, None);
        assert_eq!(clock.time, None);
        assert_eq!(clock.fps, None);
        assert_eq!(clock.average_fps, None);

        // A world without a clock gains none behind the client's back, so a
        // frame dispatched into it still reads as no frame rather than a zero
        // the client would take for a real first frame.
        let mut frame = crate::frame::Frame {
            delta_time: 1.0 / 60.0,
            index: 7,
        };
        dispatch_frame(&mut world, &mut frame);
        let clock = frame_clock(&mut world).expect("the clock reports");
        assert_eq!(clock.index, None);
    }
}
