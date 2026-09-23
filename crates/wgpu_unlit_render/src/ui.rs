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
//! # use wgpu_unlit_render::ui::{screen_view, ui_options, EguiIntegration};
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
//! ui.update(&mut graph, queue, ctx, output, 1.0);
//! let scene = ui.scene(&mut graph);
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
    // depth.
    options.depth_stencil.depth_write_enabled = Some(false);
    options.depth_stencil.depth_compare = Some(wgpu::CompareFunction::Always);
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
            samplers: Vec::new(),
            materials: Vec::new(),
            vertices: None,
            indices: None,
            vertex_capacity: 0,
            index_capacity: 0,
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
    /// `pixels_per_point` tells egui how to rasterise text and how to scale
    /// the tessellated points.
    pub fn update(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
        pixels_per_point: f32,
    ) {
        self.apply_textures(graph, queue, &output.textures_delta);
        // egui panics if a delta is dropped unapplied, so mark it handled.
        output.textures_delta.clear();

        let primitives = ctx.tessellate(output.shapes, pixels_per_point);
        self.draws.clear();
        let (vertices, indices) = measure(&primitives);
        if vertices == 0 {
            return;
        }
        self.reserve(graph, vertices, indices);
        self.draws = upload(
            queue,
            self.vertices.as_ref().expect("just reserved"),
            self.indices.as_ref().expect("just reserved"),
            &primitives,
        );
    }

    /// The frame most recently uploaded, as a scene.
    ///
    /// Every draw sets a scissor rectangle, and a scissor stays set for the
    /// rest of the pass, so record this after the draws it overlays.
    pub fn scene<'ui>(&'ui mut self, graph: &'ui mut ResourceGraph) -> Scene<'ui> {
        if self.vertices.is_none() || self.indices.is_none() || self.draws.is_empty() {
            return Scene::new();
        }
        let mut scene = Scene::new();

        // Build every material the frame needs first: the scene borrows them
        // all, which it cannot do while any is still being inserted.
        let wanted: Vec<_> = self
            .draws
            .iter()
            .map(|draw| (draw.texture, draw.options))
            .collect();
        for (id, options) in wanted {
            self.build_material(graph, id, options);
        }

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
            // slices instead would double the base vertex.
            let entry = DrawEntry::new(
                pipeline,
                DrawRange::indexed(draw.indices.clone()).with_base_vertex(draw.first_vertex as i32),
            )
            .with_bind_group(GLOBAL_GROUP, global)
            .with_bind_group(MATERIAL_GROUP, material)
            .with_vertex_buffer(POSITION_SLOT, vertices.slice(..positions_size))
            .with_vertex_buffer(UV_COLOR_SLOT, vertices.slice(positions_size..))
            .with_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32)
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
fn scissor_rect(rect: egui::Rect) -> ScissorRect {
    let min = rect.min.max(egui::Pos2::ZERO);
    ScissorRect {
        x: min.x as u32,
        y: min.y as u32,
        width: (rect.max.x - min.x).max(0.0) as u32,
        height: (rect.max.y - min.y).max(0.0) as u32,
    }
}

/// Write `primitives` into `vertices` and `indices`, returning where each one
/// landed.
///
/// Positions and UV-and-colors go into two regions of the one buffer: wgpu
/// binds one stride per slot, and the two streams differ.
fn upload(
    queue: &wgpu::Queue,
    vertices: &wgpu::Buffer,
    indices: &wgpu::Buffer,
    primitives: &[egui::ClippedPrimitive],
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
            options: egui::TextureOptions::default(),
            scissor: scissor_rect(primitive.clip_rect),
        });
    }

    if !positions.is_empty() {
        queue.write_buffer(vertices, 0, &positions);
        queue.write_buffer(vertices, uv_color_start as u64, &uv_colors);
    }
    if !index_bytes.is_empty() {
        queue.write_buffer(indices, 0, &index_bytes);
    }
    draws
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
        // Overlaid rather than depth-tested.
        assert_eq!(options.depth_stencil.depth_write_enabled, Some(false));
        assert_eq!(
            options.depth_stencil.depth_compare,
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

    /// A clip rectangle reaching off screen must not wrap into a huge `u32`.
    #[test]
    fn scissor_rect_clamps_to_the_viewport() {
        let rect = scissor_rect(egui::Rect::from_two_pos(
            egui::Pos2::new(-20.0, -8.0),
            egui::Pos2::new(30.0, 20.0),
        ));
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
        // The far edge is not clamped: only the origin moves, so the
        // rectangle keeps the width the clipped draw covers.
        assert_eq!(rect.width, 30);
        assert_eq!(rect.height, 20);
    }
}
