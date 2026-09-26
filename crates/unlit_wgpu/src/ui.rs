//! An [egui](https://egui.rs) backend built on the built-in unlit pipeline.
//!
//! [`EguiIntegration`] takes what egui produces each frame — tessellated, clipped
//! meshes plus a set of texture updates — and draws it with the built-in
//! pipeline as ordinary screen-space geometry. Nothing in the shader knows
//! about egui: the UI is premultiplied, textured, screen-space vertices, and
//! egui's clip rectangles become scissor rectangles.
//!
//! ```
//! # use unlit_wgpu::globals::Globals;
//! # use unlit_wgpu::pipeline::{
//! #     CAMERA_BINDING, FRAME_BINDING, UnlitOptions, UnlitPipeline,
//! # };
//! # use unlit_wgpu::resources::{Resource, ResourceGraph};
//! # use unlit_wgpu::ui::{EguiIntegration, ScreenDescriptor, screen_view, ui_options};
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

/// How much geometry a frame holds: the two numbers the packing and the draw
/// ranges both need, and which must agree between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GeometryCounts {
    vertices: usize,
    indices: usize,
}

/// Count the vertices and indices every tessellated primitive in `primitives`
/// contributes.
fn measure(primitives: &[egui::ClippedPrimitive]) -> GeometryCounts {
    let mut counts = GeometryCounts {
        vertices: 0,
        indices: 0,
    };
    for mesh in primitives.iter().filter_map(mesh_of) {
        counts.vertices += mesh.vertices.len();
        counts.indices += mesh.indices.len();
    }
    counts
}

/// Pack `primitives` into `vertices` and `indices`, and fill `draws` with where
/// each one landed.
///
/// Both byte buffers are cleared and resized rather than reallocated, so a
/// caller that keeps them across frames reuses the allocation. The vertex
/// buffer holds the position stream first and the interleaved UV-and-color
/// stream second: wgpu binds one stride per slot and the two differ.
///
/// `counts` must be what [`measure`] returned for `primitives`: it is both how
/// much is written and where the two vertex streams meet.
fn pack_geometry(
    primitives: &[egui::ClippedPrimitive],
    screen: ScreenDescriptor,
    texture_options: &HashMap<egui::TextureId, egui::TextureOptions>,
    counts: GeometryCounts,
    vertices: &mut Vec<u8>,
    indices: &mut Vec<u8>,
    draws: &mut Vec<UiDraw>,
) {
    use zerocopy::IntoBytes;

    // The split between the two streams is the frame's own vertex count, which
    // is what `scene` reads back. It is *not* the buffer's capacity: that is
    // usually larger, and splitting on it would write the UVs and colors
    // somewhere the draws do not look for them.
    let uv_color_start = counts.vertices * POSITION_STRIDE;
    let vertex_count = counts.vertices;
    let index_count = counts.indices;

    vertices.clear();
    vertices.resize(uv_color_start + vertex_count * UV_COLOR_STRIDE, 0);
    indices.clear();
    indices.resize(index_count * size_of::<u32>(), 0);
    draws.clear();

    let mut vertex_cursor = 0usize;
    let mut index_cursor = 0usize;
    for primitive in primitives {
        // A paint callback draws its own way; this renderer handles tessellated
        // meshes only.
        let Some(mesh) = mesh_of(primitive) else {
            continue;
        };
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            continue;
        }

        let first_vertex = vertex_cursor as u32;
        let first_index = index_cursor as u32;

        for vertex in &mesh.vertices {
            let position = vertex_cursor * POSITION_STRIDE;
            // The third component is unused: the UI is flat.
            vertices[position..position + POSITION_STRIDE]
                .copy_from_slice([vertex.pos.x, vertex.pos.y, 0.0].as_bytes());
            let uv_color = uv_color_start + vertex_cursor * UV_COLOR_STRIDE;
            vertices[uv_color..uv_color + size_of::<egui::Vec2>()]
                .copy_from_slice([vertex.uv.x, vertex.uv.y].as_bytes());
            // egui's colors are premultiplied already, straight to Unorm8.
            vertices[uv_color + size_of::<egui::Vec2>()..uv_color + UV_COLOR_STRIDE]
                .copy_from_slice(&vertex.color.to_array());
            vertex_cursor += 1;
        }

        let index_start = index_cursor * size_of::<u32>();
        for (slot, &index) in indices[index_start..]
            .as_chunks_mut::<{ size_of::<u32>() }>()
            .0
            .iter_mut()
            .zip(&mesh.indices)
        {
            slot.copy_from_slice(index.as_bytes());
        }
        index_cursor += mesh.indices.len();

        draws.push(UiDraw {
            first_vertex,
            indices: first_index..index_cursor as u32,
            texture: mesh.texture_id,
            // The texture's own sampling options, not a default: a texture egui
            // asked to sample nearest or to tile would otherwise be sampled
            // linearly and clamped, and the sampler that matches `options` is
            // the one the material binds.
            options: texture_options
                .get(&mesh.texture_id)
                .copied()
                .unwrap_or_default(),
            scissor: scissor_rect(primitive.clip_rect, screen),
        });
    }
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

