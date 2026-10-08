//! The render-thread side of the MCP server.
//!
//! A [World] is not [Send], and the GPU work that reads it must happen on the
//! thread that owns it. The MCP transport, on the other hand, runs on a tokio
//! thread. The two meet at a command channel: a tool builds a [Command] holding
//! a closure, the render thread runs the closure against a [HostContext] and
//! sends the JSON result back. Nothing here is specific to the built-in
//! pipeline; every tool goes through the same public API a caller would use.

use std::path::Path;

use base64::Engine as _;
use glam::Vec3;
use image::ImageEncoder as _;
use serde_json::{Value, json};
use unlit_ecs::ArchetypeBuilder;
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::readback::{readback_buffer, readback_texture};
use unlit_wgpu::resources::{Resource, ResourceGraph, TextureExt};
use unlit3d::gltf::UnlitGltf;
use unlit3d::prelude::*;
use unlit3d::reflect;

/// The format of the offscreen target the standalone host renders into.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A unit of work for the render thread.
///
/// The closure captures plain data (tool arguments) and runs with exclusive
/// access to the world. It must be [Send] because it travels across the channel;
/// the world it touches never does.
pub type Job = Box<dyn FnOnce(&mut HostContext<'_>) -> Result<Value, String> + Send + 'static>;

/// A job together with the channel its result is sent back on.
pub struct Command {
    job: Job,
    reply: async_channel::Sender<Result<Value, String>>,
}

impl Command {
    /// Build a command and the receiver its result will arrive on.
    pub fn new(
        job: impl FnOnce(&mut HostContext<'_>) -> Result<Value, String> + Send + 'static,
    ) -> (Self, async_channel::Receiver<Result<Value, String>>) {
        let (reply, receiver) = async_channel::bounded(1);
        (
            Self {
                job: Box::new(job),
                reply,
            },
            receiver,
        )
    }
}

/// Run a command and send its result back.
///
/// The send is best-effort: a tool call that has already been cancelled has no
/// receiver left, and the result is simply dropped.
pub fn dispatch(command: Command, context: &mut HostContext<'_>) {
    let result = (command.job)(context);
    let _ = command.reply.send_blocking(result);
}

/// The world and resources a command runs against.
pub struct HostContext<'a> {
    /// The world being driven.
    pub world: &'a mut World,
    /// The entity carrying the [Renderer].
    pub renderer: Entity,
    /// The entity carrying the [MeshSource], if the world has one.
    pub source: Option<Entity>,
    /// The render context spawned by spawn_context.
    pub context: RenderContext,
    /// The offscreen color target, if the host renders offscreen.
    pub target: Option<&'a wgpu::Texture>,
    /// The format of [Self::target].
    pub target_format: wgpu::TextureFormat,
}

impl HostContext<'_> {
    fn device(&self) -> Result<wgpu::Device, String> {
        self.world
            .get::<wgpu::Device>(self.context.device)
            .map(|device| device.clone())
            .ok_or_else(|| "the world has no device".to_string())
    }

    fn queue(&self) -> Result<wgpu::Queue, String> {
        self.world
            .get::<wgpu::Queue>(self.context.queue)
            .map(|queue| queue.clone())
            .ok_or_else(|| "the world has no queue".to_string())
    }

    fn with_graph<R>(&self, f: impl FnOnce(&mut ResourceGraph) -> R) -> Result<R, String> {
        let mut graph = self
            .world
            .get_mut::<ResourceGraph>(self.context.graph)
            .ok_or_else(|| "the world has no resource graph".to_string())?;
        Ok(f(&mut graph))
    }

    fn with_graph_ref<R>(&self, f: impl FnOnce(&ResourceGraph) -> R) -> Result<R, String> {
        let graph = self
            .world
            .get::<ResourceGraph>(self.context.graph)
            .ok_or_else(|| "the world has no resource graph".to_string())?;
        Ok(f(&graph))
    }

    fn with_mesh_source<R>(&self, f: impl FnOnce(&mut MeshSource) -> R) -> Result<R, String> {
        let source = self
            .source
            .ok_or_else(|| "the world has no mesh source".to_string())?;
        let mut handle = self
            .world
            .get_mut::<Source>(source)
            .ok_or_else(|| "the mesh source entity is gone".to_string())?;
        let mesh_source = handle
            .as_mut::<MeshSource>()
            .ok_or_else(|| "the source entity is not a mesh source".to_string())?;
        Ok(f(mesh_source))
    }

    fn entity_components(&self, entity: Entity) -> Option<Vec<&'static str>> {
        let location = self.world.location(entity)?;
        let archetype = self.world.archetype(location.archetype())?;
        Some(
            archetype
                .types()
                .iter()
                .filter_map(|type_id| self.world.type_name(*type_id))
                .collect(),
        )
    }

    /// A high-level description of the world.
    pub(crate) fn world_summary(&self) -> Value {
        json!({
            "entities": self.world.len(),
            "archetypes": self.world.archetype_count(),
            "components": reflect::names().collect::<Vec<_>>(),
        })
    }

    /// The component names the reflection table holds.
    pub(crate) fn list_components(&self) -> Value {
        json!(reflect::names().collect::<Vec<_>>())
    }

    /// Every archetype with the component names it stores.
    pub(crate) fn list_archetypes(&self) -> Value {
        let archetypes: Vec<Value> = self
            .world
            .archetypes()
            .enumerate()
            .map(|(index, archetype)| {
                let types: Vec<&str> = archetype
                    .types()
                    .iter()
                    .filter_map(|type_id| self.world.type_name(*type_id))
                    .collect();
                json!({
                    "index": index,
                    "len": archetype.len(),
                    "types": types,
                })
            })
            .collect();
        Value::Array(archetypes)
    }

    /// Every spawned entity with its component names.
    pub(crate) fn list_entities(&self, limit: usize) -> Value {
        let mut entities = Vec::new();
        'outer: for archetype in self.world.archetypes() {
            let types: Vec<&str> = archetype
                .types()
                .iter()
                .filter_map(|type_id| self.world.type_name(*type_id))
                .collect();
            for entity in archetype.entities() {
                entities.push(json!({
                    "entity": entity.to_bits(),
                    "components": types,
                }));
                if entities.len() >= limit {
                    break 'outer;
                }
            }
        }
        Value::Array(entities)
    }

    /// One entity's component names and location.
    pub(crate) fn get_entity(&self, entity: Entity) -> Result<Value, String> {
        let location = self
            .world
            .location(entity)
            .ok_or_else(|| format!("no entity with bits {}", entity.to_bits()))?;
        let components = self.entity_components(entity).unwrap_or_default();
        Ok(json!({
            "entity": entity.to_bits(),
            "alive": self.world.contains(entity),
            "archetype": location.archetype(),
            "row": location.row(),
            "components": components,
        }))
    }

    /// One component of one entity, encoded.
    pub(crate) fn get_component(&self, entity: Entity, name: &str) -> Result<Value, String> {
        let text = reflect::encode(self.world, entity, name)?;
        serde_json::from_str(&text).map_err(|error| error.to_string())
    }

    /// Overwrite one component of one entity.
    pub(crate) fn set_component(
        &self,
        entity: Entity,
        name: &str,
        value: &Value,
    ) -> Result<Value, String> {
        let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
        reflect::set(self.world, entity, name, &patch)?;
        Ok(Value::Null)
    }

    /// Spawn an entity from a map of component name to value.
    pub(crate) fn spawn_entity(&mut self, components: &Value) -> Result<Value, String> {
        let object = components
            .as_object()
            .ok_or_else(|| "components must be an object".to_string())?;
        let mut builder = ArchetypeBuilder::new();
        for (name, value) in object {
            let patch = serde_json::to_string(value).map_err(|error| error.to_string())?;
            reflect::push(&mut builder, name, &patch)?;
        }
        let entity = self.world.spawn(builder);
        Ok(json!({"entity": entity.to_bits()}))
    }

    /// Despawn one entity.
    pub(crate) fn despawn_entity(&mut self, entity: Entity) -> Result<Value, String> {
        Ok(json!({"despawned": self.world.despawn(entity)}))
    }

    /// A summary of the resource graph.
    pub(crate) fn graph_summary(&self) -> Result<Value, String> {
        self.with_graph_ref(|graph| {
            let mut kinds = std::collections::BTreeMap::<&'static str, usize>::new();
            let mut dirty = 0usize;
            let mut rebuildable = 0usize;
            for node in graph.nodes() {
                *kinds.entry(node.resource.kind_name()).or_default() += 1;
                dirty += usize::from(node.dirty);
                rebuildable += usize::from(node.rebuildable);
            }
            json!({
                "resources": graph.len(),
                "empty": graph.is_empty(),
                "dirty": dirty,
                "rebuildable": rebuildable,
                "kinds": kinds,
            })
        })
    }

    /// Every resource slot in the graph.
    pub(crate) fn list_resources(&self) -> Result<Value, String> {
        self.with_graph_ref(|graph| {
            let resources: Vec<Value> = graph
                .nodes()
                .map(|node| {
                    json!({
                        "index": node.index,
                        "kind": node.resource.kind_name(),
                        "dirty": node.dirty,
                        "rebuildable": node.rebuildable,
                    })
                })
                .collect();
            Value::Array(resources)
        })
    }

    /// The resources a slot depends on, as slot indices.
    pub(crate) fn resource_dependencies(&self, index: usize) -> Result<Value, String> {
        self.with_graph_ref(|graph| match graph.id_at(index) {
            Some(id) => json!(
                graph
                    .dependencies(&id)
                    .map(|id| id.index())
                    .collect::<Vec<_>>()
            ),
            None => Value::Null,
        })
    }

    /// Drop unreferenced resources and rebuild dirty ones.
    pub(crate) fn graph_maintain(&self) -> Result<Value, String> {
        self.with_graph(|graph| {
            let before = graph.len();
            graph.maintain();
            json!({"before": before, "after": graph.len()})
        })
    }

    /// Read bytes out of a buffer resource.
    pub(crate) fn read_buffer(
        &self,
        index: usize,
        offset: u64,
        size: Option<u64>,
    ) -> Result<Value, String> {
        let buffer = self
            .with_graph_ref(|graph| match graph.id_at(index) {
                Some(id) => match graph.get::<Resource>(&id) {
                    Some(Resource::Buffer(buffer)) => Some(buffer.clone()),
                    _ => None,
                },
                None => None,
            })?
            .ok_or_else(|| format!("no buffer at resource index {index}"))?;
        if !buffer.usage().contains(wgpu::BufferUsages::COPY_SRC) {
            return Err(format!(
                "buffer at resource index {index} was not created with COPY_SRC, so it cannot be read back"
            ));
        }
        let total = buffer.size();
        if offset > total {
            return Err(format!(
                "offset {offset} is past the buffer's {total} bytes"
            ));
        }
        let size = size.unwrap_or(total - offset).min(total - offset);
        let device = self.device()?;
        let queue = self.queue()?;
        let bytes = readback_buffer(&device, &queue, &buffer, offset, size);
        Ok(json!({
            "offset": offset,
            "size": bytes.len(),
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }))
    }

    /// Read a texture resource and encode it as a PNG.
    pub(crate) fn read_texture_as_image(&self, index: usize) -> Result<Value, String> {
        let texture = self
            .with_graph_ref(|graph| match graph.id_at(index) {
                Some(id) => match graph.get::<Resource>(&id) {
                    Some(Resource::Texture(texture)) => Some(texture.clone()),
                    Some(Resource::TextureView(view)) => Some(view.texture().clone()),
                    _ => None,
                },
                None => None,
            })?
            .ok_or_else(|| format!("no texture at resource index {index}"))?;
        let device = self.device()?;
        let queue = self.queue()?;
        let bytes = readback_texture(&device, &queue, &texture);
        let png = encode_png(&bytes, texture.width(), texture.height(), texture.format())?;
        Ok(image_value(&png, texture.width(), texture.height()))
    }

    /// The first input state, encoded, events included.
    pub(crate) fn input_state(&self) -> Result<Value, String> {
        let entity = self
            .world
            .query::<&InputState>()
            .next()
            .map(|(entity, _)| entity)
            .ok_or_else(|| "the world has no InputState".to_string())?;
        let text = reflect::encode(self.world, entity, "InputState")?;
        serde_json::from_str(&text).map_err(|error| error.to_string())
    }

    /// Push input events into the world and deliver them.
    pub(crate) fn send_input(&self, events: &Value) -> Result<Value, String> {
        let events = events
            .as_array()
            .ok_or_else(|| "events must be an array".to_string())?;
        let mut decoded = Vec::with_capacity(events.len());
        for event in events {
            decoded.push(decode_input_event(event)?);
        }
        let entity = self
            .world
            .query::<&InputState>()
            .next()
            .map(|(entity, _)| entity)
            .ok_or_else(|| "the world has no InputState".to_string())?;
        let _ = self.world.with_mut::<InputState, _>(entity, |state| {
            for event in decoded {
                state.push(event);
            }
        });
        let delivered = dispatch_input(self.world);
        Ok(json!({"delivered": delivered}))
    }

    /// Allocate a mesh and spawn an entity that draws it.
    pub(crate) fn create_mesh(&mut self, value: &Value) -> Result<Value, String> {
        let positions = vec3_array(field(value, "positions")?, "positions")?;
        if positions.is_empty() {
            return Err("positions must not be empty".to_string());
        }
        let uvs = optional_vec2_array(value.get("uvs"), "uvs")?;
        let colors = optional_color_array(value.get("colors"), "colors")?;
        let indices = optional_u32_array(value.get("indices"), "indices")?;
        let desc = UnlitMeshDesc {
            positions: &positions,
            uvs: uvs.as_deref(),
            colors: colors.as_deref(),
            indices: indices.as_deref(),
            joints: None,
            weights: None,
            morph_deltas: None,
        };
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&self.device()?));
        let mesh =
            self.with_mesh_source(|source| source.allocate_unlit_mesh(self.world, &key, desc))?;
        let transform = match present(value.get("transform")) {
            Some(transform) => decode_transform(transform)?,
            None => Transform::default(),
        };
        let color = match present(value.get("color")) {
            Some(color) => vec4(color, "color")?,
            None => glam::Vec4::ONE,
        };
        let mut builder = ArchetypeBuilder::new();
        builder.push(transform);
        builder.push(mesh);
        builder.push(UnlitPipeline::new(key));
        builder.push(InstanceColor::new(color));
        if let Some(cutoff) = present(value.get("cutoff")) {
            builder.push(InstanceCutoff::new(
                cutoff
                    .as_f64()
                    .map(|n| n as f32)
                    .ok_or_else(|| "cutoff must be a number".to_string())?,
            ));
        }
        let entity = self.world.spawn(builder);
        Ok(json!({"entity": entity.to_bits()}))
    }

    /// Remove a mesh entity and release its mesh.
    pub(crate) fn remove_mesh(&mut self, entity: Entity) -> Result<Value, String> {
        let mesh = self
            .world
            .get::<GpuMesh>(entity)
            .map(|mesh| mesh.clone())
            .ok_or_else(|| format!("entity {} has no GpuMesh", entity.to_bits()))?;
        self.with_mesh_source(|source| source.remove_mesh(mesh))?;
        let despawned = self.world.despawn(entity);
        Ok(json!({"despawned": despawned}))
    }

    /// Render one frame with the world's renderer.
    pub(crate) fn render_frame(&self) -> Result<Value, String> {
        let world: &World = &*self.world;
        let _ = world.with_mut::<Renderer, _>(self.renderer, |renderer| renderer.render(world));
        Ok(Value::Null)
    }

    /// Render one frame and return the offscreen target as a PNG.
    pub(crate) fn screenshot(&self) -> Result<Value, String> {
        let target = self
            .target
            .ok_or_else(|| "the host has no offscreen target to screenshot".to_string())?;
        self.render_frame()?;
        let device = self.device()?;
        let queue = self.queue()?;
        let bytes = readback_texture(&device, &queue, target);
        let png = encode_png(&bytes, target.width(), target.height(), self.target_format)?;
        Ok(image_value(&png, target.width(), target.height()))
    }

    /// Load a glTF file and spawn its default scene.
    pub(crate) fn load_gltf(&mut self, path: &str) -> Result<Value, String> {
        let gltf = UnlitGltf::load(Path::new(path)).map_err(|error| error.to_string())?;
        let resources =
            self.with_mesh_source(|source| gltf.insert_resources(source, self.world))?;
        let nodes = gltf.spawn_default_scene(self.world, &resources);
        let entities: Vec<u64> = nodes
            .iter()
            .flat_map(|node| node.entities.iter())
            .map(|entity| entity.to_bits())
            .collect();
        Ok(json!({"entities": entities}))
    }
}

