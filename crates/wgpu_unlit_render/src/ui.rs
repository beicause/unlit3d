//! An [egui](https://egui.rs) backend built on the built-in unlit pipeline.
//!
//! [`EguiIntegration`] takes what egui produces each frame — tessellated, clipped
//! meshes plus a set of texture updates — and draws it with the built-in
//! pipeline as ordinary screen-space geometry. Nothing in the shader knows
//! about egui: the UI is premultiplied, textured, screen-space vertices, and
//! egui's clip rectangles become scissor rectangles.
//!
//! ```
//! # use wgpu_unlit_render::globals::Globals;
//! # use wgpu_unlit_render::pipeline::{
//! #     CAMERA_BINDING, FRAME_BINDING, UnlitOptions, UnlitPipeline,
//! # };
//! # use wgpu_unlit_render::resources::{Resource, ResourceGraph};
//! # use wgpu_unlit_render::ui::{EguiIntegration, ScreenDescriptor, screen_view, ui_options};
//! # use zerocopy::IntoBytes;
//! # fn frame(device: &wgpu::Device, queue: &wgpu::Queue, ctx: &egui::Context,
//! #          color_format: wgpu::TextureFormat, multisample: wgpu::MultisampleState) {
//! // The caller owns the globals: a camera uniform (written every frame with
//! // `screen_view`), a frame-globals uniform, and the bind group binding both.
//! let mut options = ui_options(device, /* the target encodes sRGB: */ true);
//! options.color_target.format = color_format;
//! options.multisample = multisample;
//! let pipeline = UnlitPipeline::new(device, &options);
//! let camera = uniform_buffer(device, "ui::camera");
//! let globals = uniform_buffer(device, "ui::globals");
//! let global_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
//!     label: Some("ui::globals"),
//!     layout: &pipeline.global_layout,
//!     entries: &[
//!         wgpu::BindGroupEntry {
//!             binding: CAMERA_BINDING,
//!             resource: camera.as_entire_binding(),
//!         },
//!         wgpu::BindGroupEntry {
//!             binding: FRAME_BINDING,
//!             resource: globals.as_entire_binding(),
//!         },
//!     ],
//! });
//!
//! // The UI's per-texture resources join the caller's ledger.
//! let mut graph = ResourceGraph::new();
//! let camera_id = graph
//!     .insert_strong(Resource::Buffer(camera), &[])
//!     .unwrap();
//! let globals_id = graph
//!     .insert_strong(Resource::Buffer(globals), &[])
//!     .unwrap();
//! let group_id = graph
//!     .insert_strong(Resource::BindGroup(global_group), &[camera_id, globals_id])
//!     .unwrap();
//!
//! let mut ui = EguiIntegration::new(device, group_id, pipeline);
//! let mut input = egui::RawInput::default();
//! input.screen_rect = Some(egui::Rect::from_min_size(
//!     egui::Pos2::ZERO,
//!     egui::Vec2::new(256.0, 192.0),
//! ));
//! let output = ctx.run_ui(input, |ui| {
//!     ui.label("hello world");
//! });
//! // The frame's encoder carries both the UI's staged vertex and index
//! // uploads and the pass that reads them; `scene` is recorded into it
//! // after this call.
//! let mut encoder = device.create_command_encoder(&Default::default());
//! // The target's physical size and its density, which the clip rectangles
//! // are scaled by.
//! let screen = ScreenDescriptor {
//!     size_in_pixels: [256, 192],
//!     pixels_per_point: 1.0,
//! };
//! ui.update(&mut graph, queue, &mut encoder, ctx, output, screen);
//! let scene = ui.scene(&graph);
//! # }
//! # fn uniform_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
//! #     device.create_buffer(&wgpu::BufferDescriptor {
//! #         label: Some(label),
//! #         size: 256,
//! #         usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
//! #         mapped_at_creation: false,
//! #     })
//! # }
//! ```

use crate::globals::View;
use crate::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, GLOBAL_GROUP, MATERIAL_GROUP,
    POSITION_SLOT, UV_COLOR_SLOT, UnlitFlags, UnlitOptions, UnlitPipeline,
};
use crate::resources::{Resource, ResourceGraph, ResourceId};
use crate::scene::{DrawEntry, DrawRange, Scene, ScissorRect};
use crate::specialize::SurfaceKey;
use crate::staging::StagingBuffer;
use core::ops::Range;
use hashbrown::HashMap;