/// The graph nodes backing one egui texture slot.
///
/// A slot needs both: the texture, because egui patches its own atlases with
/// partial updates and a patch is written into the texture, and the view,
/// because a material bind group samples the view. They are two nodes with an
/// edge between them, so a patched texture and everything built from it stay
/// the same resources.
#[derive(Clone, Copy)]
struct TextureSlot {
    /// The texture egui's deltas are written into.
    texture: ResourceId,
    /// The default view over it, which the material samples.
    view: ResourceId,
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
    /// Graph nodes of every allocated texture slot, keyed by egui's own id.
    textures: HashMap<egui::TextureId, TextureSlot>,
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
    /// Graph node of the vertex buffer, replaced in place when it grows.
    vertex_node: Option<ResourceId>,
    /// Graph node of the index buffer, replaced in place when it grows.
    index_node: Option<ResourceId>,
    /// Vertices the vertex buffer holds room for.
    vertex_capacity: usize,
    /// Indices the index buffer holds room for.
    index_capacity: usize,
    /// Vertices the most recently uploaded frame holds. This is where its two
    /// vertex streams meet, so [`Self::scene`] has to split at the same point
    /// [`Self::upload_geometry`] packed at — which is not the capacity whenever
    /// the buffer has room to spare.
    frame_vertices: usize,
    /// Staging buffer for the per-frame vertex uploads, reused across frames.
    vertex_staging: StagingBuffer,
    /// Staging buffer for the per-frame index uploads, reused across frames.
    index_staging: StagingBuffer,
    /// Layout of the frame most recently uploaded.
    draws: Vec<UiDraw>,
    /// Packing scratch for the vertex stream — positions then interleaved UVs
    /// and colors — kept so a steady UI reuses one allocation instead of taking
    /// a fresh one every frame.
    packed_vertices: Vec<u8>,
    /// Packing scratch for the index stream, reused the same way.
    packed_indices: Vec<u8>,
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
            vertex_node: None,
            index_node: None,
            vertex_capacity: 0,
            index_capacity: 0,
            frame_vertices: 0,
            vertex_staging: StagingBuffer::new(),
            index_staging: StagingBuffer::new(),
            draws: Vec::new(),
            packed_vertices: Vec::new(),
            packed_indices: Vec::new(),
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
        let counts = measure(&primitives);
        if counts.vertices == 0 {
            self.draws.clear();
            return;
        }
        self.reserve(graph, counts);
        self.frame_vertices = counts.vertices;
        self.upload_geometry(encoder, &primitives, screen, counts);

