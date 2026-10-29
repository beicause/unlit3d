#![doc = include_str!("../README.md")]

use std::path::Path;
use unlit_ecs::bundle;

use glam::Mat4;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit3d::gltf::LitGltf;
use unlit3d::prelude::*;

pub mod cli;
mod config;

pub use config::{
    AnimationRef, CameraConfig, Config, DocumentConfig, OutputConfig, PlacementConfig,
};

/// The colour format every render target uses.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Everything that can stop a render or a save.
#[derive(Debug)]
pub enum Error {
    /// The configuration is incomplete or inconsistent.
    Config(String),
    /// The GPU could not provide an adapter or a device.
    Gpu(String),
    /// The configuration file is not valid JSON.
    Json(serde_json::Error),
    /// A glTF document could not be read.
    Gltf(gltf::Error),
    /// The image could not be encoded.
    Image(image::ImageError),
    /// A document has no animation under the requested name or index.
    Animation(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) | Self::Gpu(message) | Self::Animation(message) => {
                formatter.write_str(message)
            }
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Gltf(error) => write!(formatter, "{error}"),
            Self::Image(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Gltf(error) => Some(error),
            Self::Image(error) => Some(error),
            Self::Config(_) | Self::Gpu(_) | Self::Animation(_) => None,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<gltf::Error> for Error {
    fn from(error: gltf::Error) -> Self {
        Self::Gltf(error)
    }
}

impl From<image::ImageError> for Error {
    fn from(error: image::ImageError) -> Self {
        Self::Image(error)
    }
}

/// The pixels a render produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The RGBA8 pixels: width * height * 4 bytes, row-major, no padding.
    pub rgba: Vec<u8>,
    /// The image width in pixels.
    pub width: u32,
    /// The image height in pixels.
    pub height: u32,
}

/// Render the configured documents into an RGBA8 frame.
///
/// The output size is output.size, or output.render_size when size is absent.
/// The render size defaults to the output size. The viewport is the render size
/// scaled by output.scale's x and y factors and centred in the output. Nothing
/// is resampled, because the one output-sized texture is what the scene draws
/// into.
///
/// # Errors
///
/// Fails when the configuration is incomplete or inconsistent, a document or
/// its animation cannot be read, or the GPU cannot provide an adapter.
pub async fn render(config: &Config) -> Result<Frame, Error> {
    let output_size = output_size(config)?;
    let render_size = config.output.render_size.unwrap_or(output_size);
    if render_size.0 == 0 || render_size.1 == 0 {
        return Err(Error::Config(
            "the render size must have non-zero dimensions".to_owned(),
        ));
    }
    if !config.output.scale.0.is_finite()
        || !config.output.scale.1.is_finite()
        || config.output.scale.0 <= 0.0
        || config.output.scale.1 <= 0.0
    {
        return Err(Error::Config(
            "output.scale must be a pair of positive numbers".to_owned(),
        ));
    }
    if !matches!(config.output.samples, 1 | 2 | 4 | 8) {
        return Err(Error::Config(format!(
            "output.samples is {}, but it must be 1, 2, 4 or 8",
            config.output.samples
        )));
    }
    if config.camera.view.is_some() != config.camera.projection.is_some() {
        return Err(Error::Config(
            "camera.view and camera.projection must be given together: the camera holds \
             its view and projection apart, so one matrix alone is not a camera"
                .to_owned(),
        ));
    }

    let (device, queue, capabilities) = request_device().await?;
    let (world, target, renderer) = build_world(
        config,
        &device,
        &queue,
        capabilities,
        output_size,
        render_size,
    )?;

    world
        .root_scope()
        .query_one::<&mut Renderer>(renderer)
        .map(|mut renderer| renderer.render(&world))
        .expect("the renderer entity exists");

    let rgba = unlit_wgpu::readback::readback_texture(&device, &queue, &target);
    Ok(Frame {
        rgba,
        width: output_size.0,
        height: output_size.1,
    })
}

/// Ask the backend for a device and a queue.
///
/// # Errors
///
/// Fails when no adapter is available or the device cannot be created.
pub async fn request_device() -> Result<(wgpu::Device, wgpu::Queue, DeviceCapabilities), Error> {
    let tier = DeviceTier::from_env();
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: None,
            ..Default::default()
        })
        .await
        .map_err(|error| Error::Gpu(format!("no graphics adapter available: {error}")))?;
    let capabilities = tier.capabilities_of(&adapter);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("unlit3d_cli"),
            required_features: wgpu::Features::empty(),
            required_limits: tier.limits(&adapter.limits()),
            default_queue: wgpu::QueueDescriptor { label: None },
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|error| Error::Gpu(format!("failed to request a device: {error}")))?;
    Ok((device, queue, capabilities))
}

