//! The built-in unlit pipeline and the WESL composition behind it.
//!
//! [`UnlitPipeline`] composes the built-in `unlit.wesl` with a variant
//! selected by [`UnlitOptions`], builds the layouts the shader expects, and
//! exposes the vertex-buffer layouts the caller declares when creating meshes.
//! Each variant contains exactly the bindings and attributes the selected
//! channels need, so nothing unused reaches the GPU.

use crate::mesh::{CompressedColor, CompressedUv, MeshInfo};
use wgpu::WriteOnly;

/// Binding slot of the camera uniform in the global bind group.
pub const CAMERA_BINDING: u32 = 0;
/// Binding slot of the frame-globals uniform in the global bind group.
pub const FRAME_BINDING: u32 = 1;
/// Binding slot of the mesh-metadata storage buffer in the global bind group.
pub const MESH_METADATA_BINDING: u32 = 2;
/// Bind-group index of the global group.
pub const GLOBAL_GROUP: u32 = 0;
/// Bind-group index of the material group.
pub const MATERIAL_GROUP: u32 = 1;
/// Binding slot of the base-color texture in the material group.
pub const BASE_COLOR_TEXTURE_BINDING: u32 = 0;
/// Binding slot of the base-color sampler in the material group.
pub const BASE_COLOR_SAMPLER_BINDING: u32 = 1;
/// Bind-group index of the mesh group.
pub const MESH_GROUP: u32 = 2;
/// Binding slot of the mesh-info uniform in the mesh group.
pub const MESH_INFO_BINDING: u32 = 0;

/// Vertex-buffer slot carrying compressed positions.
pub const POSITION_SLOT: u32 = 0;
/// Vertex-buffer slot carrying UVs and vertex colors.
pub const UV_COLOR_SLOT: u32 = 1;
/// Vertex-buffer slot carrying per-instance data.
pub const INSTANCE_SLOT: u32 = 2;

/// Vertex attribute locations declared by the built-in shader.
mod location {
    /// Compressed position (`Snorm16x4`).
    pub const POSITION: u32 = 0;
    /// Compressed UV (`Snorm16x2`).
    pub const UV: u32 = 1;
    /// Vertex color (`Unorm8x4`).
    pub const COLOR: u32 = 2;
    /// First column of the per-instance model matrix.
    pub const MODEL_0: u32 = 3;
    /// Second column of the per-instance model matrix.
    pub const MODEL_1: u32 = 4;
    /// Third column of the per-instance model matrix.
    pub const MODEL_2: u32 = 5;
    /// Per-instance base color.
    pub const BASE_COLOR: u32 = 6;
}

/// Entry point name of the built-in shader's vertex stage.
pub const VS_MAIN: &str = "vs_main";
/// Entry point name of the built-in shader's fragment stage.
pub const FS_MAIN: &str = "fs_main";

bitflags::bitflags! {
    /// The channels and bindings the built-in shader variant reads.
    ///
    /// Each flag adds both a shader code path and the matching vertex
    /// attribute or binding, so a variant contains exactly what it uses. The
    /// flags are independent except where noted.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct UnlitFlags: u8 {
        /// Read a per-vertex position.
        ///
        /// Without it the geometry is a single point at the instance origin,
        /// so the draw needs no position vertex buffer — what point, particle
        /// and impostor draws want, where the per-instance data alone places
        /// the vertex.
        const VERTEX_POSITION = 1 << 0;
        /// Store [`Self::VERTEX_POSITION`] as full-precision `Float32x3`
        /// instead of the compressed `Snorm16x4`.
        const UNCOMPRESSED_POSITION = 1 << 1;
        /// Read a per-vertex UV. Required by [`Self::BASE_COLOR_TEXTURE`],
        /// which samples with it.
        const VERTEX_UV = 1 << 2;
        /// Store [`Self::VERTEX_UV`] as full-precision `Float32x2` instead of
        /// the compressed `Snorm16x2`.
        const UNCOMPRESSED_UV = 1 << 3;
        /// Read a per-vertex color and multiply it into the base color.
        const VERTEX_COLOR = 1 << 4;
        /// Read the [`INSTANCE_SLOT`] vertex stream: the per-instance affine
        /// model matrix and base color.
        ///
        /// Without it nothing transforms the vertices — they are already in
        /// world space — and the base color is white, which is what
        /// screen-space draws (a user-interface pass, for example) want.
        const VERTEX_INSTANCE = 1 << 5;
        /// Sample a base-color texture from the material group.
        const BASE_COLOR_TEXTURE = 1 << 6;
        /// The color target is sRGB-aware: it encodes the values written to
        /// it, so the fragment converts them from sRGB to linear first.
        ///
        /// The shader multiplies the caller's colors in whichever space they
        /// arrive in — premultiplied sRGB for a user-interface pass, say — and
        /// a target that encodes sRGB would otherwise encode them a second
        /// time. Alpha is coverage rather than color, so it never converts.
        const SRGB_TO_LINEAR_OUTPUT = 1 << 7;
    }
}

/// The variant of the built-in unlit shader to compose.
///
/// [`UnlitOptions::standard`] is the usual starting point: compressed
/// positions, per-instance transforms and blending off.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnlitOptions {
    /// The channels and bindings the variant reads.
    pub flags: UnlitFlags,
    /// How the pipeline assembles and culls primitives.
    pub primitive: wgpu::PrimitiveState,
    /// The pipeline's depth-stencil state, used when the pipeline is created
    /// for a pass with a depth attachment.
    ///
    /// Its format is overwritten with the format the pass attaches, so only
    /// the comparison, the write mask and the stencil and bias settings need
    /// to be chosen here. [`Self::standard`] is the renderer's reverse-z
    /// convention: the pass clears depth to [`crate::renderer::DEPTH_CLEAR`],
    /// so nearer geometry carries the greater value.
    pub depth: wgpu::DepthStencilState,
    /// How the pipeline blends its output into the color target.
    ///
    /// `None` writes the fragment output unblended.
    pub blend: Option<wgpu::BlendState>,
}

