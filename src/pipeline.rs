//! The built-in unlit pipeline and WESL shader composition.
//!
//! [`compose`] runs the WESL compiler over a main module, resolving imports
//! against the built-in package ([`crate::shader`]) plus the caller's own
//! modules, and returns the WGSL that a [`wgpu::ShaderModule`] needs. Custom
//! shaders import the built-in modules directly:
//!
//! ```wgsl
//! import wgpu_unlit_render::mesh_compression;
//! import wgpu_unlit_render::view::View;
//! ```
//!
//! [`UnlitPipeline`] is the ready-made pipeline: it composes `unlit.wesl`
//! with a variant selected by [`UnlitOptions`], builds the layouts the shader
//! expects, and exposes the vertex-buffer layouts the caller declares when
//! creating meshes.

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

/// The variant of the built-in unlit shader to compose.
///
/// [`Self::default`] enables nothing, which draws a single point at each
/// instance's origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnlitOptions {
    /// Read the compressed per-vertex position.
    ///
    /// Without it the geometry is a single point at the instance origin, so
    /// the draw needs no position vertex buffer — what point, particle and
    /// impostor draws want, where the per-instance data alone places the
    /// vertex.
    pub vertex_position: bool,
    /// Read and decode the per-vertex UV. Required by
    /// [`Self::base_color_texture`], which samples with it.
    pub vertex_uv: bool,
    /// Sample a base-color texture from the material group.
    pub base_color_texture: bool,
    /// Read a per-vertex color and multiply it into the base color.
    pub vertex_color: bool,
}

impl UnlitOptions {
    /// The feature flags this variant enables, as WESL `@if` names.
    pub fn features(&self) -> [(&'static str, bool); 4] {
        [
            ("VERTEX_POSITION", self.vertex_position),
            ("VERTEX_UV", self.vertex_uv),
            ("BASE_COLOR_TEXTURE", self.base_color_texture),
            ("VERTEX_COLOR", self.vertex_color),
        ]
    }

    /// The UV-and-color vertex stream this variant expects.
    pub fn uv_color_stream(&self) -> MeshUvColorStream {
        MeshUvColorStream {
            uv: self.vertex_uv,
            color: self.vertex_color,
        }
    }
}

/// Failure reasons reported by [`compose`].
#[derive(Debug)]
pub enum ComposeError {
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
    /// Include the `Snorm16x2` UV attribute at location 1.
    pub uv: bool,
    /// Include the `Unorm8x4` color attribute at location 2.
    pub color: bool,
}

impl MeshUvColorStream {
    /// Bytes per vertex of the packed stream.
    pub fn stride(&self) -> u32 {
        let uv = if self.uv {
            wgpu::VertexFormat::Snorm16x2.size() as u32
        } else {
            0
        };
        let color = if self.color {
            wgpu::VertexFormat::Unorm8x4.size() as u32
        } else {
            0
        };
        uv + color
    }