/// Build the world a render draws, and the target it draws into.
///
/// Returns the world, the output-sized colour target, and the renderer entity.
/// The world holds the render context, the renderer, the frame's one pass, a
/// mesh source, the configured documents and camera, and the pass's viewport.
///
/// # Errors
///
/// Fails when a document or its animation cannot be read.
pub fn build_world(
    config: &Config,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    capabilities: DeviceCapabilities,
    output_size: (u32, u32),
    render_size: (u32, u32),
) -> Result<(World, wgpu::Texture, Entity), Error> {
    let mut world = World::new();
    let context = spawn_context(
        &mut world,
        device.clone(),
        queue.clone(),
        ResourceGraph::new(),
        capabilities,
    );
    let renderer = world
        .root_scope_mut()
        .spawn(bundle![Renderer::new(context)]);

    let clear = config.output.clear;
    // The frame is one pass: it states what it clears and where it draws, and
    // the mesh source hangs under it, which is what records the source into it.
    let load = RenderLoadOps {
        color: wgpu::LoadOp::Clear(wgpu::Color {
            r: clear[0],
            g: clear[1],
            b: clear[2],
            a: clear[3],
        }),
        depth: depth_clear(),
        stencil: stencil_clear(),
    };
    let pass = world.root_scope_mut().spawn(Pass {
        load,
        ..Pass::new(FramePassOrder(0))
    });

    let target = bind_target(
        &world,
        context,
        pass,
        device,
        output_size,
        config.output.samples,
        config.output.depth,
    );

    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    let source_entity = world
        .scope_mut(pass)
        .expect("the pass entity is its own scope")
        .spawn(bundle![Source::new(source)]);

    for document in &config.documents {
        let gltf = LitGltf::load(&document.path)?;
        let resources = with_mesh_source(&world, source_entity, |source, world| {
            gltf.insert_resources(source, world)
        });
        let nodes = {
            let mut scope = world
                .scope_mut(source_entity)
                .expect("the source entity is its own scope");
            gltf.spawn_default_scene(&mut scope, &resources)
        };
        if let Some(placement) = &document.placement {
            let transform = placement.transform();
            for node in &nodes {
                for &entity in &node.entities {
                    let _ = world
                        .root_scope()
                        .query_one::<&mut Transform>(entity)
                        .map(|mut current| *current = transform.clone());
                }
            }
        }
        if let Some(animation) = &document.animation {
            let index = animation_index(&gltf, animation)?;
            gltf.apply_animation(&world.root_scope(), index, document.time, &nodes);
        }
    }

    // The camera sits in the source's own subtree, which is what it frames.
    world
        .scope_mut(source_entity)
        .expect("the source entity is its own scope")
        .spawn(bundle![camera(&config.camera, render_size)]);

    let viewport_width = render_size.0 as f32 * config.output.scale.0;
    let viewport_height = render_size.1 as f32 * config.output.scale.1;
    let viewport_x = (output_size.0 as f32 - viewport_width) * 0.5;
    let viewport_y = (output_size.1 as f32 - viewport_height) * 0.5;
    world
        .root_scope()
        .query_one::<&mut FramePass>(pass)
        .map(|mut pass| {
            pass.viewport =
                Some(ViewportRect::new(viewport_width, viewport_height).at(viewport_x, viewport_y));
        })
        .expect("the pass entity exists");

    Ok((world, target, renderer))
}