impl UnlitOptions {
    /// The usual starting point: the full compressed mesh variant — positions,
    /// UVs, vertex colors, a base-color texture and per-instance transforms —
    /// with the renderer's reverse-z depth and no blending.
    ///
    /// Back faces are culled: the geometry this variant is for is closed
    /// meshes wound counter-clockwise, whose insides are never meant to show.
    ///
    /// Every variant must read at least one vertex attribute: one reading
    /// nothing composes a `VertexInput` struct with no members, which is not
    /// valid WGSL.
    pub fn standard() -> Self {
        Self {
            flags: UnlitFlags::VERTEX_POSITION
                | UnlitFlags::VERTEX_UV
                | UnlitFlags::VERTEX_COLOR
                | UnlitFlags::VERTEX_INSTANCE
                | UnlitFlags::BASE_COLOR_TEXTURE,
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth: wgpu::DepthStencilState {
                // Replaced with the pass's format when the pipeline is built.
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            },
            blend: None,
        }
    }

    /// Which of [`Self::flags`] are set, as WESL `@if` names paired with their
    /// state.
    ///
    /// Every name appears, so the composed variant never sees a name it does
    /// not know.
    pub fn features(&self) -> [(&'static str, bool); 8] {
        [
            (
                "VERTEX_POSITION",
                self.flags.contains(UnlitFlags::VERTEX_POSITION),
            ),
            (
                "UNCOMPRESSED_POSITION",
                self.flags.contains(UnlitFlags::UNCOMPRESSED_POSITION),
            ),
            ("VERTEX_UV", self.flags.contains(UnlitFlags::VERTEX_UV)),
            (
                "UNCOMPRESSED_UV",
                self.flags.contains(UnlitFlags::UNCOMPRESSED_UV),
            ),
            (
                "VERTEX_COLOR",
                self.flags.contains(UnlitFlags::VERTEX_COLOR),
            ),
            (
                "VERTEX_INSTANCE",
                self.flags.contains(UnlitFlags::VERTEX_INSTANCE),
            ),
            (
                "BASE_COLOR_TEXTURE",
                self.flags.contains(UnlitFlags::BASE_COLOR_TEXTURE),
            ),
            (
                "SRGB_TO_LINEAR_OUTPUT",
                self.flags.contains(UnlitFlags::SRGB_TO_LINEAR_OUTPUT),
            ),
        ]
    }

    /// Whether this variant reads a compressed channel and therefore needs the
    /// mesh-metadata bindings: the global group's storage buffer and the mesh
    /// group's [`MeshInfo`] uniform.
    ///
    /// Mirrors the shader's metadata condition, so the layouts and the
    /// composed variant agree on whether the bindings exist.
    pub fn needs_metadata(&self) -> bool {
        let compressed_position = self.flags.contains(UnlitFlags::VERTEX_POSITION)
            && !self.flags.contains(UnlitFlags::UNCOMPRESSED_POSITION);
        let compressed_uv = self.flags.contains(UnlitFlags::VERTEX_UV)
            && !self.flags.contains(UnlitFlags::UNCOMPRESSED_UV);
        compressed_position || compressed_uv
    }

    /// The UV-and-color vertex stream this variant expects.
    pub fn uv_color_stream(&self) -> MeshUvColorStream {
        MeshUvColorStream {
            flags: self.flags & MeshUvColorStream::FLAGS,
        }
    }
}

/// Failure reasons reported by [`compose_builtin`].
#[derive(Debug)]
enum ComposeError {
    /// The WESL compiler rejected the module.
    Compile(wesl::Error),
}

impl core::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "WESL compilation failed: {error}"),
        }
    }
}

impl std::error::Error for ComposeError {}

/// The optional per-vertex channels of the built-in pipeline's UV-and-color
/// vertex stream.
///
/// Each channel adds one attribute to [`UV_COLOR_SLOT`] in the order UV then
/// color, so the packed stream matches the shader's declared locations for
/// any combination. [`Self::write`] compresses the raw attributes and
/// interleaves them straight into the target buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshUvColorStream {
    /// The variant's own channels, narrowed to the three this stream writes:
    /// [`UnlitFlags::VERTEX_UV`], [`UnlitFlags::UNCOMPRESSED_UV`] and
    /// [`UnlitFlags::VERTEX_COLOR`].
    pub flags: UnlitFlags,
}

impl MeshUvColorStream {
    /// The stream flags this type reads.
    ///
    /// A variant often carries channels belonging to other streams; this mask
    /// selects the three that belong here, so [`UnlitOptions::uv_color_stream`]
    /// can hand over the whole set unchanged.
    const FLAGS: UnlitFlags = UnlitFlags::VERTEX_UV
        .union(UnlitFlags::UNCOMPRESSED_UV)
        .union(UnlitFlags::VERTEX_COLOR);

    /// Whether the UV attribute is written.
    pub fn uv(&self) -> bool {
        self.flags.contains(UnlitFlags::VERTEX_UV)
    }

    /// Whether the color attribute is written.
    pub fn color(&self) -> bool {
        self.flags.contains(UnlitFlags::VERTEX_COLOR)
    }

    /// Whether the UV is written full precision rather than compressed to
    /// `Snorm16x2`.
    ///
    /// An uncompressed UV needs no decode parameters, so [`Self::write`]
    /// leaves the metadata alone.
    pub fn uncompressed_uv(&self) -> bool {
        self.flags.contains(UnlitFlags::UNCOMPRESSED_UV)
    }

    /// Bytes per vertex of the packed stream.
    pub fn stride(&self) -> u32 {
        let uv = if self.uv() { self.uv_size() } else { 0 };
        let color = if self.color() {
            wgpu::VertexFormat::Unorm8x4.size() as u32
        } else {
            0
        };
        uv + color
    }

    /// Bytes the UV attribute occupies in this encoding.
    fn uv_size(&self) -> u32 {
        let format = if self.uncompressed_uv() {
            wgpu::VertexFormat::Float32x2
        } else {
            wgpu::VertexFormat::Snorm16x2
        };
        format.size() as u32
    }