/// The standalone host: an offscreen world served over MCP.
pub struct Host {
    world: World,
    renderer: Entity,
    source: Option<Entity>,
    context: RenderContext,
    target: wgpu::Texture,
    target_format: wgpu::TextureFormat,
}

impl Host {
    /// Create a world with a renderer, a mesh source, an input state, a default
    /// camera and an offscreen target of the given size.
    pub async fn new_offscreen(
        size: (u32, u32),
        samples: u32,
        depth: bool,
    ) -> Result<Self, String> {
        let (device, queue, capabilities) = request_device("unlit3d_mcp").await?;

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
        let source = spawn_source(&mut world, source);
        world.spawn((InputState::default(),));

        let target = create_render_target(&device, COLOR_FORMAT, size.0, size.1, samples);
        // The graph borrow ends before the renderer is touched: both live in
        // the world, and holding one column while borrowing another panics.
        let (color, depth_view, msaa) = {
            let mut graph = world
                .get_mut::<ResourceGraph>(context.graph)
                .ok_or_else(|| "the context has no graph".to_string())?;
            let color = graph.insert(
                TextureExt::create_view(&target.color, &wgpu::TextureViewDescriptor::default()),
                None,
            );
            let depth_view = depth.then(|| {
                graph.insert(
                    TextureExt::create_view(&target.depth, &wgpu::TextureViewDescriptor::default()),
                    None,
                )
            });
            let msaa = target.msaa.as_ref().map(|msaa| {
                graph.insert(
                    TextureExt::create_view(msaa, &wgpu::TextureViewDescriptor::default()),
                    None,
                )
            });
            (color, depth_view, msaa)
        };
        let _ = world.with_mut::<Renderer, _>(renderer, |renderer| {
            renderer.set_render_target(&world, Some(color), depth_view, msaa)
        });

        world.spawn((default_camera(size),));

        Ok(Self {
            world,
            renderer,
            source: Some(source),
            context,
            target: target.color,
            target_format: COLOR_FORMAT,
        })
    }

