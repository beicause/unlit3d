//! An [egui](https://egui.rs) backend built on the built-in lit pipeline.
//!
//! [`EguiIntegration`] takes what egui produces each frame — tessellated, clipped
//! meshes plus a set of texture updates — and draws it with the built-in
//! pipeline as ordinary screen-space geometry. Nothing in the shader knows
//! about egui: the UI is premultiplied, textured, screen-space vertices, and
//! egui's clip rectangles become scissor rectangles.
//!
//! ```rust
//! # use unlit_wgpu::globals::Globals;
//! # use unlit_wgpu::pipeline::{CAMERA_BINDING, FRAME_BINDING, VIEW_BINDING};
//! # use unlit_wgpu::resources::ResourceGraph;
//! # use unlit_wgpu::specialize::{SpecializedPipeline, SurfaceKey};
//! # use unlit_wgpu::ui::{
//! #     EguiIntegration, ScreenDescriptor, screen_view, screen_viewport, ui_variant,
//! # };
//! # use zerocopy::IntoBytes;
//! # fn frame(device: &wgpu::Device, queue: &wgpu::Queue, ctx: &egui::Context,
//! #          surface: SurfaceKey) {
//! // The caller owns the globals: a camera uniform (written every frame with
//! // `screen_view`), a frame-globals uniform, the view `screen_viewport`
//! // derives from the same points, and the bind group binding all three.
//! let variant = ui_variant(/* the target encodes sRGB: */ true, surface);
//! let pipeline = SpecializedPipeline::create(device, variant);
//! let camera = uniform_buffer(device, "ui::camera");
//! let globals = uniform_buffer(device, "ui::globals");
//! let view = uniform_buffer(device, "ui::view");
//! let global_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
//!     label: Some("ui::globals"),
//!     layout: &pipeline.descriptor().bind_group_layouts(device).global,
//!     entries: &[
//!         wgpu::BindGroupEntry {
//!             binding: CAMERA_BINDING,
//!             resource: camera.as_entire_binding(),
//!         },
//!         wgpu::BindGroupEntry {
//!             binding: FRAME_BINDING,
//!             resource: globals.as_entire_binding(),
//!         },
//!         wgpu::BindGroupEntry {
//!             binding: VIEW_BINDING,
//!             resource: view.as_entire_binding(),
//!         },
//!     ],
//! });
//!
//! // The camera and the view are written every frame; the UI reads no clock,
//! // so its frame globals stay at their default.
//! queue.write_buffer(&camera, 0, screen_view([256.0, 192.0]).as_bytes());
//! queue.write_buffer(&view, 0, screen_viewport([256.0, 192.0]).as_bytes());
//! queue.write_buffer(&globals, 0, Globals::default().as_bytes());
//!
//! // The UI's per-texture resources join the caller's ledger.
//! let mut graph = ResourceGraph::new();
//! let camera_id = graph.insert(camera, None);
//! let globals_id = graph.insert(globals, None);
//! let view_id = graph.insert(view, None);
//! let group_id = graph.insert(global_group, None);
//! graph.add_dependency(&group_id, &camera_id);
//! graph.add_dependency(&group_id, &globals_id);
//! graph.add_dependency(&group_id, &view_id);
//!
//! let mut ui = EguiIntegration::new(device, pipeline);
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
//! let scene = ui.scene(&graph, &graph.get(&group_id).expect("the group exists"));
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

use crate::globals::{Camera, View};
use crate::mesh::{ChannelEncoding, MeshInstance, PositionStreamChannels, SecondaryFlags};
use crate::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, GLOBAL_GROUP, INSTANCE_SLOT,
    LitOptions, LitVariant, LitVertexChannels, MATERIAL_GROUP, PRIMARY_SLOT, SECONDARY_SLOT,
    SpecializedLitPipeline,
};
use crate::resources::{Rebuild, ResHandle, Resource, ResourceGraph, TextureExt, TextureView};
use crate::scene::{DrawEntry, DrawOrder, DrawRange, Scene, ScissorRect};
use crate::specialize::SurfaceKey;
use crate::staging::StagingBuffer;
use crate::util::Hashed;
use core::ops::Range;
use hashbrown::HashMap;
use zerocopy::IntoBytes;

mod frame;
mod materials;
#[cfg(test)]
mod tests;