    /// Whether the variant declares no attributes at all, in which case the
    /// slot must be omitted from the vertex-buffer list.
    pub fn is_empty(&self) -> bool {
        !self.uv() && !self.color()
    }

    /// Bytes one `vertex_count`-long stream occupies.
    pub fn byte_len(&self, vertex_count: usize) -> usize {
        vertex_count * self.stride() as usize
    }

    /// Compress the raw `uvs` and `colors` and write the interleaved result
    /// into `out`, which must be exactly [`Self::byte_len`]`(vertex_count)`
    /// bytes — normally a mapped-at-creation vertex buffer, so the vertices
    /// land in GPU memory without an intermediate byte buffer.
    ///
    /// `metadata` receives the UV decode parameters the compression derives,
    /// exactly as [`crate::mesh::compress_uvs`] would.
    ///
    /// Nothing is allocated: the compressed values are produced lazily and
    /// written as they are computed. Use [`Self::write_compressed`] to write
    /// channels that are already compressed, for example to share one stream
    /// across several meshes.
    ///
    /// # Panics
    /// If an enabled channel has no matching slice, if the channel lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write(
        &self,
        uvs: &[[f32; 2]],
        colors: &[[f32; 4]],
        metadata: &mut crate::mesh::MeshMetadata,
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = self.raw_vertex_count(uvs, colors);
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );
        if vertex_count == 0 {
            return;
        }

        // Deriving the UV range is the only pass over the input; the encoding
        // itself streams straight into `out`. An uncompressed channel is
        // copied as it is, so it never derives metadata.
        let (write_uv, write_color) = (self.uv(), self.color());
        let compress_uv = write_uv && !self.uncompressed_uv();
        let stride = self.stride() as usize;
        let mut packed_uvs = compress_uv
            .then(|| crate::mesh::compress_uvs(uvs, metadata))
            .into_iter()
            .flatten();
        let mut packed_colors = write_color
            .then(|| crate::mesh::compress_colors(colors))
            .into_iter()
            .flatten();

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_UV_COLOR_STRIDE];
            let mut len = 0;
            if write_uv {
                // A compressed UV is produced lazily as a temporary, so it
                // is copied out while it is still alive; an uncompressed one
                // is the caller's `f32` pair verbatim.
                if compress_uv {
                    let uv = packed_uvs.next().expect("one UV per vertex");
                    let bytes = uv.as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                } else {
                    let bytes = uvs[index].as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
            }
            if write_color {
                let color = packed_colors.next().expect("one color per vertex");
                let bytes = color.as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, stride);
            vertex.into_iter().take(len)
        }));
    }

    /// Write the already compressed `uvs` and `colors` into `out` as the
    /// interleaved vertex stream, which must be exactly
    /// [`Self::byte_len`]`(vertex_count)` bytes.
    ///
    /// Takes the packed form produced by [`crate::mesh::compress_uvs`] and
    /// [`crate::mesh::compress_colors`], so one compressed stream can be
    /// uploaded for several meshes without recompressing it.
    ///
    /// # Panics
    /// If an enabled channel has no matching slice, if the channel lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write_compressed(
        &self,
        uvs: &[CompressedUv],
        colors: &[CompressedColor],
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = self.compressed_vertex_count(uvs, colors);
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out, so writing never allocates at all.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_UV_COLOR_STRIDE];
            let mut len = 0;
            if self.uv() {
                let bytes = uvs[index].as_bytes();
                vertex[..bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            if self.color() {
                let bytes = colors[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, self.stride() as usize);
            vertex.into_iter().take(len)
        }));
    }

    /// Number of vertices the raw channel slices describe.
    fn raw_vertex_count(&self, uvs: &[[f32; 2]], colors: &[[f32; 4]]) -> usize {
        match (self.uv(), self.color()) {
            (true, true) => {
                assert_eq!(
                    uvs.len(),
                    colors.len(),
                    "the UV and color streams must describe the same vertices"
                );
                uvs.len()
            }
            (true, false) => uvs.len(),
            (false, true) => colors.len(),
            (false, false) => 0,
        }
    }

    /// Number of vertices the compressed channel slices describe.
    fn compressed_vertex_count(&self, uvs: &[CompressedUv], colors: &[CompressedColor]) -> usize {
        match (self.uv(), self.color()) {
            (true, true) => {
                assert_eq!(
                    uvs.len(),
                    colors.len(),
                    "the UV and color streams must describe the same vertices"
                );
                uvs.len()
            }
            (true, false) => uvs.len(),
            (false, true) => colors.len(),
            (false, false) => 0,
        }
    }
}

/// Bytes per vertex of the widest UV-and-color stream, used to size the
/// per-vertex scratch the writer assembles on the stack.
///
/// The widest encoding the stream can write is an uncompressed UV followed by
/// the color, so the scratch covers those two attribute formats.
const MAX_UV_COLOR_STRIDE: usize =
    wgpu::VertexFormat::Float32x2.size() as usize + wgpu::VertexFormat::Unorm8x4.size() as usize;

/// The built-in unlit pipeline, its layouts and its vertex-buffer
/// declarations.
///
/// Created once and reused for every draw that shares the variant; the
/// resources it owns can be registered with a [`crate::resources::ResourceGraph`]
/// so a render-target change rebuilds it.
pub struct UnlitPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Layout of the global bind group (index 0).
    pub global_layout: wgpu::BindGroupLayout,
    /// Layout of the material bind group (index 1), present only when the
    /// variant samples a base-color texture.
    pub material_layout: Option<wgpu::BindGroupLayout>,
    /// Layout of the mesh bind group (index 2), present only when the variant
    /// reads a compressed channel and therefore decodes mesh metadata.
    pub mesh_layout: Option<wgpu::BindGroupLayout>,
    /// The variant this pipeline was built for.
    pub options: UnlitOptions,
}