    /// Whether the variant declares no attributes at all, in which case the
    /// slot must be omitted from the vertex-buffer list.
    pub fn is_empty(&self) -> bool {
        !self.uv && !self.color
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
        // itself streams straight into `out`.
        let (write_uv, write_color) = (self.uv, self.color);
        let stride = self.stride() as usize;
        let mut packed_uvs = write_uv
            .then(|| crate::mesh::compress_uvs(uvs, metadata))
            .into_iter()
            .flatten();
        let mut packed_colors = write_color
            .then(|| crate::mesh::compress_colors(colors))
            .into_iter()
            .flatten();

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out.
        out.write_iter((0..vertex_count).flat_map(move |_| {
            let mut vertex = [0u8; MAX_UV_COLOR_STRIDE];
            let mut len = 0;
            if write_uv {
                let uv = packed_uvs.next().expect("one UV per vertex");
                let bytes = uv.as_bytes();
                vertex[..bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
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
            if self.uv {
                let bytes = uvs[index].as_bytes();
                vertex[..bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            if self.color {
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
        match (self.uv, self.color) {
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
        match (self.uv, self.color) {
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
const MAX_UV_COLOR_STRIDE: usize =
    wgpu::VertexFormat::Snorm16x2.size() as usize + wgpu::VertexFormat::Unorm8x4.size() as usize;

/// Compose the WESL `main_module` (a module path inside `package_root`) into
/// WGSL.
///
/// Imports resolve against the built-in package, so a main module can import
/// `wgpu_unlit_render::*` modules as well as its own siblings. `features`
/// toggles WESL `@if` flags; flags left unset are disabled.
///
/// ```no_run
/// # use wgpu_unlit_render::pipeline::compose;
/// let wgsl = compose("shaders", "main", &[("VERTEX_COLOR", true)]).unwrap();
/// ```
pub fn compose(
    package_root: impl AsRef<std::path::Path>,
    main_module: &str,
    features: &[(&str, bool)],
) -> Result<String, ComposeError> {
    // `package::main_module` — the `package` prefix sets the absolute origin.
    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Absolute,
        vec![main_module.to_owned()],
    );

    let mut options = wesl::CompileOptions::default();
    for (name, enabled) in features {
        options.features.set(*name, *enabled);
    }
    // Keep every entry point so a caller can address several of them from one
    // module (for example a depth-only and a color pass).
    options.keep_main = true;

    let mut resolver = wesl::resolver::StandardResolver::new(package_root);
    resolver.add_package(&crate::shader::PACKAGE);

    wesl::Compiler::new_with_resolver(options, resolver)
        .compile_module(&main_path)
        .map(|result| result.syntax.to_string())
        .map_err(ComposeError::Compile)
}

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
    /// Layout of the mesh bind group (index 2).
    pub mesh_layout: wgpu::BindGroupLayout,
    /// The variant this pipeline was built for.
    pub options: UnlitOptions,
}

impl UnlitPipeline {
    /// Compose the built-in `unlit.wesl` for `options` and build the
    /// pipeline.
    ///
    /// `color_format` and `sample_count` must match the render target the
    /// pipeline will be used with; `depth_format` must match its depth
    /// attachment, or be `None` for a pass without depth.
    pub fn new(
        device: &wgpu::Device,
        options: UnlitOptions,
        color_format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
        sample_count: u32,
    ) -> Self {
        let wgsl = compose_builtin(options).expect("the built-in unlit shader composes");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgpu_unlit_render::unlit"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let global_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgpu_unlit_render::unlit::globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
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
                },
                wgpu::BindGroupLayoutEntry {
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
                },
                wgpu::BindGroupLayoutEntry {
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
                },
            ],
        });

        let material_layout = options.base_color_texture.then(|| {
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

        let mesh_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgpu_unlit_render::unlit::mesh"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: MESH_INFO_BINDING,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(<MeshInfo as const_shader_layout::ShaderLayout>::SIZE),
                },
                count: None,
            }],
        });

        let layouts = [
            Some(&global_layout),
            material_layout.as_ref(),
            Some(&mesh_layout),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgpu_unlit_render::unlit::layout"),
            bind_group_layouts: &layouts,
            immediate_size: 0,
        });

        let vertex_buffers = Self::vertex_buffer_layouts(options);
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgpu_unlit_render::unlit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &vertex_buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
                format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            options,
        }
    }

    /// The vertex-buffer declarations the built-in shader expects, in slot
    /// order.
    ///
    /// The strides are derived from the attribute formats, so they follow any
    /// change to the compressed vertex layout. A slot whose variant declares
    /// no attributes — the UV-and-color slot without either channel — is
    /// reported as `None`.
    pub fn vertex_buffer_layouts(
        options: UnlitOptions,
    ) -> [Option<wgpu::VertexBufferLayout<'static>>; 3] {
        const POSITION_ATTRIBUTES: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::POSITION => Snorm16x4];

        // One static slice per UV/color combination: the layout must be
        // `'static` to live on the pipeline descriptor.
        const UV_ONLY: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::UV => Snorm16x2];
        const COLOR_ONLY: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![location::COLOR => Unorm8x4];
        const UV_COLOR: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![location::UV => Snorm16x2, location::COLOR => Unorm8x4];

        let stream = options.uv_color_stream();
        let uv_color_attributes: Option<&'static [wgpu::VertexAttribute]> =
            match (stream.uv, stream.color) {
                (false, false) => None,
                (true, false) => Some(&UV_ONLY),
                (false, true) => Some(&COLOR_ONLY),
                (true, true) => Some(&UV_COLOR),
            };

        const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            location::MODEL_0 => Float32x4,
            location::MODEL_1 => Float32x4,
            location::MODEL_2 => Float32x4,
            location::BASE_COLOR => Float32x4,
        ];