/// Build an empty offscreen world a host can serve over MCP.
///
/// The world holds a render context, a renderer, a mesh source, an input state
/// and a default camera, and an offscreen colour target registered in the graph
/// — both as a texture a screenshot can read back and as the view the renderer
/// draws into. Returns the world and the renderer entity.
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
    let renderer = world
        .root_scope_mut()
        .spawn(bundle![Renderer::new(context)]);
    // The frame is one pass, drawing into the offscreen target and clearing
    // with the defaults; the mesh source hangs under it.
    let pass = world.root_scope_mut().spawn(Pass::new(FramePassOrder(0)));
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    let source_entity = world
        .scope_mut(pass)
        .expect("the pass entity is its own scope")
        .spawn(bundle![Source::new(source)]);
    // The input is shared by every source, so it lives in the subtree every
    // scope sees.
    let input = InputWriter::new();
    {
        let shared = world.shared();
        let mut shared_view = world.scope_mut(shared).expect("the shared scope exists");
        shared_view.spawn(InputFeed(input.clone()));
    }

    bind_target(&world, context, pass, &device, size, samples, depth);
    // The camera sits in the source's own subtree, which is what it frames.
    world
        .scope_mut(source_entity)
        .expect("the source entity is its own scope")
        .spawn(bundle![default_camera(size)]);
    (world, renderer)
}

/// The camera an offscreen world starts with.
fn default_camera(size: (u32, u32)) -> Camera {
    let aspect = size.0.max(1) as f32 / size.1.max(1) as f32;
    let view_from_world = glam::camera::rh::view::look_at_mat4(
        glam::Vec3::new(0.0, 1.0, 3.0),
        glam::Vec3::ZERO,
        glam::Vec3::Y,
    );
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

/// Serve an empty offscreen world over MCP stdio.
///
/// The world lives on a render thread of its own; the transport thread only
/// forwards commands to it, exactly as a windowed host forwards them to its
/// event loop. A browser has no stdio to serve, so this exists only off wasm.
///
/// # Errors
///
/// Fails when no adapter is available, when the render thread cannot start, or
/// when the transport fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn serve_stdio_offscreen(size: (u32, u32), samples: u32, depth: bool) -> Result<(), Error> {
    use std::sync::mpsc;
    let (sender, receiver) = mpsc::channel::<unlit_ecs::mcp::McpCommand>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let render = std::thread::Builder::new()
        .name("unlit3d-cli-mcp-render".to_owned())
        .spawn(move || {
            let built =
                pollster::block_on(request_device()).map(|(device, queue, capabilities)| {
                    offscreen_world(device, queue, capabilities, size, samples, depth)
                });
            match built {
                Ok((mut world, _renderer)) => {
                    let _ = ready_tx.send(Ok(()));
                    while let Ok(command) = receiver.recv() {
                        if command.wants_worlds() {
                            command.answer_worlds(vec![unlit_ecs::mcp::WorldEntry {
                                world: unlit_ecs::mcp::WorldId::Host,
                                name: "host",
                            }]);
                        } else {
                            command.dispatch(&mut world);
                        }
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                }
            }
        })
        .map_err(|error| Error::Gpu(error.to_string()))?;
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return Err(Error::Gpu(message)),
        Err(_) => return Err(Error::Gpu("the render thread stopped".to_owned())),
    }
    let served = unlit_ecs::mcp::serve_stdio_with(move |command| sender.send(command).is_ok());
    let _ = render.join();
    served.map_err(|error| Error::Gpu(error.to_string()))
}

/// Encode a frame to path, choosing the format from the extension.
///
/// png and webp write the RGBA8 pixels directly; jpeg has no alpha, so the
/// colour is converted to RGB8 first.
///
/// # Errors
///
/// Fails when the extension names no supported format or the encoder fails.
pub fn save(path: impl AsRef<Path>, frame: &Frame) -> Result<(), Error> {
    let path = path.as_ref();
    let format = image::ImageFormat::from_path(path)?;
    if format == image::ImageFormat::Jpeg {
        let mut rgb = Vec::with_capacity(frame.rgba.len() / 4 * 3);
        for pixel in frame.rgba.as_chunks::<4>().0 {
            rgb.extend_from_slice(&pixel[..3]);
        }
        image::save_buffer_with_format(
            path,
            &rgb,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgb8,
            format,
        )?;
    } else {
        image::save_buffer_with_format(
            path,
            &frame.rgba,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgba8,
            format,
        )?;
    }
    Ok(())
}

