//! An [egui](https://egui.rs) backend built on the built-in unlit pipeline.
//!
//! [`UiRenderer`] takes what egui produces each frame — tessellated, clipped
//! meshes plus a set of texture updates — and draws it with the built-in
//! pipeline as ordinary screen-space geometry. Nothing in the shader knows
//! about egui: the UI is premultiplied, textured, screen-space vertices, and
//! egui's clip rectangles become scissor rectangles.
//!
//! ```no_run
//! # use wgpu_unlit_render::ui::UiRenderer;
//! # fn frame(device: &wgpu::Device, queue: &wgpu::Queue, ctx: &egui::Context,
//! #          color_format: wgpu::TextureFormat) {
//! let mut ui = UiRenderer::new(device, color_format, 1);
//! let mut input = egui::RawInput::default();
//! input.screen_rect = Some(egui::Rect::from_min_size(
//!     egui::Pos2::ZERO,
//!     egui::Vec2::new(256.0, 192.0),
//! ));
//! let output = ctx.run_ui(input, |ui| {
//!     ui.label("hello world");
//! });
//! ui.update(device, queue, ctx, output, 1.0, [256.0, 192.0]);
//! let scene = ui.scene();
//! # }
//! ```

use crate::globals::{Globals, View};
use crate::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, MATERIAL_GROUP, POSITION_SLOT, UV_COLOR_SLOT, UnlitFlags, UnlitOptions,
    UnlitPipeline,
};
use crate::scene::{DrawRange, MaterialGroup, MeshDraw, PipelineGroup, Scene, ScissorRect};
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
fn screen_view(viewport_points: [f32; 2]) -> View {
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
fn ui_options(srgb_target: bool) -> UnlitOptions {
    let mut options = UnlitOptions::standard();
    // Full-precision screen-space vertices carrying a premultiplied color and
    // a texture coordinate: no compression, no per-instance stream.
    let mut flags = UnlitFlags::VERTEX_POSITION
        | UnlitFlags::UNCOMPRESSED_POSITION
        | UnlitFlags::VERTEX_UV
        | UnlitFlags::UNCOMPRESSED_UV
        | UnlitFlags::VERTEX_COLOR
        | UnlitFlags::BASE_COLOR_TEXTURE;
    if srgb_target {
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
    // depth.
    options.depth.depth_write_enabled = Some(false);
    options.depth.depth_compare = Some(wgpu::CompareFunction::Always);
    options
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
pub struct UiRenderer {
    /// Kept to build the resources a frame turns out to need.
    device: wgpu::Device,
    pipeline: UnlitPipeline,
    camera: wgpu::Buffer,
    globals: wgpu::Buffer,
    global_group: wgpu::BindGroup,
    /// GPU texture of every egui texture slot currently allocated.
    textures: HashMap<egui::TextureId, wgpu::Texture>,
    /// One sampler per distinct set of egui sampling options seen.
    samplers: Vec<(egui::TextureOptions, wgpu::Sampler)>,
    /// One material bind group per (texture, options) pair.
    materials: Vec<Material>,
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

/// One material bind group and what it binds.
struct Material {
    texture: egui::TextureId,
    options: egui::TextureOptions,
    group: wgpu::BindGroup,
}

impl UiRenderer {
    /// Build the UI pipeline for a target of `color_format` and `sample_count`.
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let options = ui_options(color_format.is_srgb());
        let pipeline = UnlitPipeline::new(
            device,
            &options,
            color_format,
            // The UI overlays whatever the pass holds, so it neither tests nor
            // writes depth — but every pipeline in a pass with a depth
            // attachment must declare that attachment's format, which `new`
            // takes from here rather than guessing.
            Some(wgpu::TextureFormat::Depth32Float),
            sample_count,
        );

        let camera = uniform_buffer(device, "ui::camera", view_size());
        let globals = uniform_buffer(device, "ui::globals", globals_size());
        let global_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui::globals"),
            layout: &pipeline.global_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: CAMERA_BINDING,
                    resource: camera.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: FRAME_BINDING,
                    resource: globals.as_entire_binding(),
                },
            ],
        });

        Self {
            device: device.clone(),
            pipeline,
            camera,
            globals,
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

    /// Advance the frame globals the UI's shader reads.
    pub fn set_globals(&self, queue: &wgpu::Queue, globals: &Globals) {
        use zerocopy::IntoBytes;
        queue.write_buffer(&self.globals, 0, globals.as_bytes());
    }

    /// Apply `output`'s texture updates and upload its tessellated shapes,
    /// ready for [`Self::scene`].
    ///
    /// `viewport_points` is the target's size in logical points — the space
    /// egui tessellates into — which the projection maps onto clip space.
    /// `pixels_per_point` tells egui how to rasterise text but never enters
    /// the projection.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
        pixels_per_point: f32,
        viewport_points: [f32; 2],
    ) {
        use zerocopy::IntoBytes;

        self.apply_textures(device, queue, &output.textures_delta);
        // egui panics if a delta is dropped unapplied, so mark it handled.
        output.textures_delta.clear();
        queue.write_buffer(&self.camera, 0, screen_view(viewport_points).as_bytes());

        let primitives = ctx.tessellate(output.shapes, pixels_per_point);
        self.draws.clear();
        let (vertices, indices) = measure(&primitives);
        if vertices == 0 {
            return;
        }
        self.reserve(device, vertices, indices);
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
    pub fn scene(&mut self) -> Scene<'_> {
        let mut scene = Scene::new();
        if self.vertices.is_none() || self.indices.is_none() || self.draws.is_empty() {
            return scene;
        }

        // Build every material the frame needs first: the scene borrows them
        // all, which it cannot do while any is still being inserted.
        let wanted: Vec<_> = self
            .draws
            .iter()
            .map(|draw| (draw.texture, draw.options))
            .collect();
        for (id, options) in wanted {
            self.build_material(&id, &options);
        }

        let (vertices, indices) = (
            self.vertices.as_ref().expect("checked"),
            self.indices.as_ref().expect("checked"),
        );
        // The position stream is the first half of the buffer, the interleaved
        // UVs and colors the second; each holds `vertex_capacity` vertices.
        let positions_size = (self.vertex_capacity * POSITION_STRIDE) as u64;
        let mut group = PipelineGroup::new(&self.pipeline.pipeline)
            .with_bind_group(GLOBAL_GROUP, &self.global_group);
        for draw in &self.draws {
            // Its own material group per draw: each carries its own scissor
            // rectangle, so consecutive draws sharing one are what the
            // recorder's deduplication is for.
            let Some(material) = self.find_material(&draw.texture, &draw.options) else {
                // A primitive whose texture was freed this frame has nothing
                // to sample, so it is skipped rather than bound to a stale view.
                continue;
            };
            // Both slots cover their whole stream: every vertex of the frame
            // sits at the same ordinal in each, so one base vertex addresses
            // the position, the UV and the color together. Offsetting the
            // slices instead would double the base vertex.
            let mesh = MeshDraw::new(
                DrawRange::indexed(draw.indices.clone()).with_base_vertex(draw.first_vertex as i32),
            )
            .with_bind_group(MATERIAL_GROUP, &material.group)
            .with_vertex_buffer(POSITION_SLOT, vertices.slice(..positions_size))
            .with_vertex_buffer(UV_COLOR_SLOT, vertices.slice(positions_size..))
            .with_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32)
            .with_scissor(draw.scissor);
            group = group.with_material(MaterialGroup::new().with_mesh(mesh));
        }
        scene.push(group);
        scene
    }

    /// Reallocate the buffers when they cannot hold `vertices` and `indices`.
    fn reserve(&mut self, device: &wgpu::Device, vertices: usize, indices: usize) {
        // Grow geometrically so a UI that grows over a few frames does not
        // reallocate every frame.
        if self.vertices.is_none() || self.vertex_capacity < vertices {
            self.vertex_capacity = vertices.max(self.vertex_capacity * 2).max(1);
            self.vertices = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui::vertices"),
                size: (self.vertex_capacity * (POSITION_STRIDE + UV_COLOR_STRIDE)) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if self.indices.is_none() || self.index_capacity < indices {
            self.index_capacity = indices.max(self.index_capacity * 2).max(1);
            self.indices = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui::indices"),
                size: (self.index_capacity * size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }

    /// Build the material group for (`id`, `options`) if it does not exist.
    fn build_material(&mut self, id: &egui::TextureId, options: &egui::TextureOptions) {
        if self.find_material(id, options).is_some() {
            return;
        }
        // The sampler is cloned out first, so no borrow of `self` is held
        // while the texture and the layout are borrowed.
        let sampler = self.sampler(options).clone();
        let Some(texture) = self.textures.get(id) else {
            return;
        };
        let Some(layout) = self.pipeline.material_layout.as_ref() else {
            return;
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui::material"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.materials.push(Material {
            texture: *id,
            options: *options,
            group,
        });
    }

    fn find_material(
        &self,
        id: &egui::TextureId,
        options: &egui::TextureOptions,
    ) -> Option<&Material> {
        self.materials
            .iter()
            .find(|material| material.texture == *id && material.options == *options)
    }

    /// The sampler for `options`, created on first use.
    fn sampler(&mut self, options: &egui::TextureOptions) -> &wgpu::Sampler {
        let index = self.samplers.iter().position(|(seen, _)| seen == options);
        let index = match index {
            Some(index) => index,
            None => {
                let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("ui::sampler"),
                    address_mode_u: address_mode(options.wrap_mode),
                    address_mode_v: address_mode(options.wrap_mode),
                    mag_filter: texture_filter(options.magnification),
                    min_filter: texture_filter(options.minification),
                    ..Default::default()
                });
                self.samplers.push((*options, sampler));
                self.samplers.len() - 1
            }
        };
        &self.samplers[index].1
    }

    fn apply_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        delta: &egui::TexturesDelta,
    ) {
        for (&id, deltas) in &delta.set {
            for image in deltas {
                self.upload_texture(device, queue, id, image);
            }
        }
        for &id in &delta.free {
            self.textures.remove(&id);
            self.materials.retain(|material| material.texture != id);
        }
    }

    fn upload_texture(
        &mut self,
        device: &wgpu::Device,
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
                let Some(texture) = self.textures.get(&id) else {
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
                let texture = device.create_texture(&wgpu::TextureDescriptor {
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
                self.textures.insert(id, texture);
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

/// Shader size of the camera uniform, straight from its layout.
fn view_size() -> u64 {
    <View as const_shader_layout::ShaderLayout>::SIZE.get()
}

/// Shader size of the frame-globals uniform.
fn globals_size() -> u64 {
    <Globals as const_shader_layout::ShaderLayout>::SIZE.get()
}

/// A uniform buffer of `size` bytes, written through the queue.
fn uniform_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
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

    #[test]
    fn ui_variant_is_screen_space_and_uncompressed() {
        let options = ui_options(false);
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
        assert_eq!(options.depth.depth_write_enabled, Some(false));
        assert_eq!(
            options.depth.depth_compare,
            Some(wgpu::CompareFunction::Always)
        );
        // egui's colors premultiply their own alpha.
        let blend = options.blend.expect("the UI variant blends");
        assert_eq!(blend.color.src_factor, wgpu::BlendFactor::One);
        assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    }

    #[test]
    fn srgb_target_adds_the_output_conversion() {
        assert!(
            !ui_options(false)
                .flags
                .contains(UnlitFlags::SRGB_TO_LINEAR_OUTPUT)
        );
        assert!(
            ui_options(true)
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
