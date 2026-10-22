//! The rendering tools: the resource graph, buffers, textures, input, meshes
//! and frames.
//!
//! A tool names the entity handles it needs and this module resolves them
//! through the public API, reporting a readable error when a handle does not
//! name what the tool asked for. The tools themselves live one level up, in
//! [crate::mcp].

use std::path::Path;

use base64::Engine as _;
use glam::Vec3;
use image::ImageEncoder as _;
use unlit_ecs::mcp::host::{
    Despawned, SavedFile, SpawnedEntity, extension_of, saved_name, to_value, write_file,
};
use unlit_ecs::mcp::serde_json::{self, Value};
use unlit_ecs::mcp::{CreateMesh, EncodedImage, Output};
use unlit_ecs::{ArchetypeBuilder, Entity, Scope, World};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::readback::{readback_buffer, readback_texture};
use unlit_wgpu::resources::{
    ResId, Resource, ResourceGraph, ResourceGraphInfo, ResourceGraphMaintain,
};

use crate::gltf::UnlitGltf;
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
            .ok_or_else(|| format!("entity {} carries no render context", entity.to_bits()));
    }
    let mut found: Option<RenderContext> = None;
    for render_context in world.root_scope().query::<&RenderContext>() {
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
            .query_one::<Has<Renderer>>(entity)
            .unwrap_or(false)
            .then_some(entity)
            .ok_or_else(|| format!("entity {} carries no renderer", entity.to_bits()));
    }
    let mut found: Option<Entity> = None;
    for (_renderer, entity) in world.root_scope().query::<(&Renderer, Entity)>() {
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
            .query_one::<&Source>(entity)
            .is_some_and(|source| source.as_ref::<MeshSource>().is_some());
        return is_mesh_source
            .then_some(entity)
            .ok_or_else(|| format!("entity {} carries no mesh source", entity.to_bits()));
    }
    let mut found: Option<Entity> = None;
    for (source, entity) in world.root_scope().query::<(&Source, Entity)>() {
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
    f: impl FnOnce(&mut MeshSource, &Scope<'_>) -> R,
) -> Result<R, String> {
    let entity = mesh_source_entity(world, handle)?;
    // The scope is the source's own: a source entity is its own scope, and a
    // source only ever sees the subtree it draws.
    let scope = world
        .scope(entity)
        .ok_or_else(|| "the mesh source entity is gone".to_string())?;
    let mut source = scope
        .query_one::<&mut Source>(entity)
        .ok_or_else(|| "the mesh source entity is gone".to_string())?;
    let mesh_source = source
        .as_mut::<MeshSource>()
        .ok_or_else(|| "the source entity is not a mesh source".to_string())?;
    Ok(f(mesh_source, &scope))
}

/// The device the context names, cloned out of the world.
fn device_of(world: &World, context: RenderContext) -> Result<wgpu::Device, String> {
    world
        .root_scope()
        .query_one::<&wgpu::Device>(context.device)
        .map(|device| device.clone())
        .ok_or_else(|| "the render context has no device".to_string())
}

/// The queue the context names, cloned out of the world.
fn queue_of(world: &World, context: RenderContext) -> Result<wgpu::Queue, String> {
    world
        .root_scope()
        .query_one::<&wgpu::Queue>(context.queue)
        .map(|queue| queue.clone())
        .ok_or_else(|| "the render context has no queue".to_string())
}

/// The graph the context names, borrowed from the world.
fn graph_of<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::CellRef<'w, ResourceGraph>, String> {
    world
        .root_scope()
        .query_one::<&ResourceGraph>(context.graph)
        .ok_or_else(|| "the render context has no resource graph".to_string())
}

/// The graph the context names, mutably borrowed from the world.
fn graph_of_mut<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::CellRefMut<'w, ResourceGraph>, String> {
    world
        .root_scope()
        .query_one::<&mut ResourceGraph>(context.graph)
        .ok_or_else(|| "the render context has no resource graph".to_string())
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
    let ids = graph
        .dependencies(&handle)
        .map(|dependency| dependency.id())
        .collect::<Vec<_>>();
    to_value(&ids)
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
    Ok((offset, bytes))
}