/// The output image size the configuration asks for.
fn output_size(config: &Config) -> Result<(u32, u32), Error> {
    let size = config
        .output
        .size
        .or(config.output.render_size)
        .ok_or_else(|| {
            Error::Config("set output.size (or output.render_size) to the image size".to_owned())
        })?;
    if size.0 == 0 || size.1 == 0 {
        return Err(Error::Config(
            "the output size must have non-zero dimensions".to_owned(),
        ));
    }
    Ok(size)
}

/// Give `pass` an output-sized colour target, and its optional depth and
/// multisample attachments, and return the colour texture.
///
/// The texture is registered in the graph in its own right, so a screenshot can
/// name it and read it back, and the view depends on it so the graph keeps it
/// alive.
fn bind_target(
    world: &World,
    context: RenderContext,
    pass: Entity,
    device: &wgpu::Device,
    size: (u32, u32),
    samples: u32,
    with_depth: bool,
) -> wgpu::Texture {
    let color = create_color_target(device, COLOR_FORMAT, size.0, size.1);
    let color_view = {
        let mut graph = world
            .root_scope()
            .query_one::<&mut ResourceGraph>(context.graph)
            .expect("the context's resource graph exists");
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments =
        FrameAttachments::new(world, context, COLOR_FORMAT, size, samples, with_depth);
    world
        .root_scope()
        .query_one::<&mut FramePass>(pass)
        .map(|mut pass| {
            pass.set_attachments(&attachments, color_view);
        })
        .expect("the pass entity exists");
    color
}

/// Run f with the mesh source mounted at source_entity, borrowing both at once.
fn with_mesh_source<R>(
    world: &World,
    source_entity: Entity,
    f: impl FnOnce(&mut MeshSource, &Scope<'_>) -> R,
) -> R {
    // The scope is the source's own, so what its closure reads is the subtree
    // the source draws.
    let scope = world
        .scope(source_entity)
        .expect("the source entity is alive");
    let mut source = scope
        .query_one::<&mut Source>(source_entity)
        .expect("the mesh source entity exists");
    let mesh = source
        .as_mut::<MeshSource>()
        .expect("the source is a MeshSource");
    f(mesh, &scope)
}

/// Build the camera from the configuration and the render size's aspect ratio.
fn camera(config: &CameraConfig, render_size: (u32, u32)) -> Camera {
    if let (Some(view), Some(projection)) = (config.view, config.projection) {
        return Camera {
            view_from_world: Mat4::from_cols_array(&view),
            clip_from_view: Mat4::from_cols_array(&projection),
            active: true,
        };
    }
    let aspect = render_size.0 as f32 / render_size.1.max(1) as f32;
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        config.fov_y.to_radians(),
        aspect,
        config.z_near,
    );
    let view = glam::camera::rh::view::look_at_mat4(
        glam::Vec3::from(config.eye),
        glam::Vec3::from(config.target),
        glam::Vec3::from(config.up),
    );
    Camera {
        view_from_world: view,
        clip_from_view: projection,
        active: true,
    }
}

/// Resolve the animation a document reference names.
fn animation_index(document: &LitGltf, reference: &AnimationRef) -> Result<usize, Error> {
    match reference {
        AnimationRef::Index(index) => {
            if *index < document.animation_count() {
                Ok(*index)
            } else {
                Err(Error::Animation(format!(
                    "animation index {index} is out of range: the document has {} animations",
                    document.animation_count()
                )))
            }
        }
        AnimationRef::Name(name) => (0..document.animation_count())
            .find(|&animation| document.animation_name(animation) == Some(name.as_str()))
            .ok_or_else(|| {
                Error::Animation(format!(
                    "the document has no animation named {name}; it has {}",
                    animation_list(document)
                ))
            }),
    }
}

/// The comma-separated animation names of a document.
fn animation_list(document: &LitGltf) -> String {
    let mut list = String::new();
    for animation in 0..document.animation_count() {
        if animation > 0 {
            list.push_str(", ");
        }
        match document.animation_name(animation) {
            Some(name) => list.push_str(name),
            None => list.push_str(&format!("#{animation}")),
        }
    }
    if list.is_empty() {
        list.push_str("none");
    }
    list
}