    /// Wrap a world that already has a renderer, an offscreen target and
    /// whatever else its owner built.
    ///
    /// The caller keeps ownership of the scene's construction — the host only
    /// drives it. The target's own format is what screenshots are encoded as.
    pub fn from_world(
        world: World,
        renderer: Entity,
        source: Option<Entity>,
        context: RenderContext,
        target: wgpu::Texture,
    ) -> Self {
        Self {
            world,
            renderer,
            source,
            context,
            target_format: target.format(),
            target,
        }
    }

    /// The world being driven.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The world being driven, mutably.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// The entity carrying the [Renderer].
    pub fn renderer(&self) -> Entity {
        self.renderer
    }

    /// The entity carrying the [MeshSource], if the world has one.
    pub fn source(&self) -> Option<Entity> {
        self.source
    }

    /// The render context.
    pub fn context(&self) -> RenderContext {
        self.context
    }

    /// Run a command against this host.
    pub fn dispatch(&mut self, command: Command) {
        let mut context = HostContext {
            world: &mut self.world,
            renderer: self.renderer,
            source: self.source,
            context: self.context,
            target: Some(&self.target),
            target_format: self.target_format,
        };
        dispatch(command, &mut context);
    }
}

/// Request a headless device, queue and capability set.
///
/// The same public entry points a normal application uses, so a host built on
/// this device is no more privileged than one built on a window's.
///
/// # Errors
///
/// Fails when no adapter or device is available.
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