        // Build every material the frame needs here, while the graph is still
        // mutably available: `scene` only reads it, so a material that does
        // not exist yet by then is one this frame cannot draw.
        //
        // The draws are walked by index because `build_material` needs `&mut
        // self`: collecting the keys first would allocate a `Vec` every frame
        // for a list that is already `self.draws`.
        for index in 0..self.draws.len() {
            let (texture, options) = {
                let draw = &self.draws[index];
                (draw.texture, draw.options)
            };
            self.build_material(graph, texture, options);
        }
    }

    /// Release every graph node the integration registered, and forget its
    /// bookkeeping.
    ///
    /// Call this when the integration leaves the frame for good, before its
    /// `EguiIntegration` value is dropped: the nodes it inserted — the two
    /// geometry buffers, every texture, view, sampler and material — are
    /// strong nodes the graph cannot collect on its own. Textures go first so
    /// their views and materials drop as dependents, then the geometry
    /// buffers, then a cleanup pass for whatever the removals orphaned.
    ///
    /// After this the integration is back to its freshly built state and may
    /// be reused against the same graph.
    pub fn release(&mut self, graph: &mut ResourceGraph) {
        for slot in self.textures.values() {
            graph.remove_drop(slot.texture);
        }
        self.textures.clear();
        self.texture_options.clear();
        self.materials.clear();
        for &(_, id) in &self.samplers {
            graph.remove_drop(id);
        }
        self.samplers.clear();
        if let Some(node) = self.vertex_node.take() {
            graph.remove_drop(node);
        }
        if let Some(node) = self.index_node.take() {
            graph.remove_drop(node);
        }
        self.vertices = None;
        self.indices = None;
        self.vertex_capacity = 0;
        self.index_capacity = 0;
        self.frame_vertices = 0;
        self.draws.clear();
        graph.cleanup_drop();
    }

    /// Move the integration onto `pipeline`, keeping every node it registered.
    ///
    /// A different render target specializes the pipeline differently, and the
    /// materials were created from the old pipeline's material layout, so they
    /// are dropped here — but only they: the textures, views, samplers and
    /// geometry buffers do not depend on the target, and egui will not resend
    /// its font atlas after a rebuild, so discarding them would both leak the
    /// old nodes and leave the new integration unable to draw text. The next
    /// [`Self::update`] rebuilds each missing material from the new layout.
    pub fn retarget(&mut self, graph: &mut ResourceGraph, pipeline: UnlitPipeline) {
        for &(_, id) in &self.materials {
            graph.remove_drop(id);
        }
        self.materials.clear();
        self.draws.clear();
        self.pipeline = pipeline;
    }

    /// Pack `primitives` into the vertex and index buffers, staging the bytes
    /// through `encoder`, and fill [`Self::draws`] with where each one landed.
    ///
    /// Positions and UV-and-colors go into two regions of the one buffer: wgpu
    /// binds one stride per slot, and the two streams differ.
    fn upload_geometry(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        primitives: &[egui::ClippedPrimitive],
        screen: ScreenDescriptor,
        counts: GeometryCounts,
    ) {
        // The scratch buffers are taken out of `self` for the duration, so the
        // draw list can be filled while they are written, then put back: a
        // steady UI reuses one allocation per stream rather than taking fresh
        // ones every frame.
        let mut vertices = core::mem::take(&mut self.packed_vertices);
        let mut indices = core::mem::take(&mut self.packed_indices);
        pack_geometry(
            primitives,
            screen,
            &self.texture_options,
            counts,
            &mut vertices,
            &mut indices,
            &mut self.draws,
        );

        let device = &self.device;
        let vertex_buffer = self.vertices.as_ref().expect("just reserved");
        let index_buffer = self.indices.as_ref().expect("just reserved");
        self.vertex_staging
            .write(device, encoder, vertex_buffer, 0, &vertices);
        self.index_staging
            .write(device, encoder, index_buffer, 0, &indices);
        self.packed_vertices = vertices;
        self.packed_indices = indices;
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
        // UVs and colors the second, and the two meet at the *frame's* vertex
        // count — the point `upload_geometry` packed at. The buffer usually has
        // room to spare, so splitting at `vertex_capacity` instead would bind
        // the second stream to bytes no vertex was written to.
        let positions_size = (self.frame_vertices * POSITION_STRIDE) as u64;
        let uv_colors_size = (self.frame_vertices * UV_COLOR_STRIDE) as u64;
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
            .with_vertex_buffer_range(
                UV_COLOR_SLOT,
                vertices,
                positions_size..positions_size + uv_colors_size,
            )
            .with_index_buffer(indices, wgpu::IndexFormat::Uint32)
            .with_scissor(draw.scissor);
            scene.push(entry);
        }
        scene
    }

    /// Reallocate the buffers when they cannot hold `counts`, registering any
    /// new buffer in `graph`.
    fn reserve(&mut self, graph: &mut ResourceGraph, counts: GeometryCounts) {
        let GeometryCounts { vertices, indices } = counts;
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
            self.vertex_node = Some(match self.vertex_node {
                // A grow replaces the buffer in its existing node, so no stale
                // node is left strongly holding the old buffer.
                Some(node) => {
                    graph
                        .replace(node, Resource::Buffer(buffer.clone()))
                        .expect("the vertex node is still registered");
                    node
                }
                None => graph
                    .insert_strong(Resource::Buffer(buffer.clone()), &[])
                    .expect("an empty dependency list always resolves"),
            });
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
            self.index_node = Some(match self.index_node {
                Some(node) => {
                    graph
                        .replace(node, Resource::Buffer(buffer.clone()))
                        .expect("the index node is still registered");
                    node
                }
                None => graph
                    .insert_strong(Resource::Buffer(buffer.clone()), &[])
                    .expect("an empty dependency list always resolves"),
            });
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
        let Some(slot) = self.textures.get(&id).copied() else {
            return;
        };
        let sampler_id = self.sampler(graph, options);
        let Some(layout) = self.pipeline.material_layout.as_ref() else {
            return;
        };
        let Some(Resource::TextureView { view, .. }) = graph.get(slot.view) else {
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
            .insert_strong(Resource::BindGroup(group), &[slot.view, sampler_id])
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
            // The texture is the root of the slot's subtree: removing it drops
            // the view, and with it every material that samples the view — the
            // graph propagates the removal along the dependency edges. Removing
            // only the view instead would leave the texture strongly held and
            // never collected.
            if let Some(slot) = self.textures.remove(&id) {
                graph.remove_drop(slot.texture);
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
        let [width, height] = delta.image.size();
        // The image's own bytes, borrowed rather than copied into a fresh
        // `Vec`: a font atlas can be megabytes, and this is the whole of it.
        let pixels: &[u8] = match &delta.image {
            egui::ImageData::Color(image) => image.as_raw(),
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
                let Some(slot) = self.textures.get(&id) else {
                    return;
                };
                let Some(Resource::Texture(texture)) = graph.get(slot.texture) else {
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
                    pixels,
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
                queue.write_texture(texture.as_image_copy(), pixels, layout, size);
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let texture_id = graph
                    .insert_strong(Resource::Texture(texture), &[])
                    .expect("an empty dependency list always resolves");
                let view_id = graph
                    .insert_strong(view, &[texture_id])
                    .expect("the texture was just registered");
                self.textures.insert(
                    id,
                    TextureSlot {
                        texture: texture_id,
                        view: view_id,
                    },
                );
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

    /// A screen descriptor matching the default test target.
    fn test_screen() -> ScreenDescriptor {
        ScreenDescriptor {
            size_in_pixels: [256, 192],
            pixels_per_point: 1.0,
        }
    }

    /// One tessellated primitive carrying `vertices` points, clipped to the
    /// whole screen.
    fn primitive(texture: egui::TextureId, vertices: usize) -> egui::ClippedPrimitive {
        let mut mesh = egui::epaint::Mesh::with_texture(texture);
        for index in 0..vertices {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: egui::pos2(index as f32, index as f32 * 2.0),
                uv: egui::pos2(0.25, 0.75),
                color: egui::Color32::from_rgba_premultiplied(1, 2, 3, 4),
            });
        }
        // A degenerate triangle per vertex, enough to exercise the index path.
        for index in 0..vertices.saturating_sub(2) {
            mesh.indices
                .extend([index as u32, index as u32 + 1, index as u32 + 2]);
        }
        egui::ClippedPrimitive {
            clip_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(256.0, 192.0)),
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }
    }

    /// The packing lands each primitive's bytes where its draw says, and the
    /// two vertex streams hold the values egui put in.
    #[test]
    fn packing_places_each_primitive_where_its_draw_says() {
        let primitives = [primitive(egui::TextureId::default(), 4)];
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut draws = Vec::new();
        let counts = measure(&primitives);
        pack_geometry(
            &primitives,
            test_screen(),
            &HashMap::new(),
            counts,
            &mut vertices,
            &mut indices,
            &mut draws,
        );

        assert_eq!(draws.len(), 1, "one mesh is one draw");
        let draw = &draws[0];
        assert_eq!(draw.first_vertex, 0);
        assert_eq!(draw.indices, 0..6, "four vertices make two triangles");

        // The vertex buffer holds the position stream then the interleaved
        // UV-and-color stream, each entry one stride wide.
        let uv_color_start = 4 * POSITION_STRIDE;
        assert_eq!(vertices.len(), uv_color_start + 4 * UV_COLOR_STRIDE);
        let positions: Vec<f32> = vertices[..uv_color_start]
            .as_chunks::<{ size_of::<f32>() }>()
            .0
            .iter()
            .map(|word| f32::from_le_bytes(*word))
            .collect();
        assert_eq!(
            positions,
            vec![0.0, 0.0, 0.0, 1.0, 2.0, 0.0, 2.0, 4.0, 0.0, 3.0, 6.0, 0.0],
            "each position is x, y and a zero z"
        );

        // The colors are egui's premultiplied bytes, unchanged.
        let first_color = uv_color_start + size_of::<egui::Vec2>();
        assert_eq!(
            &vertices[first_color..first_color + size_of::<egui::Color32>()],
            &[1, 2, 3, 4]
        );

        // The indices are this primitive's, starting at zero for the first.
        let indices: Vec<u32> = indices
            .as_chunks::<{ size_of::<u32>() }>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        assert_eq!(indices, vec![0, 1, 2, 1, 2, 3]);
    }

    /// A second primitive's indices are offset by the first's vertex count, so
    /// one base vertex addresses both streams.
    #[test]
    fn a_later_primitive_is_offset_by_the_earlier_one() {
        let primitives = [
            primitive(egui::TextureId::default(), 3),
            primitive(egui::TextureId::default(), 3),
        ];
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut draws = Vec::new();
        let counts = measure(&primitives);
        pack_geometry(
            &primitives,
            test_screen(),
            &HashMap::new(),
            counts,
            &mut vertices,
            &mut indices,
            &mut draws,
        );

        assert_eq!(draws.len(), 2);
        assert_eq!(draws[0].indices, 0..3);
        assert_eq!(
            draws[1].first_vertex, 3,
            "the second mesh starts after the first"
        );
        assert_eq!(draws[1].indices, 3..6);
    }

    /// The byte buffers are cleared and refilled, not reallocated, so a caller
    /// that keeps them uploads without allocating.
    #[test]
    fn packing_reuses_the_vertex_and_index_buffers() {
        let big = [primitive(egui::TextureId::default(), 8)];
        let small = [primitive(egui::TextureId::default(), 3)];

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut draws = Vec::new();
        let big_counts = measure(&big);
        pack_geometry(
            &big,
            test_screen(),
            &HashMap::new(),
            big_counts,
            &mut vertices,
            &mut indices,
            &mut draws,
        );
        let vertex_capacity = vertices.capacity();
        let index_capacity = indices.capacity();

        // A later, smaller frame keeps both allocations rather than taking new
        // ones; a fresh `Vec` per frame would have exactly its own length.
        let small_counts = measure(&small);
        pack_geometry(
            &small,
            test_screen(),
            &HashMap::new(),
            small_counts,
            &mut vertices,
            &mut indices,
            &mut draws,
        );
        assert_eq!(
            vertices.capacity(),
            vertex_capacity,
            "the vertex scratch buffer is reused"
        );
        assert_eq!(
            indices.capacity(),
            index_capacity,
            "the index scratch buffer is reused"
        );
        assert_eq!(draws.len(), 1, "and the stale second draw is gone");
    }
}