impl UnlitPipeline {
    /// Compose the built-in `unlit.wesl` for `options` and build the
    /// pipeline.
    ///
    /// `color_format` and `sample_count` must match the render target the
    /// pipeline will be used with; `depth_format` must match its depth
    /// attachment, or be `None` for a pass without depth. When it is `Some`,
    /// the format replaces the one [`UnlitOptions::depth`] declares, so the
    /// options carry the comparison and the write mask and the pass carries
    /// the format.
    ///
    /// The fragment entry point follows `color_format`: a target that encodes
    /// sRGB needs its input converted to linear, and the format is the
    /// authority on whether it does.
    pub fn new(
        device: &wgpu::Device,
        options: &UnlitOptions,
        color_format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
        sample_count: u32,
    ) -> Self {
        let wgsl = compose_builtin(options).expect("the built-in unlit shader composes");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgpu_unlit_render::unlit"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let global_layout = {
            // Mesh metadata solves a compressed channel; a variant that reads
            // every channel uncompressed declares no binding for it.
            let mut entries = arrayvec::ArrayVec::<wgpu::BindGroupLayoutEntry, 3>::new();
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: CAMERA_BINDING,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(
                        <crate::globals::View as const_shader_layout::ShaderLayout>::SIZE,
                    ),
                },
                count: None,
            });
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: FRAME_BINDING,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(
                        <crate::globals::Globals as const_shader_layout::ShaderLayout>::SIZE,
                    ),
                },
                count: None,
            });
            if options.needs_metadata() {
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: MESH_METADATA_BINDING,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        // The element stride, not a capacity multiple: the
                        // buffer may grow without invalidating the layout.
                        min_binding_size: Some(
                            <crate::mesh::MeshMetadata as const_shader_layout::ShaderLayout>::SIZE,
                        ),
                    },
                    count: None,
                });
            }
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("wgpu_unlit_render::unlit::globals"),
                entries: &entries,
            })
        };

        let material_layout = options
            .flags
            .contains(UnlitFlags::BASE_COLOR_TEXTURE)
            .then(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("wgpu_unlit_render::unlit::material"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: BASE_COLOR_TEXTURE_BINDING,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: BASE_COLOR_SAMPLER_BINDING,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                })
            });

        let mesh_layout = options.needs_metadata().then(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("wgpu_unlit_render::unlit::mesh"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: MESH_INFO_BINDING,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Some(
                            <MeshInfo as const_shader_layout::ShaderLayout>::SIZE,
                        ),
                    },
                    count: None,
                }],
            })
        });

        let layouts = [
            Some(&global_layout),
            material_layout.as_ref(),
            mesh_layout.as_ref(),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgpu_unlit_render::unlit::layout"),
            bind_group_layouts: &layouts,
            immediate_size: 0,
        });

        let vertex_buffers = Self::vertex_buffer_layouts(options);
        // A strip or line-list topology resets its strip at the primitive
        // restart index, whose width follows the index buffer the draw uses.
        let strip_index_format =
            (options.primitive.topology.is_strip()).then_some(wgpu::IndexFormat::Uint32);
        let primitive = wgpu::PrimitiveState {
            strip_index_format,
            ..options.primitive
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgpu_unlit_render::unlit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(VS_MAIN),
                compilation_options: Default::default(),
                buffers: &vertex_buffers,
            },
            primitive,
            // A pass with a depth attachment requires every pipeline it uses
            // to declare a matching state, so the pass's format replaces the
            // one the options carry and their comparison and write mask are
            // kept.
            depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
                format,
                ..options.depth.clone()
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(FS_MAIN),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: options.blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            global_layout,
            material_layout,
            mesh_layout,
            options: options.clone(),
        }
    }

    /// The vertex-buffer declarations the built-in shader expects, in slot
    /// order.
    ///
    /// A slot whose variant declares no attributes — the UV-and-color slot
    /// without either channel, or the instance slot without
    /// [`UnlitFlags::VERTEX_INSTANCE`] — is reported as `None`, so a variant
    /// never binds a buffer the shader does not declare.
    pub fn vertex_buffer_layouts(
        options: &UnlitOptions,
    ) -> [Option<wgpu::VertexBufferLayout<'static>>; 3] {
        let flags = options.flags;

        // One static slice per position encoding: the layout must be
        // `'static` to live on the pipeline descriptor.
        const POSITION_COMPRESSED: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::POSITION => Snorm16x4];
        const POSITION_UNCOMPRESSED: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::POSITION => Float32x3];

        // One static slice per UV / color combination.
        const UV_COMPRESSED_ONLY: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::UV => Snorm16x2];
        const UV_UNCOMPRESSED_ONLY: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::UV => Float32x2];
        const COLOR_ONLY: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::COLOR => Unorm8x4];
        const UV_COMPRESSED_COLOR: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![location::UV => Snorm16x2, location::COLOR => Unorm8x4];
        const UV_UNCOMPRESSED_COLOR: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![location::UV => Float32x2, location::COLOR => Unorm8x4];

        let position_attributes = flags.contains(UnlitFlags::VERTEX_POSITION).then(|| {
            if flags.contains(UnlitFlags::UNCOMPRESSED_POSITION) {
                &POSITION_UNCOMPRESSED[..]
            } else {
                &POSITION_COMPRESSED[..]
            }
        });

        let stream = options.uv_color_stream();
        let uncompressed_uv = stream.uncompressed_uv();
        let uv_color_attributes: Option<&'static [wgpu::VertexAttribute]> =
            match (stream.uv(), stream.color()) {
                (false, false) => None,
                (true, false) if uncompressed_uv => Some(&UV_UNCOMPRESSED_ONLY),
                (true, false) => Some(&UV_COMPRESSED_ONLY),
                (false, true) => Some(&COLOR_ONLY),
                (true, true) if uncompressed_uv => Some(&UV_UNCOMPRESSED_COLOR),
                (true, true) => Some(&UV_COMPRESSED_COLOR),
            };

        const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            location::MODEL_0 => Float32x4,
            location::MODEL_1 => Float32x4,
            location::MODEL_2 => Float32x4,
            location::BASE_COLOR => Float32x4,
        ];

        [
            position_attributes
                .map(|attributes| vertex_layout(attributes, wgpu::VertexStepMode::Vertex)),
            uv_color_attributes
                .map(|attributes| vertex_layout(attributes, wgpu::VertexStepMode::Vertex)),
            flags
                .contains(UnlitFlags::VERTEX_INSTANCE)
                .then(|| vertex_layout(&INSTANCE_ATTRIBUTES, wgpu::VertexStepMode::Instance)),
        ]
    }
}