/// egui's own texture format: gamma-space RGBA, never sRGB-aware.
///
/// egui's vertex colors and its font atlas are already gamma-encoded, so a
/// texture is sampled without conversion and the pipeline's own output
/// conversion deals with the target instead.
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Bytes one [`UnlitFlags::UNCOMPRESSED_POSITION`] vertex occupies.
const POSITION_STRIDE: usize = wgpu::VertexFormat::Float32x3.size() as usize;

/// Bytes one UV-and-color vertex occupies: `Float32x2` then `Unorm8x4`.
const UV_COLOR_STRIDE: usize =
    wgpu::VertexFormat::Float32x2.size() as usize + wgpu::VertexFormat::Unorm8x4.size() as usize;

/// The target a UI frame is drawn into and the density it is drawn at.
///
/// The two are needed together and in opposite units, which is the easiest
/// thing to get wrong here: egui tessellates in **logical points**, so the
/// projection takes the point size, while a scissor rectangle is in **physical
/// pixels**, so the clip rectangles are scaled by [`Self::pixels_per_point`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenDescriptor {
    /// The target's size in physical pixels.
    pub size_in_pixels: [u32; 2],
    /// How many physical pixels one logical point covers.
    pub pixels_per_point: f32,
}

impl ScreenDescriptor {
    /// The target's size in logical points, which is what the UI's projection
    /// and its `RawInput::screen_rect` are expressed in.
    pub fn size_in_points(self) -> [f32; 2] {
        [
            self.size_in_pixels[0] as f32 / self.pixels_per_point,
            self.size_in_pixels[1] as f32 / self.pixels_per_point,
        ]
    }
}

/// A world-to-clip matrix taking egui's tessellated points to clip space.
///
/// egui positions its vertices in logical points with the origin at the top
/// left and Y growing downwards, so the Y coefficient is negated; Z collapses
/// to zero, since everything the UI draws sits at one depth. The physical
/// density never enters: the target's format already encodes its pixels.
///
/// The caller writes this into their camera uniform, since the camera buffer
/// is theirs.
pub fn screen_view(viewport_points: [f32; 2]) -> View {
    let [width, height] = viewport_points;
    View::new(
        glam::Mat4::from_cols_array(&[
            2.0 / width,
            0.0,
            0.0,
            0.0,
            0.0,
            -2.0 / height,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            -1.0,
            1.0,
            0.0,
            1.0,
        ]),
        glam::Vec3::ZERO,
    )
}

/// The pipeline variant a UI is drawn with.
///
/// `srgb_to_linear_output` is the caller's call, not this function's: it
/// depends on the target's format, which this function never sees. Set it
/// when the target encodes sRGB.
pub fn ui_options(device: &wgpu::Device, srgb_to_linear_output: bool) -> UnlitOptions {
    let mut options = UnlitOptions::standard(device);
    apply_ui_settings(&mut options, srgb_to_linear_output);
    options
}

/// The UI variant's options for a frame that draws into `surface`.
///
/// Equivalent to [`ui_options`] followed by
/// [`apply_surface`](crate::pipeline::apply_surface), which is what makes the
/// result usable as-is: the color format, the sample count and the depth state
/// all match the target, so a caller never has to remember to specialize. The
/// sRGB flag is still the caller's call, since it describes the fragment's own
/// encoding rather than the attachment's format.
pub fn ui_options_for_surface(
    device: &wgpu::Device,
    srgb_to_linear_output: bool,
    surface: SurfaceKey,
) -> UnlitOptions {
    let mut options = ui_options(device, srgb_to_linear_output);
    crate::pipeline::apply_surface(&mut options, surface);
    options
}

/// Apply the UI variant's device-independent settings — flags, culling,
/// blending and the overlaid depth behavior — to a standard options set.
fn apply_ui_settings(options: &mut UnlitOptions, srgb_to_linear_output: bool) {
    // Full-precision screen-space vertices carrying a premultiplied color and
    // a texture coordinate: no compression, no per-instance stream.
    let mut flags = UnlitFlags::VERTEX_POSITION
        | UnlitFlags::UNCOMPRESSED_POSITION
        | UnlitFlags::VERTEX_UV
        | UnlitFlags::UNCOMPRESSED_UV
        | UnlitFlags::VERTEX_COLOR
        | UnlitFlags::BASE_COLOR_TEXTURE;
    if srgb_to_linear_output {
        flags |= UnlitFlags::SRGB_TO_LINEAR_OUTPUT;
    }
    options.flags = flags;
    // No culling: egui does not guarantee a consistent winding order across
    // the primitives it emits, so neither face can be discarded safely.
    // (`UnlitOptions::standard` culls back faces for closed meshes; the UI is
    // flat, overlay geometry with no inside to hide.)
    options.primitive.cull_mode = None;
    // egui tessellates premultiplied colors, so the source's RGB is added to
    // the destination scaled by the alpha it leaves behind.
    options.color_target.blend = Some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    });
    // The UI overlays whatever the pass holds, so it neither tests nor writes
    // depth. `apply_surface` later decides whether the target has a depth
    // attachment at all; a state left here is the overlaid one, and a target
    // without depth drops it.
    if let Some(depth_stencil) = &mut options.depth_stencil {
        depth_stencil.depth_write_enabled = Some(false);
        depth_stencil.depth_compare = Some(wgpu::CompareFunction::Always);
    }
}