/// Read bytes out of a buffer resource.
pub fn read_buffer(
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
pub fn save_buffer(
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
    Ok(Frame {
        bytes,
        width: texture.width(),
        height: texture.height(),
        format: texture.format(),
    })
}

/// Read a texture resource and encode it as an inline image.
pub fn read_texture_as_image(
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
pub fn save_texture_as_image(
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
            .query_one::<Has<T>>(entity)
            .unwrap_or(false)
            .then_some(entity)
            .ok_or_else(|| format!("entity {} carries no {what}", entity.to_bits()));
    }
    let mut found: Option<Entity> = None;
    for (_t, entity) in world.root_scope().query::<(&T, Entity)>() {
        if found.is_some() {
            return Err(format!("the world has more than one {what}; pass `entity`"));
        }
        found = Some(entity);
    }
    found.ok_or_else(|| format!("the world has no {what}"))
}

/// A world's input for this tick, encoded: its state and the events that built
/// it.
pub fn input_state(world: &World, entity: Option<Entity>) -> Result<Value, String> {
    let entity = single_entity::<InputEvents>(world, entity, "input buffer")?;
    let text = reflect::encode(&world.root_scope(), entity, "InputEvents")?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Feed input events to the world and deliver them.
///
/// The events go to the world's [`InputWriter`], which reaches every world
/// subscribed to it; the dispatch that follows is this world's own tick.
pub fn send_input(world: &World, entity: Option<Entity>, events: &Value) -> Result<Value, String> {
    let text = serde_json::to_string(events).map_err(|error| error.to_string())?;
    let decoded = reflect::decode_events(&text)?;
    let entity = single_entity::<InputWriter>(world, entity, "input writer")?;
    let _ = world
        .root_scope()
        .query_one::<&mut InputWriter>(entity)
        .map(|writer| writer.push_all(decoded));
    let delivered = dispatch_input(world);
    to_value(&Delivered { delivered })
}

/// Allocate a mesh and spawn an entity that draws it.
pub fn create_mesh(
    world: &mut World,
    source: Option<Entity>,
    args: CreateMesh,
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
    let (key, mesh) = with_mesh_source(world, source, |source, scope| {
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&source.device(scope)));
        let mesh = source.allocate_unlit_mesh(scope, &key, desc);
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
    builder.push_component(transform);
    builder.push_component(mesh);
    builder.push_component(UnlitPipeline::new(key));
    builder.push_component(InstanceColor::new(color));
    if let Some(cutoff) = args.cutoff {
        builder.push_component(InstanceCutoff::new(cutoff));
    }
    // The mesh lands in the requested scope, which defaults to the source
    // entity itself: a frame draws a source's own subtree, so a mesh spawned
    // beside the source would never be drawn.
    let scope = match args.scope {
        Some(bits) => {
            let scope = Entity::from_bits(bits);
            if world.scope(scope).is_none() {
                return Err(format!("no scope at bits {bits}"));
            }
            scope
        }
        None => mesh_source_entity(world, source)?,
    };
    let entity = world
        .scope_mut(scope)
        .ok_or_else(|| format!("no scope at bits {}", scope.to_bits()))?
        .spawn(builder);
    to_value(&SpawnedEntity { entity, scope })
}

/// Release a mesh and despawn the entity that drew it.
pub fn remove_mesh(
    world: &mut World,
    source: Option<Entity>,
    entity: Entity,
) -> Result<Value, String> {
    let mesh = world
        .root_scope()
        .query_one::<&GpuMesh>(entity)
        .map(|mesh| mesh.clone())
        .ok_or_else(|| format!("entity {} has no GpuMesh", entity.to_bits()))?;
    with_mesh_source(world, source, |source, _| source.remove_mesh(mesh))?;
    let despawned = world.root_scope_mut().despawn(entity);
    to_value(&Despawned { despawned })
}

/// Render one frame from the world.
pub fn render_frame(world: &mut World, renderer: Option<Entity>) -> Result<Value, String> {
    let entity = renderer_entity(world, renderer)?;
    let world_ref: &World = &*world;
    let scope = world_ref.root_scope();
    let _ = scope
        .query_one::<&mut Renderer>(entity)
        .map(|mut renderer| renderer.render(world_ref));
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

/// Render one frame and read the offscreen target back.
fn screenshot_frame(
    world: &mut World,
    renderer: Option<Entity>,
    target: Option<ResId>,
) -> Result<Frame, String> {
    let context = render_context(world, None)?;
    let texture = screenshot_target(world, context, target)?;
    render_frame(world, renderer)?;
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = readback_texture(&device, &queue, &texture);
    Ok(Frame {
        bytes,
        width: texture.width(),
        height: texture.height(),
        format: texture.format(),
    })
}

/// Render one frame and read the offscreen target back as an inline image.
pub fn read_screenshot(
    world: &mut World,
    renderer: Option<Entity>,
    target: Option<ResId>,
    format: Option<&str>,
) -> Result<Output, String> {
    let format = match format {
        Some(name) => ImageFormat::from_name(name)?,
        None => ImageFormat::DEFAULT,
    };
    let frame = screenshot_frame(world, renderer, target)?;
    image_value(&format.encode(&frame)?, frame.width, frame.height, format)
}

/// Render one frame and write the offscreen target to a file as an image.
pub fn save_screenshot(
    world: &mut World,
    renderer: Option<Entity>,
    target: Option<ResId>,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, String> {
    let frame = screenshot_frame(world, renderer, target)?;
    let file_name = saved_name(file_name, "screenshot", "webp");
    to_value(&save_image(directory, &file_name, &frame)?)
}

/// Load a glTF file and spawn its default scene.
pub fn load_gltf(
    world: &mut World,
    source: Option<Entity>,
    scope: Option<Entity>,
    path: &str,
) -> Result<Value, String> {
    let gltf = UnlitGltf::load(Path::new(path)).map_err(|error| error.to_string())?;
    let resources = with_mesh_source(world, source, |source, scope| {
        gltf.insert_resources(source, scope)
    })?;
    // Content goes into the requested scope, which defaults to the source
    // entity itself, so the frame that draws the source also draws the scene.
    let scope = match scope {
        Some(scope) => scope,
        None => mesh_source_entity(world, source)?,
    };
    let mut scope = world
        .scope_mut(scope)
        .ok_or_else(|| format!("no scope at bits {}", scope.to_bits()))?;
    let nodes = gltf.spawn_default_scene(&mut scope, &resources);
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
}