fn default_camera(size: (u32, u32)) -> Camera {
    let eye = Vec3::new(0.0, 1.0, 3.0);
    let target = Vec3::ZERO;
    let up = Vec3::Y;
    let view_from_world = glam::camera::rh::view::look_at_mat4(eye, target, up);
    let aspect = size.0.max(1) as f32 / size.1.max(1) as f32;
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

fn image_value(png: &[u8], width: u32, height: u32) -> Value {
    json!({
        "mime_type": "image/png",
        "width": width,
        "height": height,
        "data": base64::engine::general_purpose::STANDARD.encode(png),
    })
}

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

/// The value at an optional argument that was absent, where a present JSON
/// null counts as absent: a tool argument that serializes its None fields
/// writes them as null, and both spellings mean the caller left the field out.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .get(name)
        .ok_or_else(|| format!("missing field {name:?}"))
}

fn as_f32(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|number| number as f32)
        .ok_or_else(|| "expected a number".to_string())
}

fn as_u32(value: &Value) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| "expected a non-negative integer".to_string())
}

fn vec3(value: &Value, name: &str) -> Result<Vec3, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    if array.len() != 3 {
        return Err(format!("{name} must have 3 components"));
    }
    Ok(Vec3::new(
        as_f32(&array[0])?,
        as_f32(&array[1])?,
        as_f32(&array[2])?,
    ))
}