/// Build a tightly-packed vertex-buffer layout, deriving the stride from the
/// attribute formats.
///
/// The attributes are `'static` so the resulting layout can outlive this call
/// and be stored on the pipeline descriptor.
///
/// # Panics
/// If the derived stride is not a multiple of [`wgpu::VERTEX_ALIGNMENT`].
fn vertex_layout(
    attributes: &'static [wgpu::VertexAttribute],
    step_mode: wgpu::VertexStepMode,
) -> wgpu::VertexBufferLayout<'static> {
    let array_stride = attributes
        .iter()
        .map(|attribute| attribute.format.size())
        .sum::<u64>();
    assert!(
        array_stride.is_multiple_of(wgpu::VERTEX_ALIGNMENT),
        "vertex stride {array_stride} must be a multiple of wgpu::VERTEX_ALIGNMENT"
    );
    wgpu::VertexBufferLayout {
        array_stride,
        step_mode,
        attributes,
    }
}

/// Compose the built-in `unlit.wesl` for `options`.
///
/// # Panics
/// If the flags contradict each other: a channel cannot be uncompressed
/// without being read, and the base-color texture is sampled with the
/// per-vertex UV.
fn compose_builtin(options: &UnlitOptions) -> Result<String, ComposeError> {
    let flags = options.flags;
    assert!(
        !flags.contains(UnlitFlags::UNCOMPRESSED_POSITION)
            || flags.contains(UnlitFlags::VERTEX_POSITION),
        "an uncompressed position is a position channel, so \
         `UNCOMPRESSED_POSITION` requires `VERTEX_POSITION`"
    );
    assert!(
        !flags.contains(UnlitFlags::UNCOMPRESSED_UV) || flags.contains(UnlitFlags::VERTEX_UV),
        "an uncompressed UV is a UV channel, so `UNCOMPRESSED_UV` \
         requires `VERTEX_UV`"
    );
    assert!(
        !flags.contains(UnlitFlags::BASE_COLOR_TEXTURE) || flags.contains(UnlitFlags::VERTEX_UV),
        "the base-color texture is sampled with the per-vertex UV, so \
         `BASE_COLOR_TEXTURE` requires `VERTEX_UV`"
    );

    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("wgpu_unlit_render".to_owned()),
        vec!["unlit".to_owned()],
    );

    let mut compile_options = wesl::CompileOptions::default();
    for (name, enabled) in options.features() {
        compile_options.features.set(name, enabled);
    }
    compile_options.keep_main = true;

    let mut resolver = wesl::resolver::PackageResolver::new();
    resolver.add_package(&crate::shader::PACKAGE);

    wesl::Compiler::new_with_resolver(compile_options, resolver)
        .compile_module(&main_path)
        .map(|result| result.syntax.to_string())
        .map_err(ComposeError::Compile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::IntoBytes;

    /// A caller composes their own entry shader against the built-in package
    /// with `wesl` directly: [`crate::shader`] is the `StaticPackage` that
    /// resolves `import wgpu_unlit_render::…`.
    #[test]
    fn a_caller_composes_against_the_built_in_package() {
        let source = "\
import wgpu_unlit_render::mesh_compression;
import wgpu_unlit_render::mesh_metadata::MeshMetadata;
import wgpu_unlit_render::view::View;

@group(0) @binding(0) var<uniform> camera: View;
@group(0) @binding(2) var<storage, read> mesh_meta: array<MeshMetadata>;

@vertex
fn vs_main(@location(0) position: vec4<f32>) -> @builtin(position) vec4<f32> {
    let decoded = mesh_compression::decode_position(position.xyz, mesh_meta[0]);
    return camera.clip_from_world * vec4<f32>(decoded, 1.0);
}
";
        // The caller's own module is virtual here; a real one would come from
        // a `FileResolver`. Anything not under the local namespace falls
        // through to the built-in package, which is how
        // `import wgpu_unlit_render::mesh_compression;` resolves.
        let namespace = wesl::syntax::ModulePath::new(
            wesl::syntax::PathOrigin::Absolute,
            vec!["app".to_owned()],
        );
        let main_path = wesl::syntax::ModulePath::new(
            wesl::syntax::PathOrigin::Absolute,
            vec!["app".to_owned(), "main".to_owned()],
        );
        let mut virtual_modules = wesl::resolver::VirtualResolver::new();
        virtual_modules.add_module(
            wesl::syntax::ModulePath::new(wesl::syntax::PathOrigin::Absolute, vec!["main".into()]),
            source.into(),
        );

        let mut resolver = wesl::resolver::Router::new();
        resolver.mount_resolver(namespace, virtual_modules);
        resolver.mount_fallback_resolver({
            let mut packages = wesl::resolver::PackageResolver::new();
            packages.add_package(&crate::shader::PACKAGE);
            packages
        });

        let options = wesl::CompileOptions {
            keep_main: true,
            ..Default::default()
        };
        let wgsl = wesl::Compiler::new_with_resolver(options, resolver)
            .compile_module(&main_path)
            .expect("the caller's module composes")
            .syntax
            .to_string();

        // The imports resolved into the composed module.
        assert!(wgsl.contains("fn vs_main"));
        assert!(wgsl.contains("decode_position"));
    }

    /// Every flag combination that composes.
    ///
    /// `BASE_COLOR_TEXTURE` implies `VERTEX_UV`, and an uncompressed channel
    /// implies its channel, so the invalid combinations are skipped.
    fn all_variants() -> Vec<UnlitOptions> {
        let mut variants = Vec::new();
        for vertex_position in [false, true] {
            for uncompressed_position in [false, true] {
                for vertex_uv in [false, true] {
                    for uncompressed_uv in [false, true] {
                        for vertex_color in [false, true] {
                            for vertex_instance in [false, true] {
                                for base_color_texture in [false, true] {
                                    let mut flags = UnlitFlags::empty();
                                    flags.set(UnlitFlags::VERTEX_POSITION, vertex_position);
                                    flags.set(
                                        UnlitFlags::UNCOMPRESSED_POSITION,
                                        uncompressed_position,
                                    );
                                    flags.set(UnlitFlags::VERTEX_UV, vertex_uv);
                                    flags.set(UnlitFlags::UNCOMPRESSED_UV, uncompressed_uv);
                                    flags.set(UnlitFlags::VERTEX_COLOR, vertex_color);
                                    flags.set(UnlitFlags::VERTEX_INSTANCE, vertex_instance);
                                    flags.set(UnlitFlags::BASE_COLOR_TEXTURE, base_color_texture);
                                    if base_color_texture && !vertex_uv {
                                        continue;
                                    }
                                    if uncompressed_position && !vertex_position {
                                        continue;
                                    }
                                    if uncompressed_uv && !vertex_uv {
                                        continue;
                                    }
                                    // A variant reading nothing composes an
                                    // empty vertex-input struct.
                                    if flags.is_empty() {
                                        continue;
                                    }
                                    variants.push(UnlitOptions {
                                        flags,
                                        ..UnlitOptions::standard()
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        variants
    }

    /// `SRGB_TO_LINEAR_OUTPUT` decides what the fragment writes: plain values
    /// for a target that stores what it is given, linear light for one that
    /// encodes sRGB. Both variants carry the same single entry point.
    #[test]
    fn srgb_output_flag_switches_the_conversion() {
        let composed = |flags: UnlitFlags| {
            let options = UnlitOptions {
                flags: UnlitOptions::standard().flags | flags,
                ..UnlitOptions::standard()
            };
            compose_builtin(&options).expect("compose")
        };

        let plain = composed(UnlitFlags::empty());
        assert!(plain.contains("fn fs_main"), "one entry point either way");
        assert!(
            !plain.contains("srgb_to_linear(color.r)"),
            "a plain target stores what it is given"
        );

        let converted = composed(UnlitFlags::SRGB_TO_LINEAR_OUTPUT);
        // Only RGB converts: alpha is coverage, not color.
        assert!(converted.contains("srgb_to_linear(color.r)"));
        assert!(converted.contains("srgb_to_linear(color.g)"));
        assert!(converted.contains("srgb_to_linear(color.b)"));
        assert!(converted.contains("color.a"), "alpha passes through");
        assert!(!converted.contains("srgb_to_linear(color.a)"));
    }

    #[test]
    fn composes_each_variant_to_valid_wgsl() {
        for options in all_variants() {
            let wgsl = compose_builtin(&options)
                .unwrap_or_else(|error| panic!("variant {options:?} failed: {error}"));
            assert!(wgsl.contains("fn vs_main"), "variant {options:?}");
            assert!(wgsl.contains("fn fs_main"), "variant {options:?}");
            // Disabled features must leave no conditional attributes behind.
            assert!(!wgsl.contains("@if"), "variant {options:?} kept @if");

            let flags = options.flags;
            // Each channel appears exactly when its flag is on.
            assert_eq!(
                wgsl.contains("base_color_tex"),
                flags.contains(UnlitFlags::BASE_COLOR_TEXTURE),
                "variant {options:?}"
            );
            assert_eq!(
                wgsl.contains("decode_uv"),
                flags.contains(UnlitFlags::VERTEX_UV)
                    && !flags.contains(UnlitFlags::UNCOMPRESSED_UV),
                "variant {options:?}"
            );
            let compressed_position = flags.contains(UnlitFlags::VERTEX_POSITION)
                && !flags.contains(UnlitFlags::UNCOMPRESSED_POSITION);
            assert_eq!(
                wgsl.contains("decode_position"),
                compressed_position,
                "variant {options:?}"
            );
            // The metadata bindings exist only while a channel is compressed.
            assert_eq!(
                wgsl.contains("mesh_meta"),
                options.needs_metadata(),
                "variant {options:?}"
            );
            assert_eq!(
                wgsl.contains("mesh_info"),
                options.needs_metadata(),
                "variant {options:?}"
            );
            // Without a position stream the vertex input declares no
            // position attribute, so the shader cannot read one.
            let vertex_input = wgsl
                .split_once("struct VertexInput {")
                .expect("the variant declares vertex inputs")
                .1;
            let vertex_input = vertex_input
                .split_once('}')
                .expect("the vertex input struct ends")
                .0;
            assert_eq!(
                vertex_input.contains("position:"),
                flags.contains(UnlitFlags::VERTEX_POSITION),
                "variant {options:?}"
            );
        }
    }

    /// A flag combination that contradicts itself must be rejected rather than
    /// composed into a shader that cannot work.
    #[test]
    fn contradictory_flags_are_rejected() {
        for flags in [
            UnlitFlags::BASE_COLOR_TEXTURE,
            UnlitFlags::BASE_COLOR_TEXTURE | UnlitFlags::VERTEX_POSITION,
            UnlitFlags::UNCOMPRESSED_POSITION,
            UnlitFlags::UNCOMPRESSED_UV,
        ] {
            let result = std::panic::catch_unwind(|| {
                compose_builtin(&UnlitOptions {
                    flags,
                    ..UnlitOptions::standard()
                })
            });
            assert!(result.is_err(), "flags {flags:?} must be rejected");
        }
    }

    #[test]
    fn vertex_strides_follow_the_attribute_formats() {
        let layouts = UnlitPipeline::vertex_buffer_layouts(&UnlitOptions {
            flags: UnlitFlags::VERTEX_POSITION
                | UnlitFlags::VERTEX_UV
                | UnlitFlags::VERTEX_INSTANCE,
            ..UnlitOptions::standard()
        });
        let position = layouts[POSITION_SLOT as usize]
            .as_ref()
            .expect("position slot");
        assert_eq!(position.array_stride, wgpu::VertexFormat::Snorm16x4.size());

        let uv_color = layouts[UV_COLOR_SLOT as usize].as_ref().expect("uv slot");
        assert_eq!(uv_color.array_stride, wgpu::VertexFormat::Snorm16x2.size());

        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot");
        assert_eq!(
            instance.array_stride,
            wgpu::VertexFormat::Float32x4.size() * 4
        );
        assert_eq!(instance.step_mode, wgpu::VertexStepMode::Instance);
    }

    /// An uncompressed channel widens its slot to the full-precision format
    /// and drops the metadata bindings it no longer needs.
    #[test]
    fn uncompressed_channels_widen_their_slots() {
        // The screen-space flags: uncompressed channels and no per-instance
        // stream.
        let options = UnlitOptions {
            flags: UnlitFlags::VERTEX_POSITION
                | UnlitFlags::UNCOMPRESSED_POSITION
                | UnlitFlags::VERTEX_UV
                | UnlitFlags::UNCOMPRESSED_UV
                | UnlitFlags::VERTEX_COLOR
                | UnlitFlags::BASE_COLOR_TEXTURE,
            ..UnlitOptions::standard()
        };
        assert!(!options.needs_metadata());

        let layouts = UnlitPipeline::vertex_buffer_layouts(&options);
        let position = layouts[POSITION_SLOT as usize]
            .as_ref()
            .expect("position slot");
        assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x3.size());
        assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x3);

        let uv_color = layouts[UV_COLOR_SLOT as usize].as_ref().expect("uv slot");
        assert_eq!(
            uv_color.array_stride,
            wgpu::VertexFormat::Float32x2.size() + wgpu::VertexFormat::Unorm8x4.size()
        );

        // Screen-space draws need no per-instance stream.
        assert!(layouts[INSTANCE_SLOT as usize].is_none());
    }

    #[test]
    fn uv_color_slot_follows_its_channels() {
        let stride = |flags: UnlitFlags| {
            UnlitPipeline::vertex_buffer_layouts(&UnlitOptions {
                flags,
                ..UnlitOptions::standard()
            })[UV_COLOR_SLOT as usize]
                .as_ref()
                .map(|layout| layout.array_stride)
        };

        let uv = wgpu::VertexFormat::Snorm16x2.size();
        let uv_uncompressed = wgpu::VertexFormat::Float32x2.size();
        let color = wgpu::VertexFormat::Unorm8x4.size();

        // No channel: the slot disappears entirely, so a variant never binds
        // a buffer the shader does not declare.
        assert_eq!(stride(UnlitFlags::empty()), None);
        assert_eq!(stride(UnlitFlags::VERTEX_UV), Some(uv));
        assert_eq!(
            stride(UnlitFlags::VERTEX_UV | UnlitFlags::UNCOMPRESSED_UV),
            Some(uv_uncompressed)
        );
        assert_eq!(stride(UnlitFlags::VERTEX_COLOR), Some(color));
        assert_eq!(
            stride(UnlitFlags::VERTEX_UV | UnlitFlags::VERTEX_COLOR),
            Some(uv + color)
        );
        assert_eq!(
            stride(UnlitFlags::VERTEX_UV | UnlitFlags::UNCOMPRESSED_UV | UnlitFlags::VERTEX_COLOR),
            Some(uv_uncompressed + color)
        );
    }

    /// The position slot follows its flag, so a position-less variant binds no
    /// position buffer at all.
    #[test]
    fn position_slot_follows_its_flag() {
        let position = |flags: UnlitFlags| {
            UnlitPipeline::vertex_buffer_layouts(&UnlitOptions {
                flags,
                ..UnlitOptions::standard()
            })[POSITION_SLOT as usize]
                .as_ref()
                .map(|layout| (layout.array_stride, layout.step_mode))
        };

        assert_eq!(
            position(UnlitFlags::empty()),
            None,
            "no position stream means no position slot"
        );
        assert_eq!(
            position(UnlitFlags::VERTEX_POSITION),
            Some((
                wgpu::VertexFormat::Snorm16x4.size(),
                wgpu::VertexStepMode::Vertex
            ))
        );
        assert_eq!(
            position(UnlitFlags::VERTEX_POSITION | UnlitFlags::UNCOMPRESSED_POSITION),
            Some((
                wgpu::VertexFormat::Float32x3.size(),
                wgpu::VertexStepMode::Vertex
            ))
        );
    }

    /// Compress raw channels, write them through the mapped-buffer path and
    /// return the bytes.
    fn write_stream(
        stream: MeshUvColorStream,
        uvs: &[[f32; 2]],
        colors: &[[f32; 4]],
        vertex_count: usize,
        metadata: &mut crate::mesh::MeshMetadata,
    ) -> Vec<u8> {
        let mut out = vec![0u8; stream.byte_len(vertex_count)];
        stream.write(
            uvs,
            colors,
            metadata,
            WriteOnly::from_mut(out.as_mut_slice()),
        );
        out
    }

    /// Both writers must produce byte-identical streams, since the raw one
    /// compresses into exactly what the compressed one expects.
    #[test]
    fn raw_and_compressed_writers_agree() {
        let uvs = [[0.0f32, 0.25], [0.5, 1.0], [1.0, 0.0]];
        let colors = [
            [0.0f32, 0.25, 0.5, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.2, 0.4, 0.6, 0.8],
        ];

        for stream in [
            MeshUvColorStream {
                flags: UnlitFlags::VERTEX_UV | UnlitFlags::VERTEX_COLOR,
            },
            MeshUvColorStream {
                flags: UnlitFlags::VERTEX_UV,
            },
            MeshUvColorStream {
                flags: UnlitFlags::VERTEX_COLOR,
            },
        ] {
            let mut metadata = crate::mesh::MeshMetadata::default();
            let from_raw = write_stream(stream, &uvs, &colors, 3, &mut metadata);

            // Compress separately, exactly as a caller sharing one stream
            // across meshes would.
            let packed_uvs: Vec<_> = if stream.uv() {
                crate::mesh::compress_uvs(&uvs, &mut metadata).collect()
            } else {
                Vec::new()
            };
            let packed_colors: Vec<_> = if stream.color() {
                crate::mesh::compress_colors(&colors).collect()
            } else {
                Vec::new()
            };
            let mut from_compressed = vec![0u8; stream.byte_len(3)];
            stream.write_compressed(
                &packed_uvs,
                &packed_colors,
                WriteOnly::from_mut(from_compressed.as_mut_slice()),
            );

            assert_eq!(from_raw, from_compressed, "stream {stream:?}");
            assert_eq!(from_raw.len(), stream.byte_len(3), "stream {stream:?}");
        }
    }

    #[test]
    fn uv_color_stream_interleaves_in_attribute_order() {
        let uvs = [[0.0f32, 0.0], [1.0, 1.0]];
        // Colors are linear RGBA in [0, 1]; they quantize to Unorm8.
        let colors = [[0.0f32, 0.25, 0.5, 1.0], [1.0, 0.0, 0.0, 1.0]];
        let mut metadata = crate::mesh::MeshMetadata::default();

        let out = write_stream(
            MeshUvColorStream {
                flags: UnlitFlags::VERTEX_UV | UnlitFlags::VERTEX_COLOR,
            },
            &uvs,
            &colors,
            2,
            &mut metadata,
        );
        assert_eq!(out.len(), 2 * 8);
        // Vertex 0 starts with the UV then the color, matching the shader's
        // location order 1 then 2. The UV remaps [0, 1] to Snorm16 [-1, 1],
        // so 0.0 quantizes to -32767 (little-endian) and 1.0 to 32767.
        let expected_uv = [(-32767i16).to_le_bytes(), (-32767i16).to_le_bytes()].concat();
        assert_eq!(&out[0..4], &expected_uv[..]);
        assert_eq!(&out[4..8], &[0, 64, 128, 255]);
        // The second vertex sits at the far corner of the UV range.
        assert_eq!(
            &out[8..12],
            &[32767i16.to_le_bytes(), 32767i16.to_le_bytes()].concat()
        );

        // The UV compression still produced the decode parameters.
        assert_eq!(
            metadata.uv_min_and_extents,
            glam::Vec4::new(0.0, 0.0, 1.0, 1.0)
        );

        // UV only: four bytes per vertex.
        let out = write_stream(
            MeshUvColorStream {
                flags: UnlitFlags::VERTEX_UV,
            },
            &uvs,
            &[],
            2,
            &mut crate::mesh::MeshMetadata::default(),
        );
        assert_eq!(out.len(), 2 * 4);

        // Color only: four bytes per vertex, no UV bytes.
        let out = write_stream(
            MeshUvColorStream {
                flags: UnlitFlags::VERTEX_COLOR,
            },
            &[],
            &colors,
            2,
            &mut crate::mesh::MeshMetadata::default(),
        );
        assert_eq!(out.len(), 2 * 4);
        assert_eq!(&out[0..4], &[0, 64, 128, 255]);
    }

    /// An uncompressed UV is copied at full precision: it neither derives
    /// metadata nor quantizes, so the bytes are the input `f32`s.
    #[test]
    fn uncompressed_uv_is_written_at_full_precision() {
        let uvs = [[0.0f32, 0.0], [0.5, 0.75]];
        let stream = MeshUvColorStream {
            flags: UnlitFlags::VERTEX_UV | UnlitFlags::UNCOMPRESSED_UV,
        };
        let mut metadata = crate::mesh::MeshMetadata::default();
        let out = write_stream(stream, &uvs, &[], 2, &mut metadata);

        let stride = wgpu::VertexFormat::Float32x2.size() as usize;
        assert_eq!(out.len(), 2 * stride);
        for (vertex, uv) in out.chunks(stride).zip(&uvs) {
            assert_eq!(vertex, uv.as_bytes());
        }
        // No compression happened, so the metadata is untouched.
        assert_eq!(metadata, crate::mesh::MeshMetadata::default());
    }

    /// The instance slot's GPU attribute offsets and stride must match the
    /// CPU struct. This is the vertex-buffer equivalent of the shader-layout
    /// validation that `const_shader_layout` gives the uniform and storage
    /// structs: vertex attributes are addressed by the offsets declared here,
    /// not by WGSL alignment rules.
    #[test]
    fn instance_vertex_layout_matches_the_struct() {
        use crate::mesh::MeshInstance;
        use core::mem::{offset_of, size_of};

        let layouts = UnlitPipeline::vertex_buffer_layouts(&UnlitOptions {
            flags: UnlitFlags::VERTEX_INSTANCE,
            ..UnlitOptions::standard()
        });
        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot");

        assert_eq!(
            instance.array_stride,
            size_of::<MeshInstance>() as u64,
            "the instance stride must step exactly one MeshInstance"
        );

        let model = offset_of!(MeshInstance, model) as u64;
        let vec4 = size_of::<glam::Vec4>() as u64;
        let expected = [
            model,
            model + vec4,
            model + 2 * vec4,
            offset_of!(MeshInstance, base_color) as u64,
        ];
        let actual: Vec<u64> = instance.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(
            actual, expected,
            "attribute offsets must line up with the MeshInstance fields"
        );

        // The shader reads three matrix columns followed by the base color,
        // each as one `vec4`.
        let locations: Vec<u32> = instance
            .attributes
            .iter()
            .map(|a| a.shader_location)
            .collect();
        assert_eq!(
            locations,
            [
                location::MODEL_0,
                location::MODEL_1,
                location::MODEL_2,
                location::BASE_COLOR
            ]
        );
        assert!(
            instance
                .attributes
                .iter()
                .all(|a| a.format == wgpu::VertexFormat::Float32x4),
            "every instance attribute is one vec4 column"
        );
    }
}