/// Vertex count and index count the buffers must hold for `primitives`.
fn measure(primitives: &[egui::ClippedPrimitive]) -> (usize, usize) {
    let mut vertices = 0usize;
    let mut indices = 0usize;
    for mesh in primitives.iter().filter_map(mesh_of) {
        vertices += mesh.vertices.len();
        indices += mesh.indices.len();
    }
    (vertices, indices)
}

/// One uploaded primitive: where its vertices and indices sit, what it samples
/// and how it is clipped.
struct UiDraw {
    /// Index of the primitive's first vertex, which indexes both vertex
    /// streams at once: the two slots carry one vertex each at the same
    /// stride, so one number addresses the position, the UV and the color.
    first_vertex: u32,
    /// Index range into the index buffer, counting indices.
    indices: Range<u32>,
    texture: egui::TextureId,
    options: egui::TextureOptions,
    scissor: ScissorRect,
}

/// Draws tessellated egui output with the built-in unlit pipeline.
///
/// The UI's GPU resources are registered in a [`ResourceGraph`] the caller
/// supplies — normally the one kept alongside the
/// [`RenderAttachments`](crate::render_attachments::RenderAttachments) — so
/// the whole frame shares one ledger. Every method that touches the graph
/// takes it as an argument: the renderer holds only the bookkeeping (ids,
/// keys) that maps egui's world onto graph nodes.
pub struct EguiIntegration {
    /// Kept to build the resources a frame turns out to need.
    device: wgpu::Device,
    pipeline: UnlitPipeline,
    /// Graph node of the caller's global bind group: camera and frame
    /// globals, written by the caller.
    global_group: ResourceId,
    /// Graph node of the egui texture *view* of every allocated texture
    /// slot, keyed by egui's own id; the view depends on its texture node.
    textures: HashMap<egui::TextureId, ResourceId>,
    /// The sampling options egui last stated for each texture.
    ///
    /// Kept separately from the texture itself because every [`ImageDelta`]
    /// carries the options that apply to the whole texture, and the draw that
    /// samples it has to name the matching sampler:
    /// [`Self::sampler`] keys its cache on exactly these options, so a texture
    /// uploaded as nearest samples nearest.
    ///
    /// [`ImageDelta`]: egui::epaint::ImageDelta
    texture_options: HashMap<egui::TextureId, egui::TextureOptions>,
    /// One sampler per distinct set of egui sampling options seen, with its
    /// graph node.
    samplers: Vec<(egui::TextureOptions, ResourceId)>,
    /// One material bind group per (texture, options) pair, with its graph
    /// node; the node depends on the texture's view and the sampler.
    materials: Vec<(MaterialKey, ResourceId)>,
    /// Positions, then interleaved UVs and colors.
    vertices: Option<wgpu::Buffer>,
    /// `Uint32` indices.
    indices: Option<wgpu::Buffer>,
    /// Vertices the vertex buffer holds room for.
    vertex_capacity: usize,
    /// Indices the index buffer holds room for.
    index_capacity: usize,
    /// Staging buffer for the per-frame vertex uploads, reused across frames.
    vertex_staging: StagingBuffer,
    /// Staging buffer for the per-frame index uploads, reused across frames.
    index_staging: StagingBuffer,
    /// Layout of the frame most recently uploaded.
    draws: Vec<UiDraw>,
}

/// What one material bind group was built from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct MaterialKey {
    texture: egui::TextureId,
    options: egui::TextureOptions,
}