/// egui's own texture format: gamma-space RGBA, never sRGB-aware.
///
/// egui's vertex colors and its font atlas are already gamma-encoded, so a
/// texture is sampled without conversion and the pipeline's own output
/// conversion deals with the target instead.
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Bytes one planar-position vertex occupies.
///
/// The UI writes positions at full precision and in the plane, so this is
/// [`ChannelEncoding::PlanarPosition`]'s format: two components, with the
/// zero third supplied by the shader rather than stored.
const POSITION_STRIDE: usize = wgpu::VertexFormat::Float32x2.size() as usize;

/// Bytes one secondary vertex occupies: `Float32x2` then `Unorm8x4`.
const SECONDARY_STRIDE: usize =
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
/// is theirs; [`screen_viewport`] is the view to bind beside it.
pub fn screen_view(viewport_points: [f32; 2]) -> Camera {
    let [width, height] = viewport_points;
    Camera::from_clip_from_world(
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

/// The view to bind beside [`screen_view`].
///
/// The clipping is in points, so the size a shader measures against is the point
/// size too: that is what keeps the two in one unit.
pub fn screen_viewport(viewport_points: [f32; 2]) -> View {
    View::new(viewport_points[0], viewport_points[1])
}

/// The pipeline variant a UI is drawn with.
///
/// `srgb_to_linear_output` is the caller's call, not this function's: it
/// depends on the target's format, which this function never sees. Set it
/// when the target encodes sRGB.
///
/// Every UI draw has the same shape: full-precision screen-space vertices
/// carrying a premultiplied color and a texture coordinate, no skinning and no
/// morph target, and a base-color texture sampled through a material group.
/// What changes with the target is the surface the variant carries, so this is
/// the only place a UI variant is built.
pub fn ui_variant(srgb_to_linear_output: bool, surface: SurfaceKey) -> LitVariant {
    let mut options = LitOptions::standard();
    apply_ui_settings(&mut options, srgb_to_linear_output);
    LitVariant {
        options: Hashed::new(options),
        surface,
        channels: ui_channels(),
        base_color_texture: true,
        morph: false,
        strip_index_format: None,
    }
}

/// The per-vertex channels a UI mesh is packed with.
///
/// The UI writes positions at full precision and in the plane — two components,
/// with the shader supplying the zero third — and needs no skinning, and every
/// vertex carries a texture coordinate and a premultiplied color. A caller
/// packing a UI mesh builds its streams to match this.
pub fn ui_channels() -> LitVertexChannels {
    LitVertexChannels {
        position: PositionStreamChannels {
            position: Some(ChannelEncoding::PlanarPosition),
            joints: false,
        },
        secondary: SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
    }
}

/// Apply the UI's device-independent policy — the output conversion, culling,
/// blending and the overlaid depth behavior — to a standard options set.
///
/// The geometry the UI draws with is not policy and lives in
/// [`ui_channels`] and the variant: an encoding, a skin or a morph target is
/// a property of the mesh, not of what the caller picked.
fn apply_ui_settings(options: &mut LitOptions, srgb_to_linear_output: bool) {
    options.srgb_to_linear_output = srgb_to_linear_output;
    // A UI fragment composites what egui tessellated and shades nothing: the
    // overlay is drawn after the scene, so a light would tint text.
    options.unlit = true;
    // No culling: egui does not guarantee a consistent winding order across
    // the primitives it emits, so neither face can be discarded safely.
    // (`LitOptions::standard` culls back faces for closed meshes; the UI is
    // flat, overlay geometry with no inside to hide.)
    options.primitive.cull_mode = None;
    // egui tessellates premultiplied colors, so the source's RGB is added to
    // the destination scaled by the alpha it leaves behind.
    options.blend = Some(wgpu::BlendState {
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
    // depth. Whether the target has a depth attachment at all is decided by
    // the surface the variant carries; the policy only says what a target
    // with one would run.
    options.depth_stencil.depth_write_enabled = false;
    options.depth_stencil.depth_compare = wgpu::CompareFunction::Always;
}

/// How much geometry a frame holds: the two numbers the packing and the draw
/// ranges both need, and which must agree between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GeometryCounts {
    vertices: usize,
    indices: usize,
}

/// Count the vertices and indices every tessellated primitive in
/// `primitives` contributes.
fn measure<'a>(
    primitives: impl IntoIterator<Item = (Source, &'a egui::ClippedPrimitive)>,
) -> GeometryCounts {
    let mut counts = GeometryCounts {
        vertices: 0,
        indices: 0,
    };
    for (_, primitive) in primitives {
        if let Some(mesh) = mesh_of(primitive) {
            counts.vertices += mesh.vertices.len();
            counts.indices += mesh.indices.len();
        }
    }
    counts
}

/// Pack `primitives` into `vertices` and `indices`, and fill `draws` with where
/// each one landed.
///
/// Both byte buffers are cleared and resized rather than reallocated, so a
/// caller that keeps them across frames reuses the allocation. The vertex
/// buffer holds the primary stream first and the interleaved secondary
/// stream second: wgpu binds one stride per slot and the two differ.
///
/// `counts` must be what [`measure`] returned for `primitives`: it is both how
/// much is written and where the two vertex streams meet.
fn pack_geometry<'a>(
    primitives: impl IntoIterator<Item = (Source, &'a egui::ClippedPrimitive)>,
    screen: ScreenDescriptor,
    texture_options: &HashMap<TextureKey, egui::TextureOptions>,
    counts: GeometryCounts,
    vertices: &mut Vec<u8>,
    indices: &mut Vec<u8>,
    draws: &mut Vec<UiDraw>,
) {
    // The split between the two streams is the frame's own vertex count, which
    // is what `scene` reads back. It is *not* the buffer's capacity: that is
    // usually larger, and splitting on it would write the UVs and colors
    // somewhere the draws do not look for them.
    let secondary_start = counts.vertices * POSITION_STRIDE;
    let vertex_count = counts.vertices;
    let index_count = counts.indices;

    vertices.clear();
    vertices.resize(secondary_start + vertex_count * SECONDARY_STRIDE, 0);
    indices.clear();
    indices.resize(index_count * size_of::<u32>(), 0);
    draws.clear();

    let mut vertex_cursor = 0usize;
    let mut index_cursor = 0usize;
    for (source, primitive) in primitives {
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
            // The UI is flat: two components, the shader supplies the zero
            // third.
            vertices[position..position + POSITION_STRIDE]
                .copy_from_slice([vertex.pos.x, vertex.pos.y].as_bytes());
            let secondary = secondary_start + vertex_cursor * SECONDARY_STRIDE;
            vertices[secondary..secondary + size_of::<egui::Vec2>()]
                .copy_from_slice([vertex.uv.x, vertex.uv.y].as_bytes());
            // egui's colors are premultiplied already, straight to Unorm8.
            vertices[secondary + size_of::<egui::Vec2>()..secondary + SECONDARY_STRIDE]
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
            // The primitive's indices are local to its own vertices, and the
            // draw addresses both streams by a pool-global ordinal, so the
            // primitive's first vertex is added here. Baking it into the
            // indices rather than passing it as the draw's `base_vertex` costs
            // nothing — the indices are rewritten every frame either way — and
            // it is the only form a device without `base_vertex` (WebGL2)
            // accepts.
            slot.copy_from_slice((index + first_vertex).as_bytes());
        }
        index_cursor += mesh.indices.len();

        let texture = TextureKey {
            source,
            id: mesh.texture_id,
        };
        draws.push(UiDraw {
            indices: first_index..index_cursor as u32,
            texture,
            // The texture's own sampling options, not a default: a texture egui
            // asked to sample nearest or to tile would otherwise be sampled
            // linearly and clamped, and the sampler that matches `options` is
            // the one the material binds.
            options: texture_options.get(&texture).copied().unwrap_or_default(),
            scissor: scissor_rect(primitive.clip_rect, screen),
        });
    }
}

/// One uploaded primitive: where its indices sit, what it samples and how it
/// is clipped.
struct UiDraw {
    /// Index range into the index buffer, counting indices.
    ///
    /// The indices themselves already name the primitive's vertices in the
    /// pool, so a draw needs no `base_vertex` to address them; see
    /// [`pack_geometry`].
    indices: Range<u32>,
    /// The texture this primitive samples, and the context it came from.
    texture: TextureKey,
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
#[derive(Clone)]
struct TextureSlot {
    /// The texture egui's deltas are written into.
    texture: ResHandle<wgpu::Texture>,
    /// The default view over it, which the material samples.
    view: ResHandle<TextureView>,
}

/// Draws tessellated egui output with the built-in lit pipeline.
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
    pipeline: SpecializedLitPipeline,
    /// Graph nodes of every allocated texture slot, keyed by origin and id.
    textures: HashMap<TextureKey, TextureSlot>,
    /// The sampling options egui last stated for each texture.
    ///
    /// Kept separately from the texture itself because every [`ImageDelta`]
    /// carries the options that apply to the whole texture, and the draw that
    /// samples it has to name the matching sampler:
    /// [`Self::sampler`] keys its cache on exactly these options, so a texture
    /// uploaded as nearest samples nearest.
    ///
    /// [`ImageDelta`]: egui::epaint::ImageDelta
    texture_options: HashMap<TextureKey, egui::TextureOptions>,
    /// One sampler per distinct set of egui sampling options seen, with its
    /// graph node.
    samplers: Vec<(egui::TextureOptions, ResHandle<wgpu::Sampler>)>,
    /// One material bind group per (texture, options) pair, with its graph
    /// node; the node depends on the texture's view and the sampler.
    materials: Vec<(MaterialKey, ResHandle<wgpu::BindGroup>)>,
    /// Positions, then interleaved UVs and colors.
    vertices: Option<wgpu::Buffer>,
    /// `Uint32` indices.
    indices: Option<wgpu::Buffer>,
    /// Graph node of the vertex buffer, replaced in place when it grows.
    vertex_node: Option<ResHandle<wgpu::Buffer>>,
    /// Graph node of the index buffer, replaced in place when it grows.
    index_node: Option<ResHandle<wgpu::Buffer>>,
    /// One default [`MeshInstance`], bound at [`INSTANCE_SLOT`] by every
    /// draw.
    ///
    /// The built-in pipeline always declares the instance stream, and a UI
    /// draw reads no instance state, so one identity record serves every draw.
    /// It never changes, so it needs no graph node.
    instance: wgpu::Buffer,
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

/// Which context a texture belongs to.
///
/// Every egui context allocates its texture ids from zero, so one id names
/// different images in different contexts. A UI drawn from several contexts —
/// the world's own panels plus a frame laid out in another world — therefore
/// keys its textures by this pair rather than by the id alone, which is what
/// keeps two contexts' atlases apart without rewriting the ids a frame
/// carries.
///
/// The origin of the world's own panels is [`Source::LOCAL`]; every other
/// publisher picks its own, and the numbers are that publisher's to allocate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Source(pub u32);

impl Source {
    /// The context of the world doing the drawing.
    pub const LOCAL: Self = Self(0);

    /// The context of the world publishing into `slot`.
    ///
    /// The number is the slot's plus one, so no publisher collides with the
    /// drawing world's own panels and each slot has a namespace of its own.
    #[must_use]
    pub const fn published(slot: usize) -> Self {
        Self(slot as u32 + 1)
    }
}

/// One texture of one source.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TextureKey {
    source: Source,
    id: egui::TextureId,
}

/// One source's contribution to a frame: what to upload and what it owns.
///
/// The textures are consumed — a delta is applied once, and egui panics when
/// one is dropped unapplied — while the primitives are borrowed: a frame laid
/// out in another world is drawn again on every drawn frame until that world
/// publishes a newer one, so its tessellation has to outlive the upload.
pub struct UiFrame<'a> {
    /// Which context this contribution came from.
    pub source: Source,
    /// The context's texture updates for this frame.
    pub textures: egui::TexturesDelta,
    /// The context's tessellated geometry.
    pub primitives: &'a [egui::ClippedPrimitive],
}

/// What one material bind group was built from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct MaterialKey {
    texture: TextureKey,
    options: egui::TextureOptions,
}

/// A one-element buffer holding the identity [`MeshInstance`].
///
/// The built-in lit pipeline declares the instance stream for every variant,
/// so a draw that binds nothing at [`INSTANCE_SLOT`] is rejected. A UI draw
/// reads no instance state — its vertices are already in screen space — so one
/// default record satisfies the slot for every draw the integration records.
fn default_instance(device: &wgpu::Device) -> wgpu::Buffer {
    let instance = MeshInstance::default();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ui::instance"),
        size: size_of::<MeshInstance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .expect("mapped at creation")
        .copy_from_slice(instance.as_bytes());
    buffer.unmap();
    buffer
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