fn vec4(value: &Value, name: &str) -> Result<glam::Vec4, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    if array.len() != 4 {
        return Err(format!("{name} must have 4 components"));
    }
    Ok(glam::Vec4::new(
        as_f32(&array[0])?,
        as_f32(&array[1])?,
        as_f32(&array[2])?,
        as_f32(&array[3])?,
    ))
}

fn decode_transform(value: &Value) -> Result<Transform, String> {
    Ok(Transform {
        translation: vec3(field(value, "translation")?, "translation")?,
        rotation: quat(field(value, "rotation")?, "rotation")?,
        scale: vec3(field(value, "scale")?, "scale")?,
    })
}

fn quat(value: &Value, name: &str) -> Result<glam::Quat, String> {
    let vec = vec4(value, name)?;
    Ok(glam::Quat::from_xyzw(vec.x, vec.y, vec.z, vec.w))
}

fn vec3_array(value: &Value, name: &str) -> Result<Vec<[f32; 3]>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|point| {
            let point = vec3(point, name)?;
            Ok([point.x, point.y, point.z])
        })
        .collect()
}

fn optional_vec2_array(value: Option<&Value>, name: &str) -> Result<Option<Vec<[f32; 2]>>, String> {
    let Some(value) = present(value) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    let mut out = Vec::with_capacity(array.len());
    for pair in array {
        let pair = pair
            .as_array()
            .ok_or_else(|| format!("{name} must be an array of pairs"))?;
        if pair.len() != 2 {
            return Err(format!("{name} must have 2 components"));
        }
        out.push([as_f32(&pair[0])?, as_f32(&pair[1])?]);
    }
    Ok(Some(out))
}