impl EguiIntegration {
    /// Build the UI integration around a caller-supplied `pipeline`,
    /// registering the UI's per-texture resources in `graph`.
    ///
    /// `pipeline` is the built-in unlit pipeline the UI draws with, configured
    /// for the target the pass renders into — its color format, sample count
    /// and blend state. The caller builds it (and the global bind group from
    /// its layout) up front, so the UI does not compile a pipeline of its own.
    ///
    /// The global bind group — the camera and frame-globals uniforms — is
    /// the caller's, as is everything else the UI overlays: the integration
    /// owns only egui's textures and the geometry they are drawn with.
    /// Sharing the caller's graph — usually the one kept alongside the
    /// [`crate::render_attachments::RenderAttachments`] — means those
    /// resources live in the same ledger as the rest of the frame's, with the
    /// same dependency tracking.
    pub fn new(device: &wgpu::Device, global_group: ResourceId, pipeline: UnlitPipeline) -> Self {
        Self {
            device: device.clone(),
            pipeline,
            global_group,
            textures: HashMap::new(),
            texture_options: HashMap::new(),
            samplers: Vec::new(),
            materials: Vec::new(),
            vertices: None,
            indices: None,
            vertex_capacity: 0,
            index_capacity: 0,
            vertex_staging: StagingBuffer::new(),
            index_staging: StagingBuffer::new(),
            draws: Vec::new(),
        }
    }

    /// The pipeline the UI draws with.
    pub fn pipeline(&self) -> &UnlitPipeline {
        &self.pipeline
    }