        [
            options
                .vertex_position
                .then(|| vertex_layout(&POSITION_ATTRIBUTES, wgpu::VertexStepMode::Vertex)),
            uv_color_attributes
                .map(|attributes| vertex_layout(attributes, wgpu::VertexStepMode::Vertex)),
            Some(vertex_layout(
                &INSTANCE_ATTRIBUTES,
                wgpu::VertexStepMode::Instance,
            )),
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
fn compose_builtin(options: UnlitOptions) -> Result<String, ComposeError> {
    assert!(
        !options.base_color_texture || options.vertex_uv,
        "the base-color texture is sampled with the per-vertex UV, so \
         `base_color_texture` requires `vertex_uv`"
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

    /// Every `UnlitOptions` combination that composes.
    ///
    /// `base_color_texture` implies `vertex_uv`, so texture variants always
    /// carry the UV flag.
    fn all_variants() -> Vec<UnlitOptions> {
        let mut variants = Vec::new();
        for vertex_position in [false, true] {
            for vertex_uv in [false, true] {
                for vertex_color in [false, true] {
                    for base_color_texture in [false, true] {
                        if base_color_texture && !vertex_uv {
                            continue;
                        }
                        variants.push(UnlitOptions {
                            vertex_position,
                            vertex_uv,
                            base_color_texture,
                            vertex_color,
                        });
                    }
                }
            }
        }
        variants
    }

    #[test]
    fn composes_each_variant_to_valid_wgsl() {
        for options in all_variants() {
            let wgsl = compose_builtin(options)
                .unwrap_or_else(|error| panic!("variant {options:?} failed: {error}"));
            assert!(wgsl.contains("fn vs_main"), "variant {options:?}");
            assert!(wgsl.contains("fn fs_main"), "variant {options:?}");
            // Disabled features must leave no conditional attributes behind.
            assert!(!wgsl.contains("@if"), "variant {options:?} kept @if");
            // Each channel appears exactly when its flag is on.
            assert_eq!(
                wgsl.contains("base_color_tex"),
                options.base_color_texture,
                "variant {options:?}"
            );
            assert_eq!(
                wgsl.contains("decode_uv"),
                options.vertex_uv,
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
                options.vertex_position,
                "variant {options:?}"
            );
        }
    }

    #[test]
    fn base_color_texture_requires_vertex_uv() {
        let result = std::panic::catch_unwind(|| {
            compose_builtin(UnlitOptions {
                vertex_position: true,
                vertex_uv: false,
                base_color_texture: true,
                vertex_color: false,
            })
        });
        assert!(
            result.is_err(),
            "sampling a base-color texture without UVs must be rejected"
        );
    }

    #[test]
    fn vertex_strides_follow_the_attribute_formats() {
        let layouts = UnlitPipeline::vertex_buffer_layouts(UnlitOptions {
            vertex_position: true,
            vertex_uv: true,
            ..Default::default()
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

    #[test]
    fn uv_color_slot_follows_its_channels() {
        let stride = |options: UnlitOptions| {
            UnlitPipeline::vertex_buffer_layouts(options)[UV_COLOR_SLOT as usize]
                .as_ref()
                .map(|layout| layout.array_stride)
        };

        let uv = wgpu::VertexFormat::Snorm16x2.size();
        let color = wgpu::VertexFormat::Unorm8x4.size();

        // No channel: the slot disappears entirely, so a variant never binds
        // a buffer the shader does not declare.
        assert_eq!(
            stride(UnlitOptions {
                vertex_position: true,
                vertex_uv: false,
                base_color_texture: false,
                vertex_color: false,
            }),
            None
        );
        assert_eq!(
            stride(UnlitOptions {
                vertex_position: true,
                vertex_uv: true,
                base_color_texture: false,
                vertex_color: false,
            }),
            Some(uv)
        );
        assert_eq!(
            stride(UnlitOptions {
                vertex_position: true,
                vertex_uv: false,
                base_color_texture: false,
                vertex_color: true,
            }),
            Some(color)
        );
        assert_eq!(
            stride(UnlitOptions {
                vertex_position: true,
                vertex_uv: true,
                base_color_texture: false,
                vertex_color: true,
            }),
            Some(uv + color)
        );
    }

    /// The position slot follows its flag, so a position-less variant binds no
    /// position buffer at all.
    #[test]
    fn position_slot_follows_its_flag() {
        let position = |options: UnlitOptions| {
            UnlitPipeline::vertex_buffer_layouts(options)[POSITION_SLOT as usize]
                .as_ref()
                .map(|layout| (layout.array_stride, layout.step_mode))
        };

        assert_eq!(
            position(UnlitOptions::default()),
            None,
            "no position stream means no position slot"
        );
        assert_eq!(
            position(UnlitOptions {
                vertex_position: true,
                ..Default::default()
            }),
            Some((
                wgpu::VertexFormat::Snorm16x4.size(),
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
                uv: true,
                color: true,
            },
            MeshUvColorStream {
                uv: true,
                color: false,
            },
            MeshUvColorStream {
                uv: false,
                color: true,
            },
        ] {
            let mut metadata = crate::mesh::MeshMetadata::default();
            let from_raw = write_stream(stream, &uvs, &colors, 3, &mut metadata);

            // Compress separately, exactly as a caller sharing one stream
            // across meshes would.
            let packed_uvs: Vec<_> = if stream.uv {
                crate::mesh::compress_uvs(&uvs, &mut metadata).collect()
            } else {
                Vec::new()
            };
            let packed_colors: Vec<_> = if stream.color {
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
                uv: true,
                color: true,
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
                uv: true,
                color: false,
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
                uv: false,
                color: true,
            },
            &[],
            &colors,
            2,
            &mut crate::mesh::MeshMetadata::default(),
        );
        assert_eq!(out.len(), 2 * 4);
        assert_eq!(&out[0..4], &[0, 64, 128, 255]);
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

        let layouts = UnlitPipeline::vertex_buffer_layouts(UnlitOptions::default());
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