fn optional_color_array(value: Option<&Value>, name: &str) -> Result<Option<Vec<[u8; 4]>>, String> {
    let Some(value) = present(value) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    let mut out = Vec::with_capacity(array.len());
    for color in array {
        let color = color
            .as_array()
            .ok_or_else(|| format!("{name} must be an array of colors"))?;
        if color.len() != 4 {
            return Err(format!("{name} must have 4 components"));
        }
        let mut bytes = [0u8; 4];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from(as_u32(&color[index])?)
                .map_err(|_| format!("{name} components must fit a byte"))?;
        }
        out.push(bytes);
    }
    Ok(Some(out))
}

fn optional_u32_array(value: Option<&Value>, name: &str) -> Result<Option<Vec<u32>>, String> {
    let Some(value) = present(value) else {
        return Ok(None);
    };
    value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|number| as_u32(number).map_err(|error| format!("{name}: {error}")))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn decode_key(name: &str) -> Result<Key, String> {
    let key = match name {
        "Escape" => Key::Escape,
        "Space" => Key::Space,
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "ArrowUp" => Key::ArrowUp,
        "ArrowDown" => Key::ArrowDown,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "0" | "Num0" => Key::Num0,
        "1" | "Num1" => Key::Num1,
        "2" | "Num2" => Key::Num2,
        "3" | "Num3" => Key::Num3,
        "4" | "Num4" => Key::Num4,
        "5" | "Num5" => Key::Num5,
        "6" | "Num6" => Key::Num6,
        "7" | "Num7" => Key::Num7,
        "8" | "Num8" => Key::Num8,
        "9" | "Num9" => Key::Num9,
        other => {
            if let Some(letter) = other
                .strip_prefix('F')
                .and_then(|rest| rest.parse::<u8>().ok())
            {
                match letter {
                    1 => Key::F1,
                    2 => Key::F2,
                    3 => Key::F3,
                    4 => Key::F4,
                    5 => Key::F5,
                    6 => Key::F6,
                    7 => Key::F7,
                    8 => Key::F8,
                    9 => Key::F9,
                    10 => Key::F10,
                    _ => return Err(format!("unsupported function key {name:?}")),
                }
            } else if other.len() == 1 && other.as_bytes()[0].is_ascii_alphabetic() {
                key_from_letter(other.as_bytes()[0].to_ascii_uppercase())
            } else {
                return Err(format!("unknown key {name:?}"));
            }
        }
    };
    Ok(key)
}