    /// The bind group behind a graph node that is known to be one.
    fn bind_group<'a>(&self, graph: &'a ResourceGraph, id: ResourceId) -> &'a wgpu::BindGroup {
        graph
            .get(id)
            .and_then(Resource::as_bind_group)
            .expect("the node is a bind group the UI created")
    }

    /// Apply `output`'s texture updates and upload its tessellated shapes,
    /// ready for [`Self::scene`].
    ///
    /// The vertex and index bytes change every frame, so they are staged
    /// through `encoder`: `scene`'s pass is recorded into the same encoder
    /// afterwards, and one submission carries the upload and the draw that
    /// reads it. Texture updates still go through `queue`.
    ///
    /// `screen` tells egui how to rasterise text and scales the tessellated points
    /// and their clip rectangles; see [`ScreenDescriptor`].
    pub fn update(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
        screen: ScreenDescriptor,
    ) {
        self.apply_textures(graph, queue, &output.textures_delta);
        // egui panics if a delta is dropped unapplied, so mark it handled.
        output.textures_delta.clear();

        let primitives = ctx.tessellate(output.shapes, screen.pixels_per_point);
        self.draws.clear();
        let (vertices, indices) = measure(&primitives);
        if vertices == 0 {
            return;
        }
        self.reserve(graph, vertices, indices);
        self.draws = self.upload_geometry(encoder, &primitives, screen);

        // Build every material the frame needs here, while the graph is still
        // mutably available: `scene` only reads it, so a material that does
        // not exist yet by then is one this frame cannot draw.
        let wanted: Vec<_> = self
            .draws
            .iter()
            .map(|draw| (draw.texture, draw.options))
            .collect();
        for (id, options) in wanted {
            self.build_material(graph, id, options);
        }
    }

    /// Pack `primitives` into the vertex and index buffers, staging the bytes
    /// through `encoder`, and return where each one landed.
    ///
    /// Positions and UV-and-colors go into two regions of the one buffer: wgpu
    /// binds one stride per slot, and the two streams differ.
    fn upload_geometry(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        primitives: &[egui::ClippedPrimitive],
        screen: ScreenDescriptor,
    ) -> Vec<UiDraw> {
        let (vertex_count, _) = measure(primitives);
        let uv_color_start = vertex_count * POSITION_STRIDE;

        let mut positions = Vec::with_capacity(uv_color_start);
        let mut uv_colors = Vec::with_capacity(vertex_count * UV_COLOR_STRIDE);
        let mut index_bytes = Vec::new();
        let mut draws = Vec::with_capacity(primitives.len());

        for primitive in primitives {
            // A paint callback draws its own way; this renderer handles
            // tessellated meshes only.
            let Some(mesh) = mesh_of(primitive) else {
                continue;
            };
            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                continue;
            }

            let first_vertex = (positions.len() / POSITION_STRIDE) as u32;
            let indices_start = index_bytes.len() / size_of::<u32>();

            for vertex in &mesh.vertices {
                positions.extend_from_slice(&vertex.pos.x.to_le_bytes());
                positions.extend_from_slice(&vertex.pos.y.to_le_bytes());
                // The third component is unused: the UI is flat.
                positions.extend_from_slice(&0f32.to_le_bytes());
                uv_colors.extend_from_slice(&vertex.uv.x.to_le_bytes());
                uv_colors.extend_from_slice(&vertex.uv.y.to_le_bytes());
                // egui's colors are premultiplied already, straight to Unorm8.
                uv_colors.extend_from_slice(&vertex.color.to_array());
            }
            for &index in &mesh.indices {
                index_bytes.extend_from_slice(&index.to_le_bytes());
            }

            draws.push(UiDraw {
                first_vertex,
                indices: indices_start as u32..(indices_start + mesh.indices.len()) as u32,
                texture: mesh.texture_id,
                // The texture's own sampling options, not a default: a texture
                // egui asked to sample nearest or to tile would otherwise be
                // sampled linearly and clamped, and the sampler that matches
                // `options` is the one the material binds.
                options: self
                    .texture_options
                    .get(&mesh.texture_id)
                    .copied()
                    .unwrap_or_default(),
                scissor: scissor_rect(primitive.clip_rect, screen),
            });
        }

        let vertices = self.vertices.as_ref().expect("just reserved").clone();
        let indices = self.indices.as_ref().expect("just reserved").clone();
        let device = &self.device;
        // The two streams are adjacent regions of the one buffer, so they go in
        // as one upload: one staging buffer, one copy.
        positions.extend_from_slice(&uv_colors);
        self.vertex_staging
            .write(device, encoder, &vertices, 0, &positions);
        self.index_staging
            .write(device, encoder, &indices, 0, &index_bytes);
        draws
    }

    /// The frame most recently uploaded, as a scene.
    ///
    /// Every draw sets a scissor rectangle, and a scissor stays set for the
    /// rest of the pass, so record this after the draws it overlays.
    ///
    /// The materials the frame needs were built by [`Self::update`], so this
    /// only reads the graph — a scene can therefore be taken while the graph is
    /// shared with whatever else the frame is drawing.
    pub fn scene(&self, graph: &ResourceGraph) -> Scene {
        if self.vertices.is_none() || self.indices.is_none() || self.draws.is_empty() {
            return Scene::new();
        }
        let mut scene = Scene::new();

        let (vertices, indices) = (
            self.vertices.as_ref().expect("checked"),
            self.indices.as_ref().expect("checked"),
        );
        // The position stream is the first half of the buffer, the interleaved
        // UVs and colors the second; each holds `vertex_capacity` vertices.
        let positions_size = (self.vertex_capacity * POSITION_STRIDE) as u64;
        let pipeline = &self.pipeline.pipeline;
        let global = self.bind_group(graph, self.global_group);
        for draw in &self.draws {
            // Each draw carries its own scissor rectangle, so consecutive
            // draws sharing one are what the recorder's deduplication is for.
            let Some(material) = self.material(graph, draw.texture, draw.options) else {
                // A primitive whose texture was freed this frame has nothing
                // to sample, so it is skipped rather than bound to a stale view.
                continue;
            };
            // Both slots cover their whole stream: every vertex of the frame
            // sits at the same ordinal in each, so one base vertex addresses
            // the position, the UV and the color together. Offsetting the
            // ranges instead would double the base vertex.
            let entry = DrawEntry::new(
                pipeline,
                DrawRange::indexed(draw.indices.clone()).with_base_vertex(draw.first_vertex as i32),
            )
            .with_bind_group(GLOBAL_GROUP, global)
            .with_bind_group(MATERIAL_GROUP, material)
            .with_vertex_buffer_range(POSITION_SLOT, vertices, 0..positions_size)
            .with_vertex_buffer_range(UV_COLOR_SLOT, vertices, positions_size..vertices.size())
            .with_index_buffer(indices, wgpu::IndexFormat::Uint32)
            .with_scissor(draw.scissor);
            scene.push(entry);
        }
        scene
    }

    /// Reallocate the buffers when they cannot hold `vertices` and `indices`,
    /// registering any new buffer in `graph`.
    fn reserve(&mut self, graph: &mut ResourceGraph, vertices: usize, indices: usize) {
        // Grow geometrically so a UI that grows over a few frames does not
        // reallocate every frame.
        if self.vertices.is_none() || self.vertex_capacity < vertices {
            self.vertex_capacity = vertices.max(self.vertex_capacity * 2).max(1);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui::vertices"),
                size: (self.vertex_capacity * (POSITION_STRIDE + UV_COLOR_STRIDE)) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            graph
                .insert_strong(Resource::Buffer(buffer.clone()), &[])
                .expect("an empty dependency list always resolves");
            self.vertices = Some(buffer);
        }
        if self.indices.is_none() || self.index_capacity < indices {
            self.index_capacity = indices.max(self.index_capacity * 2).max(1);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui::indices"),
                size: (self.index_capacity * size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            graph
                .insert_strong(Resource::Buffer(buffer.clone()), &[])
                .expect("an empty dependency list always resolves");
            self.indices = Some(buffer);
        }
    }

    /// The material bind group for (`id`, `options`), if it exists.
    fn material<'a>(
        &self,
        graph: &'a ResourceGraph,
        id: egui::TextureId,
        options: egui::TextureOptions,
    ) -> Option<&'a wgpu::BindGroup> {
        let key = MaterialKey {
            texture: id,
            options,
        };
        let index = self.materials.iter().position(|(seen, _)| *seen == key)?;
        graph.get(self.materials[index].1)?.as_bind_group()
    }

    /// Build the material bind group for (`id`, `options`) if it does not
    /// exist, registered in the graph as a dependent of its texture's view
    /// and its sampler.
    fn build_material(
        &mut self,
        graph: &mut ResourceGraph,
        id: egui::TextureId,
        options: egui::TextureOptions,
    ) {
        let key = MaterialKey {
            texture: id,
            options,
        };
        if self.materials.iter().any(|(seen, _)| *seen == key) {
            return;
        }
        let Some(texture) = self.textures.get(&id).copied() else {
            return;
        };
        let sampler_id = self.sampler(graph, options);
        let Some(layout) = self.pipeline.material_layout.as_ref() else {
            return;
        };
        let Some(Resource::TextureView { view, .. }) = graph.get(texture) else {
            return;
        };
        let Some(Resource::Sampler(sampler)) = graph.get(sampler_id) else {
            return;
        };
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui::material"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        let id = graph
            .insert_strong(Resource::BindGroup(group), &[texture, sampler_id])
            .expect("both dependencies were registered");
        self.materials.push((key, id));
    }

    /// The sampler for `options`, created and registered on first use.
    fn sampler(&mut self, graph: &mut ResourceGraph, options: egui::TextureOptions) -> ResourceId {
        if let Some(&(_, id)) = self.samplers.iter().find(|(seen, _)| *seen == options) {
            return id;
        }
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui::sampler"),
            address_mode_u: address_mode(options.wrap_mode),
            address_mode_v: address_mode(options.wrap_mode),
            mag_filter: texture_filter(options.magnification),
            min_filter: texture_filter(options.minification),
            ..Default::default()
        });
        let id = graph
            .insert_strong(Resource::Sampler(sampler), &[])
            .expect("an empty dependency list always resolves");
        self.samplers.push((options, id));
        id
    }

    fn apply_textures(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        delta: &egui::TexturesDelta,
    ) {
        for (&id, deltas) in &delta.set {
            for image in deltas {
                // Every delta for a texture states its sampling options, and
                // the last one wins: they describe the texture as a whole, not
                // the patch being written.
                self.texture_options.insert(id, image.options);
                self.upload_texture(graph, queue, id, image);
            }
        }
        for &id in &delta.free {
            // Removing the texture drops its view, and with it every
            // material that samples it — the graph propagates the removal
            // along the dependency edges.
            if let Some(view) = self.textures.remove(&id) {
                graph.remove_drop(view);
            }
            self.texture_options.remove(&id);
            // A material whose nodes were removed no longer resolves; drop
            // its bookkeeping entry so it can be rebuilt if egui reuses the
            // (texture, options) pair.
            self.materials.retain(|(key, _)| key.texture != id);
        }
    }

    fn upload_texture(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        id: egui::TextureId,
        delta: &egui::epaint::ImageDelta,
    ) {
        use zerocopy::IntoBytes;

        let [width, height] = delta.image.size();
        let pixels: Vec<[u8; 4]> = match &delta.image {
            egui::ImageData::Color(image) => {
                image.pixels.iter().map(egui::Color32::to_array).collect()
            }
        };
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width as u32 * texture_bytes()),
            rows_per_image: Some(height as u32),
        };
        let size = wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        };

        match delta.pos {
            // A patch written into the texture already there.
            Some([x, y]) => {
                let Some(&texture_id) = self.textures.get(&id) else {
                    return;
                };
                let Some(Resource::Texture(texture)) = graph.get(texture_id) else {
                    return;
                };
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: x as u32,
                            y: y as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    pixels.as_bytes(),
                    layout,
                    size,
                );
            }
            None => {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("ui::texture"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: TEXTURE_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(texture.as_image_copy(), pixels.as_bytes(), layout, size);
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let texture_id = graph
                    .insert_strong(Resource::Texture(texture), &[])
                    .expect("an empty dependency list always resolves");
                let view_id = graph
                    .insert_strong(view, &[texture_id])
                    .expect("the texture was just registered");
                self.textures.insert(id, view_id);
            }
        }
    }
}

/// Bytes per texel of [`TEXTURE_FORMAT`], derived from the format.
fn texture_bytes() -> u32 {
    TEXTURE_FORMAT
        .block_copy_size(None)
        .expect("rgba8 has a known texel size")
}

fn address_mode(mode: egui::TextureWrapMode) -> wgpu::AddressMode {
    match mode {
        egui::TextureWrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        egui::TextureWrapMode::Repeat => wgpu::AddressMode::Repeat,
        egui::TextureWrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
    }
}

fn texture_filter(filter: egui::TextureFilter) -> wgpu::FilterMode {
    match filter {
        egui::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        egui::TextureFilter::Linear => wgpu::FilterMode::Linear,
    }
}

/// The mesh of `primitive`, or `None` for a paint callback.
fn mesh_of(primitive: &egui::ClippedPrimitive) -> Option<&egui::epaint::Mesh> {
    match &primitive.primitive {
        egui::epaint::Primitive::Mesh(mesh) => Some(mesh),
        egui::epaint::Primitive::Callback(_) => None,
    }
}

/// egui's clip rectangle as a scissor rectangle, in whole pixels.
///
/// egui states a clip rectangle in logical points, while a scissor rectangle is
/// in physical pixels, so the rectangle is scaled by the density and rounded to
/// whole pixels — the same conversion `egui-wgpu` performs. Both edges are then
/// clamped to the target: a clip rectangle may extend past it (a panel scrolled
/// out of view, or a widget larger than the surface), and a scissor rectangle
/// outside the attachment is a validation error rather than a no-op.
fn scissor_rect(clip_rect: egui::Rect, screen: ScreenDescriptor) -> ScissorRect {
    let ppp = screen.pixels_per_point;
    let [width, height] = screen.size_in_pixels;

    let min_x = (ppp * clip_rect.min.x).round().clamp(0.0, width as f32) as u32;
    let min_y = (ppp * clip_rect.min.y).round().clamp(0.0, height as f32) as u32;
    let max_x = (ppp * clip_rect.max.x)
        .round()
        .clamp(min_x as f32, width as f32) as u32;
    let max_y = (ppp * clip_rect.max.y)
        .round()
        .clamp(min_y as f32, height as f32) as u32;

    ScissorRect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UI options set for tests that have no device. Only the fields these
    /// tests assert on matter, so the depth-stencil format is the
    /// device-independent placeholder.
    fn ui_options_for_tests(srgb_to_linear_output: bool) -> UnlitOptions {
        let mut options = UnlitOptions::standard_shape();
        apply_ui_settings(&mut options, srgb_to_linear_output);
        options
    }

    #[test]
    fn ui_variant_is_screen_space_and_uncompressed() {
        let options = ui_options_for_tests(false);
        let flags = options.flags;
        assert!(flags.contains(UnlitFlags::VERTEX_POSITION));
        assert!(flags.contains(UnlitFlags::UNCOMPRESSED_POSITION));
        assert!(flags.contains(UnlitFlags::VERTEX_UV));
        assert!(flags.contains(UnlitFlags::UNCOMPRESSED_UV));
        assert!(flags.contains(UnlitFlags::VERTEX_COLOR));
        assert!(flags.contains(UnlitFlags::BASE_COLOR_TEXTURE));
        // Nothing per-instance: the vertices are already in the projection's
        // space, and nothing is compressed, so nothing needs decoding.
        assert!(!flags.contains(UnlitFlags::VERTEX_INSTANCE));
        assert!(!options.needs_metadata());
        // Overlaid rather than depth-tested. The base options carry a depth
        // state, so the UI's overlaid state is the one left behind.
        let depth_stencil = options
            .depth_stencil
            .expect("the base options carry a depth state");
        assert_eq!(depth_stencil.depth_write_enabled, Some(false));
        assert_eq!(
            depth_stencil.depth_compare,
            Some(wgpu::CompareFunction::Always)
        );
        // egui's colors premultiply their own alpha.
        let blend = options.color_target.blend.expect("the UI variant blends");
        assert_eq!(blend.color.src_factor, wgpu::BlendFactor::One);
        assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    }

    /// The conversion flag follows the parameter, since it depends on the
    /// target's format, not the UI.
    #[test]
    fn srgb_flag_follows_the_parameter() {
        assert!(
            !ui_options_for_tests(false)
                .flags
                .contains(UnlitFlags::SRGB_TO_LINEAR_OUTPUT)
        );
        assert!(
            ui_options_for_tests(true)
                .flags
                .contains(UnlitFlags::SRGB_TO_LINEAR_OUTPUT)
        );
    }

    /// The projection maps the viewport's corners onto clip space, with Y
    /// flipped: egui's origin is the top left, clip space's the bottom left.
    #[test]
    fn screen_view_maps_the_viewport_corners() {
        let view = screen_view([256.0, 192.0]);
        for (point, expected) in [([0.0, 0.0], [-1.0, 1.0]), ([256.0, 192.0], [1.0, -1.0])] {
            let clip = view.clip_from_world * glam::Vec4::new(point[0], point[1], 0.0, 1.0);
            let got = [clip.x / clip.w, clip.y / clip.w];
            assert!(
                (got[0] - expected[0]).abs() < 1e-5 && (got[1] - expected[1]).abs() < 1e-5,
                "point {point:?} projected to {got:?}, expected {expected:?}"
            );
        }
    }

    /// A clip rectangle reaching off the target is clamped rather than wrapping
    /// into a huge `u32`, and its far edge never falls below its near edge.
    #[test]
    fn scissor_rect_clamps_to_the_target() {
        let screen = ScreenDescriptor {
            size_in_pixels: [64, 48],
            pixels_per_point: 1.0,
        };
        let rect = scissor_rect(
            egui::Rect::from_two_pos(egui::Pos2::new(-20.0, -8.0), egui::Pos2::new(200.0, 200.0)),
            screen,
        );
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
        // Both edges are clamped to the target, so a clip rectangle larger
        // than it covers exactly the target rather than overrunning it.
        assert_eq!(rect.width, 64);
        assert_eq!(rect.height, 48);

        // A rectangle entirely off the target collapses to nothing instead of
        // producing a negative width.
        let off = scissor_rect(
            egui::Rect::from_two_pos(egui::Pos2::new(-40.0, -40.0), egui::Pos2::new(-10.0, -10.0)),
            screen,
        );
        assert_eq!((off.x, off.y, off.width, off.height), (0, 0, 0, 0));
    }

    /// A clip rectangle is stated in logical points, so at a density above one
    /// it must cover that many physical pixels — the conversion egui-wgpu
    /// performs. Getting it wrong clips the UI short of its own bounds.
    #[test]
    fn scissor_rect_scales_points_to_physical_pixels() {
        let screen = ScreenDescriptor {
            size_in_pixels: [256, 192],
            pixels_per_point: 2.0,
        };
        // A 16x8-point clip at (8, 4) is 32x16 physical pixels at (16, 8).
        let clip = egui::Rect::from_min_size(egui::Pos2::new(8.0, 4.0), egui::Vec2::new(16.0, 8.0));
        let rect = scissor_rect(clip, screen);
        assert_eq!(
            rect,
            ScissorRect {
                x: 16,
                y: 8,
                width: 32,
                height: 16
            }
        );

        // Where an unscaled conversion would have ended, in pixels: egui's own
        // point coordinates taken as pixels. The scaled rectangle extends
        // beyond it, so an unscaled clip would have cut that strip away.
        let unscaled_end = clip.max.x as u32;
        assert!(
            unscaled_end < rect.x + rect.width,
            "the scaled rectangle must reach past where an unscaled one would \
             have ended ({unscaled_end} px), but it ends at {} px",
            rect.x + rect.width
        );
    }

    /// Fractional points round to whole pixels rather than truncating, so a
    /// clip edge does not drift by up to a pixel.
    #[test]
    fn scissor_rect_rounds_fractional_points() {
        let screen = ScreenDescriptor {
            size_in_pixels: [64, 64],
            pixels_per_point: 1.0,
        };
        let rect = scissor_rect(
            egui::Rect::from_min_size(egui::Pos2::new(1.4, 2.6), egui::Vec2::new(4.0, 4.0)),
            screen,
        );
        assert_eq!((rect.x, rect.y), (1, 3));

        let rounded_up = scissor_rect(
            egui::Rect::from_min_size(egui::Pos2::new(1.6, 2.4), egui::Vec2::new(4.0, 4.0)),
            screen,
        );
        assert_eq!((rounded_up.x, rounded_up.y), (2, 2));
    }

    /// The point size the projection takes is the physical size divided by the
    /// density — the opposite conversion from the scissor rectangle's.
    #[test]
    fn size_in_points_divides_by_the_density() {
        let screen = ScreenDescriptor {
            size_in_pixels: [256, 192],
            pixels_per_point: 2.0,
        };
        assert_eq!(screen.size_in_points(), [128.0, 96.0]);
    }
}