fn key_from_letter(letter: u8) -> Key {
    match letter {
        b'A' => Key::A,
        b'B' => Key::B,
        b'C' => Key::C,
        b'D' => Key::D,
        b'E' => Key::E,
        b'F' => Key::F,
        b'G' => Key::G,
        b'H' => Key::H,
        b'I' => Key::I,
        b'J' => Key::J,
        b'K' => Key::K,
        b'L' => Key::L,
        b'M' => Key::M,
        b'N' => Key::N,
        b'O' => Key::O,
        b'P' => Key::P,
        b'Q' => Key::Q,
        b'R' => Key::R,
        b'S' => Key::S,
        b'T' => Key::T,
        b'U' => Key::U,
        b'V' => Key::V,
        b'W' => Key::W,
        b'X' => Key::X,
        b'Y' => Key::Y,
        b'Z' => Key::Z,
        _ => Key::Other(letter as u32),
    }
}

fn decode_mouse_button(name: &str) -> Result<MouseButton, String> {
    Ok(match name {
        "Primary" | "primary" | "Left" | "left" => MouseButton::Primary,
        "Secondary" | "secondary" | "Right" | "right" => MouseButton::Secondary,
        "Middle" | "middle" => MouseButton::Middle,
        "Back" | "back" => MouseButton::Back,
        "Forward" | "forward" => MouseButton::Forward,
        other => return Err(format!("unknown mouse button {other:?}")),
    })
}

fn decode_modifiers(value: Option<&Value>) -> Result<Modifiers, String> {
    let Some(value) = value else {
        return Ok(Modifiers::default());
    };
    let flag = |name: &str| value.get(name).and_then(Value::as_bool).unwrap_or(false);
    Ok(Modifiers {
        alt: flag("alt"),
        ctrl: flag("ctrl"),
        shift: flag("shift"),
        mac_cmd: flag("mac_cmd"),
        command: flag("command"),
    })
}

fn decode_position(value: &Value, name: &str) -> Result<[f32; 2], String> {
    let position = vec2(value, name)?;
    Ok([position.x, position.y])
}

fn vec2(value: &Value, name: &str) -> Result<glam::Vec2, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    if array.len() != 2 {
        return Err(format!("{name} must have 2 components"));
    }
    Ok(glam::Vec2::new(as_f32(&array[0])?, as_f32(&array[1])?))
}

fn decode_input_event(value: &Value) -> Result<InputEvent, String> {
    let kind = field(value, "kind")?
        .as_str()
        .ok_or_else(|| "an event's kind must be a string".to_string())?;
    Ok(match kind {
        "key" => InputEvent::Key(KeyEvent {
            key: decode_key(
                field(value, "key")?
                    .as_str()
                    .ok_or_else(|| "key must be a string".to_string())?,
            )?,
            pressed: field(value, "pressed")?.as_bool().unwrap_or(true),
            repeat: value
                .get("repeat")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            modifiers: decode_modifiers(value.get("modifiers"))?,
        }),
        "mouse_move" => InputEvent::Mouse(MouseEvent::Moved {
            position: decode_position(field(value, "position")?, "position")?,
        }),
        "mouse_button" => InputEvent::Mouse(MouseEvent::Button {
            position: decode_position(field(value, "position")?, "position")?,
            button: decode_mouse_button(
                field(value, "button")?
                    .as_str()
                    .ok_or_else(|| "button must be a string".to_string())?,
            )?,
            pressed: field(value, "pressed")?.as_bool().unwrap_or(true),
            modifiers: decode_modifiers(value.get("modifiers"))?,
        }),
        "mouse_left" => InputEvent::Mouse(MouseEvent::Left),
        "wheel" => InputEvent::Mouse(MouseEvent::Wheel {
            delta: decode_position(field(value, "delta")?, "delta")?,
            unit: WheelUnit::Pixel,
            phase: TouchPhase::Moved,
            modifiers: decode_modifiers(value.get("modifiers"))?,
        }),
        "text" => InputEvent::Text(TextEvent(
            field(value, "text")?
                .as_str()
                .ok_or_else(|| "text must be a string".to_string())?
                .to_string(),
        )),
        "focus" => InputEvent::FocusChanged(
            field(value, "focused")?
                .as_bool()
                .ok_or_else(|| "focused must be a boolean".to_string())?,
        ),
        other => return Err(format!("unknown event kind {other:?}")),
    })
}
