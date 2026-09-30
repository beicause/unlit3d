//! The built-in unlit pipeline and the WESL composition behind it.
//!
//! [`SpecializedUnlitPipeline`] composes the built-in `unlit.wesl` with a variant
//! selected by [`UnlitOptions`], builds the layouts the shader expects, and
//! exposes the vertex-buffer layouts the caller declares when creating meshes.
//! Each variant contains exactly the bindings and attributes the selected
//! channels need, so nothing unused reaches the GPU.

#[cfg(feature = "unlit")]
use crate::mesh::{
    ChannelEncoding, MeshVertexStreamWriter, PositionStreamChannels, PositionStreamWriter,
    UvColorFlags,
};
#[cfg(feature = "unlit")]
use crate::render_attachments::default_depth_stencil_format;
#[cfg(feature = "unlit")]
use crate::specialize::{PipelineDescriptor, SurfaceKey, SurfaceTarget, VertexBufferLayoutDesc};

/// Binding slot of the camera uniform in the global bind group.
pub const CAMERA_BINDING: u32 = 0;
/// Binding slot of the frame-globals uniform in the global bind group.
pub const FRAME_BINDING: u32 = 1;
/// Binding slot of the mesh-metadata storage buffer in the global bind group.
pub const MESH_METADATA_BINDING: u32 = 2;
/// Binding slot of the frame's joint matrices in the global bind group.
///
/// The array holds the pose of every visible instance, so it is per-frame data
/// the source packs rather than anything a mesh owns; an instance's own slice
/// starts at [`crate::mesh::MeshInstance::pose`]'s `x`.
pub const JOINTS_BINDING: u32 = 3;
/// Binding slot of the frame's morph weights in the global bind group.
///
/// Like the joint matrices this is one array for the whole frame, and an
/// instance's own slice starts at [`crate::mesh::MeshInstance::pose`]'s `y`.
pub const MORPH_WEIGHTS_BINDING: u32 = 4;
/// Binding slot of the frame's morph position displacements in the global bind
/// group.
///
/// A mesh's displacements are its own geometry, but one array holds them for
/// the whole frame and the mesh names its slice through the `morph_deltas_offset`
/// of its [`MeshMetadata`](crate::mesh::MeshMetadata) entry. Pooling them here
/// rather than in a per-mesh buffer is what lets a draw bind no mesh group.
pub const MORPH_DELTAS_BINDING: u32 = 5;
/// Bind-group index of the global group.
pub const GLOBAL_GROUP: u32 = 0;
/// Bind-group index of the material group.
pub const MATERIAL_GROUP: u32 = 1;
/// Binding slot of the base-color texture in the material group.
pub const BASE_COLOR_TEXTURE_BINDING: u32 = 0;
/// Binding slot of the base-color sampler in the material group.
pub const BASE_COLOR_SAMPLER_BINDING: u32 = 1;
/// Bind-group index of the mesh group.
///
/// The built-in unlit variants bind nothing here: every input they read is in
/// the global group or on the instance stream. The index stays a general
/// extension point, so a caller's own pipeline can bind per-mesh data — a tint,
/// a transform palette, a material parameter — and
/// [`crate::scene::DrawEntry`] will bind it.
pub const MESH_GROUP: u32 = 2;

/// Vertex-buffer slot carrying compressed positions.
pub const POSITION_SLOT: u32 = 0;
/// Vertex-buffer slot carrying UVs and vertex colors.
pub const UV_COLOR_SLOT: u32 = 1;
/// Vertex-buffer slot carrying per-instance data.
pub const INSTANCE_SLOT: u32 = 2;

/// Vertex attribute locations declared by the built-in shader.
pub mod location {
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
    /// Joint indices (`Uint16x4`), read from the position stream.
    pub const JOINTS: u32 = 7;
    /// Joint weights (`Unorm16x4`), read from the position stream right after
    /// the indices.
    pub const JOINTS_WEIGHTS: u32 = 8;
    /// Per-instance pose base (`Uint32x2`), read from the instance stream.
    ///
    /// `x` is the instance's first joint matrix and `y` its first morph weight
    /// in the frame's shared pose arrays.
    pub const POSE: u32 = 9;
    /// Per-instance index of the mesh metadata entry (`Uint32`), read from the
    /// instance stream.
    pub const METADATA_INDEX: u32 = 10;
}

/// Entry point name of the built-in shader's vertex stage.
#[cfg(feature = "unlit")]
pub const VS_MAIN: &str = "vs_main";
/// Entry point name of the built-in shader's fragment stage.
#[cfg(feature = "unlit")]
pub const FS_MAIN: &str = "fs_main";

#[cfg(feature = "unlit")]
bitflags::bitflags! {
    /// The channels and bindings the built-in shader variant reads.
    ///
    /// Each flag adds both a shader code path and the matching vertex
    /// attribute or binding, so a variant contains exactly what it uses. The
    /// flags are independent except where noted.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct UnlitFlags: u16 {
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
        /// Read per-vertex joint indices and weights from the position stream
        /// and deform the vertex by the joint matrices the mesh group binds.
        ///
        /// The joint pair is part of [`Self::VERTEX_POSITION`]'s stream, so
        /// this requires it; the deformation happens before the instance
        /// transform, in the mesh's own space.
        const VERTEX_JOINTS = 1 << 8;
        /// Read a per-vertex morph position offset and add it to the
        /// deformed position.
        ///
        /// Every morph target's delta for one vertex is read and weighted by
        /// the mesh's morph weights, so a target with a zero weight costs
        /// nothing but a multiply. Requires [`Self::VERTEX_POSITION`].
        const MORPH_POSITIONS = 1 << 9;
        /// Read the arrays that would otherwise be `var<storage, read>` from
        /// 2D textures instead.
        ///
        /// A device without storage buffers — WebGL2 has none, and rejects a
        /// bind-group layout naming one — can still read a flat array through
        /// `textureLoad`, so the same variant is available there with its
        /// arrays in textures. The arrays keep their binding numbers and their
        /// byte layout; only the resource type and the read change.
        ///
        /// This is a property of the device rather than of a draw, so it is
        /// deliberately outside [`Self::MESH_MASK`]: specializing a draw by its
        /// vertex layout must not clear it.
        const TEXEL_ARRAY = 1 << 10;
        /// The base-color texture stores *luminance*: one channel, sampled
        /// into every color channel.
        ///
        /// Required by neither of the other material flags, but only
        /// meaningful together with [`Self::BASE_COLOR_TEXTURE`] — without a
        /// texture there is nothing to expand. The sampled value is expanded
        /// to `vec3(l)` and decoded from sRGB on the way, because a luminance
        /// image cannot be uploaded in an sRGB format: WebGPU offers the
        /// transfer function only for four-channel `Rgba8UnormSrgb` and
        /// `Bgra8UnormSrgb`, so a one-channel texture has to carry the encoded
        /// value and let the shader decode it.
        const BASE_COLOR_LUMINANCE = 1 << 11;
        /// The base-color texture stores *luminance and alpha*: two channels
        /// holding `l` and `a`, sampled into the color and the alpha channel.
        ///
        /// Mutually exclusive with [`Self::BASE_COLOR_LUMINANCE`], and like
        /// it only meaningful together with [`Self::BASE_COLOR_TEXTURE`]. The
        /// luminance half decodes from sRGB exactly as
        /// [`Self::BASE_COLOR_LUMINANCE`] does; alpha is coverage, so it
        /// never converts.
        const BASE_COLOR_LUMINANCE_ALPHA = 1 << 12;
    }
}

#[cfg(feature = "unlit")]
impl UnlitFlags {
    /// The flags that describe the *device* rather than the variant.
    ///
    /// A caller building a variant of its own replaces every other flag, but
    /// these have to survive: they say how the device can be read from, and
    /// clearing one asks a device for something it may reject outright. Use
    /// [`UnlitOptions::with_flags`] to assign flags without losing them.
    pub const DEVICE_MASK: UnlitFlags = UnlitFlags::TEXEL_ARRAY;

    /// The flags the built-in pipeline derives from a mesh's vertex layout.
    ///
    /// Exactly the bits in this mask are replaced when a mesh is
    /// specialized; the material and target flags
    /// [`Self::BASE_COLOR_TEXTURE`] and
    /// [`Self::SRGB_TO_LINEAR_OUTPUT`] are the base descriptor's and
    /// must survive, and so is [`Self::MORPH_POSITIONS`] — a morph target's
    /// displacements are storage data rather than a vertex attribute, so no
    /// layout can imply them.
    pub const MESH_MASK: UnlitFlags = UnlitFlags::VERTEX_POSITION
        .union(UnlitFlags::UNCOMPRESSED_POSITION)
        .union(UnlitFlags::VERTEX_UV)
        .union(UnlitFlags::UNCOMPRESSED_UV)
        .union(UnlitFlags::VERTEX_COLOR)
        .union(UnlitFlags::VERTEX_INSTANCE)
        .union(UnlitFlags::VERTEX_JOINTS);

    /// The [`Self::MESH_MASK`] flags one vertex-buffer slot implies.
    ///
    /// A channel is identified by its shader location, which is stable
    /// across variants, and the instance bit by the slot's step mode. Only
    /// the built-in pipeline's slots are read; any other slot contributes
    /// nothing.
    pub fn for_vertex_buffer(
        slot: u32,
        layout: &crate::specialize::VertexBufferLayoutDesc,
    ) -> UnlitFlags {
        let mut flags = UnlitFlags::empty();
        match slot {
            POSITION_SLOT => {
                for attribute in &layout.attributes {
                    if attribute.shader_location == location::POSITION {
                        flags |= UnlitFlags::VERTEX_POSITION;
                        if attribute.format == wgpu::VertexFormat::Float32x3 {
                            flags |= UnlitFlags::UNCOMPRESSED_POSITION;
                        }
                    }
                    // The joint pair is part of this stream rather than a
                    // stream of its own: the indices are the flag's evidence,
                    // and the weights follow them.
                    if attribute.shader_location == location::JOINTS {
                        flags |= UnlitFlags::VERTEX_JOINTS;
                    }
                }
            }
            UV_COLOR_SLOT => {
                for attribute in &layout.attributes {
                    match attribute.shader_location {
                        location::UV => {
                            flags |= UnlitFlags::VERTEX_UV;
                            if attribute.format == wgpu::VertexFormat::Float32x2 {
                                flags |= UnlitFlags::UNCOMPRESSED_UV;
                            }
                        }
                        location::COLOR => flags |= UnlitFlags::VERTEX_COLOR,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if layout.step_mode == wgpu::VertexStepMode::Instance {
            flags |= UnlitFlags::VERTEX_INSTANCE;
        }
        flags
    }
}

/// The variant of the built-in unlit shader to compose.
///
/// [`UnlitOptions::standard`] is the usual starting point: compressed
/// positions, per-instance transforms and blending off.
#[cfg(feature = "unlit")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnlitOptions {
    /// The channels and bindings the variant reads.
    pub flags: UnlitFlags,
    /// Whether the base-color texture binding is filterable.
    ///
    /// A filtering binding accepts a sampler that interpolates between
    /// texels and a texture whose format is filterable — the usual case, and
    /// what [`Self::standard`] leaves. Clearing it declares the binding
    /// unfilterable instead, which is what a texture sampled with
    /// `nearest`-only filters has to bind to on a device that cannot filter
    /// that format: `Rgba32Float`, say, is filterable only where
    /// `FLOAT32_FILTERABLE` is present, and is still bindable without it
    /// through an unfilterable binding.
    ///
    /// The two declare different bind-group layouts, so this is part of what
    /// distinguishes one variant from another: a pipeline built for one cannot
    /// bind a material built for the other.
    pub texture_filtering: bool,
    /// How the pipeline assembles and culls primitives, including the strip
    /// index format a strip topology requires.
    ///
    /// [`Self::standard`] leaves `strip_index_format` at its default, and the
    /// built-in unlit family fills it from the mesh's index buffer, so the
    /// field is a caller's concern only for a pipeline a mesh does not
    /// describe.
    pub primitive: wgpu::PrimitiveState,
    /// The pipeline's depth-stencil state, or `None` for a pass with no depth
    /// attachment.
    ///
    /// Its format is the device's default depth-stencil format
    /// ([`crate::render_attachments::default_depth_stencil_format`]) when built with
    /// [`Self::standard`]. [`Self::standard`] is the renderer's reverse-z
    /// convention: depth is cleared to the far plane, so nearer geometry
    /// carries the greater value.
    ///
    /// `wgpu` requires this to agree with the render pass: a pass with a depth
    /// attachment needs a state naming that attachment's format, and a pass
    /// without one needs `None`. [`SurfaceSpecializer`] therefore follows the
    /// target rather than leaving a stale format behind, so a pipeline always
    /// matches the pass it is recorded into.
    ///
    /// [`SurfaceSpecializer`]: crate::specialize::SurfaceSpecializer
    pub depth_stencil: Option<wgpu::DepthStencilState>,
    /// The color target the pipeline writes: its format, blend state and
    /// write mask.
    ///
    /// `blend` of `None` writes the fragment output unblended.
    pub color_target: wgpu::ColorTargetState,
    /// The pipeline multisampling state: the sample count it renders with,
    /// plus the sample mask and alpha-to-coverage settings.
    ///
    /// A count of `1` disables multisampling. The state must match the
    /// render pass attachments.
    pub multisample: wgpu::MultisampleState,
}

#[cfg(feature = "unlit")]
impl UnlitOptions {
    /// The usual starting point: the full compressed mesh variant — positions,
    /// UVs, vertex colors, a base-color texture and per-instance transforms —
    /// with the device's default depth-stencil format.
    ///
    /// The depth-stencil format is [`default_depth_stencil_format`]'s choice
    /// for `device`, so the pipeline's depth-stencil state matches the render
    /// target's depth attachment on that device.
    ///
    /// Back faces are culled: the geometry this variant is for is closed
    /// meshes wound counter-clockwise, whose insides are never meant to show.
    ///
    /// Every variant must read at least one vertex attribute: one reading
    /// nothing composes a `VertexInput` struct with no members, which is not
    /// valid WGSL.
    pub fn standard(device: &wgpu::Device) -> Self {
        let mut options = Self::standard_shape();
        if let Some(depth_stencil) = &mut options.depth_stencil {
            depth_stencil.format = default_depth_stencil_format(device);
        }
        options
            .flags
            .set(UnlitFlags::TEXEL_ARRAY, !supports_storage_buffers(device));
        options
    }

    /// Replace this variant's flags, keeping the ones the *device* decides.
    ///
    /// Most flags describe the variant — the channels it reads, its material,
    /// its output — and a caller building a different one wants to say so
    /// outright. [`UnlitFlags::TEXEL_ARRAY`] is not like the rest: it says how
    /// the device can be read from, not what the variant is, and a caller that
    /// cleared it would ask a storage-less device for a binding it rejects.
    /// Assigning `flags` directly is therefore easy to get wrong, and this is
    /// the setter that cannot.
    #[must_use]
    pub fn with_flags(mut self, flags: UnlitFlags) -> Self {
        self.flags = (self.flags & UnlitFlags::DEVICE_MASK) | (flags & !UnlitFlags::DEVICE_MASK);
        self
    }

    /// The standard variant's device-independent fields: the flags, primitive
    /// state, color target and multisample state, with a placeholder
    /// depth-stencil format that [`Self::standard`] replaces with the device's
    /// default.
    ///
    /// Available crate-wide so tests and builders that have no device can
    /// construct the standard configuration without one.
    pub(crate) fn standard_shape() -> Self {
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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            color_target: wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            },
            multisample: wgpu::MultisampleState {
                count: 4,
                ..Default::default()
            },
            texture_filtering: true,
        }
    }

    /// Which of [`Self::flags`] are set, as WESL `@if` names paired with their
    /// state.
    ///
    /// Every name appears, so the composed variant never sees a name it does
    /// not know.
    pub fn features(&self) -> [(&'static str, bool); 13] {
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
            (
                "VERTEX_JOINTS",
                self.flags.contains(UnlitFlags::VERTEX_JOINTS),
            ),
            (
                "MORPH_POSITIONS",
                self.flags.contains(UnlitFlags::MORPH_POSITIONS),
            ),
            ("TEXEL_ARRAY", self.flags.contains(UnlitFlags::TEXEL_ARRAY)),
            (
                "BASE_COLOR_LUMINANCE",
                self.flags.contains(UnlitFlags::BASE_COLOR_LUMINANCE),
            ),
            (
                "BASE_COLOR_LUMINANCE_ALPHA",
                self.flags.contains(UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA),
            ),
        ]
    }

    /// Whether this variant deforms its vertices by joint matrices and so
    /// reads the global group's array of them.
    pub fn needs_joints(&self) -> bool {
        self.flags.contains(UnlitFlags::VERTEX_JOINTS)
    }

    /// Whether this variant reads the arrays through
    /// [`UnlitFlags::TEXEL_ARRAY`] instead of storage buffers.
    ///
    /// A device with no storage buffers cannot bind one at all, so this is what
    /// decides whether the four array bindings are storage buffers or textures.
    /// [`UnlitOptions::standard`] sets it from the device; a caller that builds
    /// its options by hand may set it either way, and only the combination that
    /// asks for a storage buffer on a device without them is rejected.
    pub fn uses_texel_arrays(&self) -> bool {
        self.flags.contains(UnlitFlags::TEXEL_ARRAY)
    }

    /// Reject options whose array path the device cannot serve.
    ///
    /// The texel path works on every device — a texture read is universal — so
    /// forcing it on a device that also has storage buffers is allowed, which
    /// is what lets a test exercise it anywhere. The reverse is not: a
    /// bind-group layout naming a storage buffer is rejected outright by a
    /// device without them, so the failure is caught here with the reason
    /// rather than left to wgpu's validation.
    ///
    /// A variant that declares no array binding at all — the UI's, which reads
    /// only full-precision vertex attributes — has nothing to serve either way,
    /// so it is not asked to pick a path.
    ///
    /// # Panics
    ///
    /// If the variant reads storage buffers on a device that has none.
    fn assert_device_supports_arrays(&self, device: &wgpu::Device) {
        let reads_an_array = self.needs_metadata() || self.needs_joints();
        assert!(
            !reads_an_array || self.uses_texel_arrays() || supports_storage_buffers(device),
            "this variant reads its arrays from storage buffers, but the device has none \
             (its `max_storage_buffers_per_shader_stage` is 0, as WebGL2's is): set \
             `UnlitFlags::TEXEL_ARRAY`, or start from `UnlitOptions::standard`, which does it \
             from the device"
        );
    }

    /// The layout entry binding an array of `element_size`-byte elements.
    ///
    /// On the storage path it is a read-only storage buffer whose binding
    /// minimum is one element, so the array may grow without invalidating the
    /// layout. On the texel path it is a non-filterable float texture, which is
    /// what `textureLoad` reads.
    fn array_entry(&self, binding: u32, element_size: u64) -> wgpu::BindGroupLayoutEntry {
        if self.uses_texel_arrays() {
            return crate::texel_array::TexelArrayLayout::binding(
                binding,
                wgpu::ShaderStages::VERTEX,
            );
        }
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: Some(
                    core::num::NonZeroU64::new(element_size)
                        .expect("an element has a non-zero size, which is a valid binding minimum"),
                ),
            },
            count: None,
        }
    }

    /// Whether this variant reads the global group's morph arrays: the
    /// per-instance weights and the per-mesh displacements.
    pub fn needs_morphs(&self) -> bool {
        self.flags.contains(UnlitFlags::MORPH_POSITIONS)
    }

    /// Whether this variant reads the global group's mesh-metadata array: the
    /// per-mesh decode parameters and the addressing a morph reads.
    ///
    /// Mirrors the shader's metadata condition, so the layout and the composed
    /// variant agree on whether the binding exists. A morph needs the array
    /// even when every channel is uncompressed, because the struct also carries
    /// the vertex offset and morph count its displacements are addressed by.
    pub fn needs_metadata(&self) -> bool {
        let compressed_position = self.flags.contains(UnlitFlags::VERTEX_POSITION)
            && !self.flags.contains(UnlitFlags::UNCOMPRESSED_POSITION);
        let compressed_uv = self.flags.contains(UnlitFlags::VERTEX_UV)
            && !self.flags.contains(UnlitFlags::UNCOMPRESSED_UV);
        compressed_position || compressed_uv || self.needs_morphs()
    }

    /// The UV-and-color vertex stream this variant expects.
    ///
    /// The stream is described in [`crate::mesh`]'s own terms: this variant's
    /// channels are translated into the ones that make up a vertex stream, so
    /// packing meshes for this pipeline needs nothing specific to it.
    pub fn uv_color_stream(&self) -> MeshVertexStreamWriter {
        MeshVertexStreamWriter {
            flags: uv_color_flags(self.flags),
        }
    }

    /// The position vertex stream this variant expects.
    ///
    /// The joint indices and weights the variant deforms with belong to this
    /// stream rather than one of their own, so a skinned variant's stream is
    /// wider by the joint pair and nothing else changes.
    pub fn position_stream(&self) -> PositionStreamWriter {
        PositionStreamWriter {
            channels: PositionStreamChannels {
                position: self.flags.contains(UnlitFlags::VERTEX_POSITION).then(|| {
                    if self.flags.contains(UnlitFlags::UNCOMPRESSED_POSITION) {
                        ChannelEncoding::UncompressedPosition
                    } else {
                        ChannelEncoding::CompressedPosition
                    }
                }),
                joints: self.flags.contains(UnlitFlags::VERTEX_JOINTS),
            },
        }
    }
}

/// The UV-and-color stream channels `flags` imply.
///
/// A variant often carries channels belonging to other streams; this
/// translation selects the ones that belong to the UV-and-color stream.
#[cfg(feature = "unlit")]
fn uv_color_flags(flags: UnlitFlags) -> UvColorFlags {
    let mut stream = UvColorFlags::empty();
    stream.set(UvColorFlags::UV, flags.contains(UnlitFlags::VERTEX_UV));
    stream.set(
        UvColorFlags::UNCOMPRESSED_UV,
        flags.contains(UnlitFlags::UNCOMPRESSED_UV),
    );
    stream.set(
        UvColorFlags::COLOR,
        flags.contains(UnlitFlags::VERTEX_COLOR),
    );
    stream
}

/// Failure reasons reported by [`compose_builtin`].
#[cfg(feature = "unlit")]
#[derive(Debug)]
enum ComposeError {
    /// The WESL compiler rejected the module.
    Compile(wesl::Error),
}

#[cfg(feature = "unlit")]
impl core::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "WESL compilation failed: {error}"),
        }
    }
}

#[cfg(feature = "unlit")]
impl std::error::Error for ComposeError {}

/// The bind-group layouts an [`SpecializedUnlitPipeline`] variant declares.
///
/// Built by [`UnlitOptions::bind_group_layouts`] from the same options a
/// pipeline is built from, so the two agree: the global group always exists and
/// the material group only for a variant that samples a base-color texture. The
/// mesh group is always `None` — the built-in variants bind nothing there — but
/// stays in the structure because the index is a general extension point.
#[cfg(feature = "unlit")]
#[derive(Clone, Debug)]
pub struct UnlitBindGroupLayouts {
    /// Layout of the global bind group (index 0).
    pub global: wgpu::BindGroupLayout,
    /// Layout of the material bind group (index 1), for a variant that
    /// samples a base-color texture.
    pub material: Option<wgpu::BindGroupLayout>,
    /// Layout of the mesh bind group (index 2).
    ///
    /// Always `None` for the built-in variants: every input they read is in the
    /// global group or on the instance stream.
    pub mesh: Option<wgpu::BindGroupLayout>,
}

/// The built-in unlit pipeline: a compiled [wgpu::RenderPipeline] together
/// with the [UnlitOptions] it was built from.
///
/// A variant of [UnlitOptions] is a [PipelineDescriptor], so the built-in
/// shader compiles through the same cache a caller's own descriptor does, and
/// the layouts a variant declares are derived from those options rather than
/// stored beside the pipeline.
#[cfg(feature = "unlit")]
pub type SpecializedUnlitPipeline =
    crate::specialize::SpecializedPipeline<wgpu::RenderPipeline, UnlitOptions>;

/// Whether `device` can read a storage buffer from a shader.
///
/// WebGL2 cannot: it has no storage buffers at all, and a bind-group layout
/// naming one is rejected outright. Such a device reports a limit of zero,
/// which is the only signal available — [`wgpu::Device`] exposes its limits and
/// features but not the downlevel capabilities that would say so directly.
///
/// A device that can bind storage buffers reports a non-zero limit, so this is
/// what decides between the built-in shader's buffer and texel array paths.
#[cfg(feature = "unlit")]
pub fn supports_storage_buffers(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    limits.max_storage_buffers_per_shader_stage > 0 && limits.max_storage_buffer_binding_size > 0
}

#[cfg(feature = "unlit")]
impl UnlitOptions {
    /// Build the bind-group layouts the built-in shader's `options` variant
    /// declares, without compiling the pipeline.
    ///
    /// Splitting the layouts out of the descriptor's [PipelineDescriptor::create]
    /// lets a caller describe a pipeline's binding interface without compiling
    /// anything, and keeps the two in step: `create` builds the pipeline from
    /// this same result.
    pub fn bind_group_layouts(&self, device: &wgpu::Device) -> UnlitBindGroupLayouts {
        self.assert_device_supports_arrays(device);
        let global = {
            // The group holds the frame's shared inputs: the camera, the frame
            // globals, the mesh-metadata decode parameters, the pose arrays
            // every instance slices into, and the morph displacements every
            // mesh slices into. A variant that reads none of the optional ones
            // declares no binding for them.
            let mut entries = arrayvec::ArrayVec::<wgpu::BindGroupLayoutEntry, 6>::new();
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
            if self.needs_metadata() {
                entries.push(self.array_entry(
                    MESH_METADATA_BINDING,
                    <crate::mesh::MeshMetadata as const_shader_layout::ShaderLayout>::SIZE.get(),
                ));
            }
            if self.needs_joints() {
                entries.push(self.array_entry(
                    JOINTS_BINDING,
                    <crate::mesh::JointMatrix as const_shader_layout::ShaderLayout>::SIZE.get(),
                ));
            }
            if self.needs_morphs() {
                entries.push(self.array_entry(
                    MORPH_WEIGHTS_BINDING,
                    wgpu::BufferAddress::from(size_of::<f32>() as u64),
                ));
                // One position component of one target: like every other array
                // entry the storage binding minimum is a single element, so a
                // mesh joining or leaving the pool never invalidates the
                // layout.
                entries.push(self.array_entry(
                    MORPH_DELTAS_BINDING,
                    wgpu::BufferAddress::from(size_of::<f32>() as u64),
                ));
            }
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("unlit_wgpu::unlit::globals"),
                entries: &entries,
            })
        };

        let material = self
            .flags
            .contains(UnlitFlags::BASE_COLOR_TEXTURE)
            .then(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("unlit_wgpu::unlit::material"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: BASE_COLOR_TEXTURE_BINDING,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float {
                                    filterable: self.texture_filtering,
                                },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: BASE_COLOR_SAMPLER_BINDING,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: if self.texture_filtering {
                                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
                            } else {
                                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering)
                            },
                            count: None,
                        },
                    ],
                })
            });

        // The built-in variants bind nothing at the mesh group: every input
        // they read is in the global group or on the instance stream. The
        // index remains a general extension point a caller's pipeline can use.
        let mesh = None;

        UnlitBindGroupLayouts {
            global,
            material,
            mesh,
        }
    }

    /// The vertex-buffer declarations the built-in shader expects, in slot
    /// order.
    ///
    /// A slot whose variant declares no attributes — the UV-and-color slot
    /// without either channel, or the instance slot without
    /// [`UnlitFlags::VERTEX_INSTANCE`] — is reported as `None`, so a variant
    /// never binds a buffer the shader does not declare.
    pub fn vertex_buffer_layouts(&self) -> [Option<VertexBufferLayoutDesc>; 3] {
        let flags = self.flags;

        const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            location::MODEL_0 => Float32x4,
            location::MODEL_1 => Float32x4,
            location::MODEL_2 => Float32x4,
            location::BASE_COLOR => Unorm8x4,
            location::POSE => Uint32x2,
            location::METADATA_INDEX => Uint32,
        ];

        // Each stream describes its own layout, so the attributes a pipeline
        // declares and the bytes a packed mesh writes come from one
        // description and cannot drift apart.
        let position = self.position_stream().channels.channels();
        let uv_color = self.uv_color_stream().channels();
        [
            (!position.is_empty()).then(|| position.layout()),
            (!uv_color.is_empty()).then(|| uv_color.layout()),
            flags
                .contains(UnlitFlags::VERTEX_INSTANCE)
                .then(|| VertexBufferLayoutDesc {
                    // The record is tightly packed, so its stride is the sum of
                    // the attribute formats: the flat `[f32; 4]` columns and the
                    // `Uint32x2` pose base carry no alignment padding, and the
                    // `Unorm8x4` color fills its four bytes exactly.
                    array_stride: INSTANCE_ATTRIBUTES
                        .iter()
                        .map(|attribute| attribute.format.size())
                        .sum(),
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: INSTANCE_ATTRIBUTES.into_iter().collect(),
                }),
        ]
    }
}

#[cfg(feature = "unlit")]
impl PipelineDescriptor<wgpu::RenderPipeline> for UnlitOptions {
    /// Compose the built-in `unlit.wesl` for these options and build the
    /// pipeline, so a variant of them compiles through the same
    /// [PipelineDescriptor] mechanism a caller's own descriptor does.
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        let wgsl = compose_builtin(self).expect("the built-in unlit shader composes");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("unlit_wgpu::unlit"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let layouts = self.bind_group_layouts(device);
        let layout_refs = [
            Some(&layouts.global),
            layouts.material.as_ref(),
            layouts.mesh.as_ref(),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unlit_wgpu::unlit::layout"),
            bind_group_layouts: &layout_refs,
            immediate_size: 0,
        });

        let vertex_buffers = Self::vertex_buffer_layouts(self);
        let vertex_buffers: Vec<Option<wgpu::VertexBufferLayout<'_>>> = vertex_buffers
            .iter()
            .map(|layout| layout.as_ref().map(VertexBufferLayoutDesc::as_wgpu))
            .collect();
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unlit_wgpu::unlit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(VS_MAIN),
                compilation_options: Default::default(),
                buffers: &vertex_buffers,
            },
            // The primitive state — including `strip_index_format` for strip
            // topologies — is the caller's, so it is used as given.
            primitive: self.primitive,
            // A pass with a depth attachment requires every pipeline it uses
            // to declare a matching state; `standard` sets this to the
            // device's default depth-stencil format, and `SurfaceSpecializer`
            // follows the target so the two cannot disagree.
            depth_stencil: self.depth_stencil.clone(),
            multisample: self.multisample,
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(FS_MAIN),
                compilation_options: Default::default(),
                targets: &[Some(self.color_target.clone())],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

#[cfg(feature = "unlit")]
impl SurfaceTarget for UnlitOptions {
    /// Rewrites exactly the three fields the target reaches: the color format,
    /// the sample count and the depth-stencil format. The shader variant,
    /// primitive state, blend state and write mask stay the caller's.
    ///
    /// The [`UnlitFlags::SRGB_TO_LINEAR_OUTPUT`] flag is deliberately left
    /// alone: it describes the fragment's input encoding, not the target
    /// format.
    ///
    /// The depth-stencil state follows the target. `wgpu` compares it against
    /// the pass's attachment format when a pipeline is bound and rejects a
    /// mismatch, so a target without a depth attachment must yield a pipeline
    /// without a depth state rather than one still naming the base options'
    /// format. A target that has one keeps the base state — the reverse-z
    /// comparison, the write mask — and only takes the attachment's format.
    fn set_surface(&mut self, surface: SurfaceKey) {
        self.color_target.format = surface.color_format;
        match surface.depth_stencil_format {
            Some(format) => {
                self.depth_stencil
                    .get_or_insert_with(default_depth_stencil_state)
                    .format = format;
            }
            None => self.depth_stencil = None,
        }
        self.multisample.count = surface.sample_count;
    }
}

/// The depth-stencil state a target with a depth attachment starts from: the
/// reverse-z convention, with the format filled in from the attachment.
#[cfg(feature = "unlit")]
fn default_depth_stencil_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Greater),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Compose the built-in `unlit.wesl` for `options`.
///
/// # Panics
/// If the flags contradict each other: a channel cannot be uncompressed
/// without being read, the base-color texture is sampled with the per-vertex
/// UV, and only one luminance layout can expand a sampled texel.
#[cfg(feature = "unlit")]
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
    assert!(
        !flags.contains(UnlitFlags::VERTEX_JOINTS) || flags.contains(UnlitFlags::VERTEX_POSITION),
        "the joint stream is part of the position stream, so \
         `VERTEX_JOINTS` requires `VERTEX_POSITION`"
    );
    assert!(
        !flags.contains(UnlitFlags::MORPH_POSITIONS) || flags.contains(UnlitFlags::VERTEX_POSITION),
        "a morph target displaces the position, so `MORPH_POSITIONS` \
         requires `VERTEX_POSITION`"
    );
    assert!(
        !(flags.contains(UnlitFlags::VERTEX_JOINTS) || flags.contains(UnlitFlags::MORPH_POSITIONS))
            || flags.contains(UnlitFlags::VERTEX_INSTANCE),
        "the joints and morph weights a draw deforms by are per-instance, so \
         `VERTEX_JOINTS` and `MORPH_POSITIONS` require `VERTEX_INSTANCE`: the \
         instance stream is what carries the pose base the draw reads"
    );
    assert!(
        !options.needs_metadata() || flags.contains(UnlitFlags::VERTEX_INSTANCE),
        "the metadata index a draw decodes through is per-instance, so a \
         variant reading the mesh metadata requires `VERTEX_INSTANCE`: the \
         instance stream is what carries the index the draw loads it by"
    );
    assert!(
        !flags.contains(UnlitFlags::BASE_COLOR_LUMINANCE)
            && !flags.contains(UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA)
            || flags.contains(UnlitFlags::BASE_COLOR_TEXTURE),
        "the luminance flags describe how a sampled base-color texel \
         expands, so `BASE_COLOR_LUMINANCE` and \
         `BASE_COLOR_LUMINANCE_ALPHA` require `BASE_COLOR_TEXTURE`"
    );
    assert!(
        !(flags.contains(UnlitFlags::BASE_COLOR_LUMINANCE)
            && flags.contains(UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA)),
        "a base-color texel has one channel or two, so \
         `BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` are \
         mutually exclusive"
    );

    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("unlit_wgpu".to_owned()),
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

#[cfg(all(test, feature = "unlit"))]
mod tests {
    use super::*;
    use crate::specialize::{Specializer as _, SurfaceSpecializer, Variants};

    /// A caller composes their own entry shader against the built-in package
    /// with `wesl` directly: [`crate::shader`] is the `StaticPackage` that
    /// resolves `import unlit_wgpu::…`.
    #[test]
    fn a_caller_composes_against_the_built_in_package() {
        let source = "\
import unlit_wgpu::mesh_compression;
import unlit_wgpu::mesh_metadata::MeshMetadata;
import unlit_wgpu::view::View;

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
        // `import unlit_wgpu::mesh_compression;` resolves.
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
    /// `BASE_COLOR_TEXTURE` implies `VERTEX_UV`, an uncompressed channel
    /// implies its channel, and the joint and morph channels imply the position
    /// they deform, so the invalid combinations are skipped. Each surviving
    /// combination is then enumerated once per luminance layout, because those
    /// describe the sampled texel rather than the vertex layout and so are
    /// orthogonal to everything above — but only where a texel is sampled at
    /// all.
    fn all_variants() -> Vec<UnlitOptions> {
        let mut variants = Vec::new();
        for texel_array in [false, true] {
            for vertex_position in [false, true] {
                for uncompressed_position in [false, true] {
                    for vertex_uv in [false, true] {
                        for uncompressed_uv in [false, true] {
                            for vertex_color in [false, true] {
                                for vertex_instance in [false, true] {
                                    for base_color_texture in [false, true] {
                                        for vertex_joints in [false, true] {
                                            for morph_positions in [false, true] {
                                                let mut flags = UnlitFlags::empty();
                                                flags.set(
                                                    UnlitFlags::VERTEX_POSITION,
                                                    vertex_position,
                                                );
                                                flags.set(
                                                    UnlitFlags::UNCOMPRESSED_POSITION,
                                                    uncompressed_position,
                                                );
                                                flags.set(UnlitFlags::VERTEX_UV, vertex_uv);
                                                flags.set(
                                                    UnlitFlags::UNCOMPRESSED_UV,
                                                    uncompressed_uv,
                                                );
                                                flags.set(UnlitFlags::VERTEX_COLOR, vertex_color);
                                                flags.set(
                                                    UnlitFlags::VERTEX_INSTANCE,
                                                    vertex_instance,
                                                );
                                                flags.set(
                                                    UnlitFlags::BASE_COLOR_TEXTURE,
                                                    base_color_texture,
                                                );
                                                flags.set(UnlitFlags::VERTEX_JOINTS, vertex_joints);
                                                flags.set(
                                                    UnlitFlags::MORPH_POSITIONS,
                                                    morph_positions,
                                                );
                                                flags.set(UnlitFlags::TEXEL_ARRAY, texel_array);
                                                if base_color_texture && !vertex_uv {
                                                    continue;
                                                }
                                                if uncompressed_position && !vertex_position {
                                                    continue;
                                                }
                                                if uncompressed_uv && !vertex_uv {
                                                    continue;
                                                }
                                                // The joint stream is part of the position
                                                // stream, and a morph target displaces the
                                                // position.
                                                if (vertex_joints || morph_positions)
                                                    && !vertex_position
                                                {
                                                    continue;
                                                }
                                                // A deforming draw reads its pose base
                                                // from the instance stream, so it needs
                                                // one.
                                                if (vertex_joints || morph_positions)
                                                    && !vertex_instance
                                                {
                                                    continue;
                                                }
                                                // The metadata index is per-instance
                                                // too, so a variant that decodes through
                                                // it needs the instance stream.
                                                let compressed = (vertex_position
                                                    && !uncompressed_position)
                                                    || (vertex_uv && !uncompressed_uv);
                                                if (compressed || morph_positions)
                                                    && !vertex_instance
                                                {
                                                    continue;
                                                }
                                                // A variant reading nothing composes an
                                                // empty vertex-input struct.
                                                if flags.is_empty() {
                                                    continue;
                                                }
                                                // A texel is expanded by one of the two
                                                // luminance layouts, or not at all, and
                                                // only a sampled texel can be expanded.
                                                let luminance_layouts: &[UnlitFlags] =
                                                    if base_color_texture {
                                                        &[
                                                            UnlitFlags::empty(),
                                                            UnlitFlags::BASE_COLOR_LUMINANCE,
                                                            UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA,
                                                        ]
                                                    } else {
                                                        &[UnlitFlags::empty()]
                                                    };
                                                for luminance in luminance_layouts {
                                                    variants.push(UnlitOptions {
                                                        flags: flags | *luminance,
                                                        ..UnlitOptions::standard_shape()
                                                    });
                                                }
                                            }
                                        }
                                    }
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
    /// when the inputs are already linear, a linear encoding when they are
    /// sRGB-encoded. Both variants carry the same single entry point.
    #[test]
    fn srgb_output_flag_switches_the_conversion() {
        let composed = |flags: UnlitFlags| {
            let options = UnlitOptions {
                flags: UnlitOptions::standard_shape().flags | flags,
                ..UnlitOptions::standard_shape()
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

    /// The two luminance layouts say how a sampled texel expands, so they
    /// reach the fragment as the expansion itself — and only the one texel the
    /// variant samples, never the color the target writes.
    #[test]
    fn luminance_flags_expand_a_sampled_texel() {
        let composed = |flags: UnlitFlags| {
            let options = UnlitOptions {
                flags: UnlitOptions::standard_shape().flags | flags,
                ..UnlitOptions::standard_shape()
            };
            compose_builtin(&options).expect("compose")
        };

        let full = composed(UnlitFlags::empty());
        assert!(
            !full.contains("srgb_to_linear(texel.r)"),
            "a four-channel texture is uploaded in an sRGB format, so the \
             sampler decodes it"
        );

        let luma = composed(UnlitFlags::BASE_COLOR_LUMINANCE);
        assert!(
            luma.contains("vec3<f32>(srgb_to_linear(texel.r))"),
            "a luminance texel feeds every color channel, decoded"
        );
        assert!(
            luma.contains("texel.a"),
            "a luminance texture has no alpha, so the channel reads as one"
        );

        let luma_alpha = composed(UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA);
        assert!(luma_alpha.contains("vec3<f32>(srgb_to_linear(texel.r))"));
        assert!(
            luma_alpha.contains("texel.g") && !luma_alpha.contains("texel.a"),
            "the second channel of a luminance-alpha texture is its alpha, and \
             coverage never converts"
        );
    }

    /// The luminance layouts describe a sampled texel, so they are meaningless
    /// without one and cannot both describe the same texel.
    #[test]
    #[should_panic(
        expected = "`BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` require \
                    `BASE_COLOR_TEXTURE`"
    )]
    fn a_luminance_layout_without_a_texture_is_rejected() {
        let options = UnlitOptions {
            flags: (UnlitOptions::standard_shape().flags & !UnlitFlags::BASE_COLOR_TEXTURE)
                | UnlitFlags::BASE_COLOR_LUMINANCE,
            ..UnlitOptions::standard_shape()
        };
        let _ = compose_builtin(&options);
    }

    #[test]
    #[should_panic(
        expected = "`BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` are mutually \
                    exclusive"
    )]
    fn two_luminance_layouts_at_once_are_rejected() {
        let options = UnlitOptions {
            flags: UnlitOptions::standard_shape().flags
                | UnlitFlags::BASE_COLOR_LUMINANCE
                | UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA,
            ..UnlitOptions::standard_shape()
        };
        let _ = compose_builtin(&options);
    }

    /// Specialize `options` for `surface` the way a family does.
    fn specialize(options: &mut UnlitOptions, surface: SurfaceKey) -> SurfaceKey {
        SurfaceSpecializer.specialize(surface, options)
    }

    #[test]
    fn surface_specializer_rewrites_only_the_target() {
        let surface = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            depth_stencil_format: Some(wgpu::TextureFormat::Depth24Plus),
            sample_count: 1,
        };
        let mut options = UnlitOptions::standard_shape();
        let flags = options.flags;
        let primitive = options.primitive;
        let blend = options.color_target.blend;

        assert_eq!(
            specialize(&mut options, surface),
            surface,
            "the key is its own canonical form"
        );

        assert_eq!(options.color_target.format, surface.color_format);
        assert_eq!(
            options
                .depth_stencil
                .expect("a depth attachment keeps a state")
                .format,
            surface.depth_stencil_format.unwrap()
        );
        assert_eq!(options.multisample.count, surface.sample_count);

        assert_eq!(options.flags, flags, "the flags are not the surface's");
        assert_eq!(options.primitive, primitive);
        assert_eq!(options.color_target.blend, blend);
    }

    /// A target with no depth attachment yields a pipeline with no depth
    /// state, so `wgpu`'s compatibility check against the pass cannot fail:
    /// a pass with no depth attachment rejects any pipeline that declares one,
    /// regardless of what that state compares or writes.
    #[test]
    fn a_target_without_a_depth_attachment_clears_the_base() {
        let mut options = UnlitOptions::standard_shape();
        assert!(
            options.depth_stencil.is_some(),
            "the base options start with a depth state"
        );

        specialize(
            &mut options,
            SurfaceKey {
                color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth_stencil_format: None,
                sample_count: 1,
            },
        );

        assert!(
            options.depth_stencil.is_none(),
            "a target without depth needs a pipeline without depth"
        );
    }

    /// A target that gains a depth attachment after the state was dropped
    /// gets the reverse-z convention back, so specialization is not one-way.
    #[test]
    fn a_target_with_a_depth_attachment_restores_the_state() {
        let mut options = UnlitOptions::standard_shape();
        specialize(
            &mut options,
            SurfaceKey {
                color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth_stencil_format: None,
                sample_count: 1,
            },
        );
        assert!(options.depth_stencil.is_none());

        specialize(
            &mut options,
            SurfaceKey {
                color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth_stencil_format: Some(wgpu::TextureFormat::Depth32Float),
                sample_count: 1,
            },
        );

        let state = options
            .depth_stencil
            .expect("the attachment brings one back");
        assert_eq!(state.format, wgpu::TextureFormat::Depth32Float);
        assert_eq!(state.depth_write_enabled, Some(true));
        assert_eq!(state.depth_compare, Some(wgpu::CompareFunction::Greater));
    }

    /// The specializer is a cache key like any other: one variant per target,
    /// reused when the same target comes back.
    #[test]
    fn a_surface_specializer_caches_one_variant_per_target() {
        let (device, _queue) = crate::util::test::noop_device();
        let surface = SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: Some(wgpu::TextureFormat::Depth24Plus),
            sample_count: 4,
        };
        let mut variants = Variants::new(&device, SurfaceSpecializer);
        let base = || UnlitOptions::standard(&device);

        let first = variants.specialize(base, surface);
        assert_eq!(
            variants.specialize(base, surface),
            first,
            "the same target reuses its pipeline"
        );

        // A different color format is a different target, so it compiles its
        // own variant rather than reusing the first one.
        let other_format = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            ..surface
        };
        assert_ne!(variants.specialize(base, other_format), first);
        assert_eq!(
            variants.get(first).descriptor().color_target.format,
            surface.color_format
        );
    }

    #[test]
    fn unlit_flags_for_vertex_buffer_maps_each_slot() {
        use crate::specialize::VertexBufferLayoutDesc;

        let attribute = |format, shader_location| wgpu::VertexAttribute {
            format,
            offset: 0,
            shader_location,
        };
        let layout = |step_mode, attributes: &[wgpu::VertexAttribute]| VertexBufferLayoutDesc {
            array_stride: 8,
            step_mode,
            attributes: attributes.into(),
        };

        // The position slot: presence, and the compressed/uncompressed split.
        let compressed = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION)],
        );
        assert_eq!(
            UnlitFlags::for_vertex_buffer(POSITION_SLOT, &compressed),
            UnlitFlags::VERTEX_POSITION
        );
        let uncompressed = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Float32x3, location::POSITION)],
        );
        assert_eq!(
            UnlitFlags::for_vertex_buffer(POSITION_SLOT, &uncompressed),
            UnlitFlags::VERTEX_POSITION | UnlitFlags::UNCOMPRESSED_POSITION
        );

        // The UV-and-color slot: each channel is independent.
        let uv_color = layout(
            wgpu::VertexStepMode::Vertex,
            &[
                attribute(wgpu::VertexFormat::Snorm16x2, location::UV),
                attribute(wgpu::VertexFormat::Unorm8x4, location::COLOR),
            ],
        );
        assert_eq!(
            UnlitFlags::for_vertex_buffer(UV_COLOR_SLOT, &uv_color),
            UnlitFlags::VERTEX_UV | UnlitFlags::VERTEX_COLOR
        );
        let uncompressed_uv = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Float32x2, location::UV)],
        );
        assert_eq!(
            UnlitFlags::for_vertex_buffer(UV_COLOR_SLOT, &uncompressed_uv),
            UnlitFlags::VERTEX_UV | UnlitFlags::UNCOMPRESSED_UV
        );

        // An instance-stepped slot, whatever it carries.
        let instance = layout(
            wgpu::VertexStepMode::Instance,
            &[attribute(wgpu::VertexFormat::Float32x4, location::MODEL_0)],
        );
        assert!(
            UnlitFlags::for_vertex_buffer(UV_COLOR_SLOT, &instance)
                .contains(UnlitFlags::VERTEX_INSTANCE)
        );

        // No result ever leaves the mesh mask or claims a material/target bit.
        for (slot, layout) in [
            (POSITION_SLOT, &compressed),
            (POSITION_SLOT, &uncompressed),
            (UV_COLOR_SLOT, &uv_color),
            (UV_COLOR_SLOT, &uncompressed_uv),
            (UV_COLOR_SLOT, &instance),
        ] {
            let flags = UnlitFlags::for_vertex_buffer(slot, layout);
            assert!(
                UnlitFlags::MESH_MASK.contains(flags),
                "for_vertex_buffer produced flags outside MESH_MASK"
            );
            assert!(!flags.contains(UnlitFlags::BASE_COLOR_TEXTURE));
            assert!(!flags.contains(UnlitFlags::SRGB_TO_LINEAR_OUTPUT));
        }
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
            // Every array read goes through an accessor, whose name survives
            // assembly unmangled as a suffix: the binding it reads is mangled
            // with its module path, so the accessor is what a variant can be
            // checked against.
            assert_eq!(
                wgsl.contains("load_mesh_metadata"),
                options.needs_metadata(),
                "variant {options:?}"
            );
            // The pose arrays are the frame's, so they live in the global
            // group alongside the camera and the metadata.
            assert_eq!(
                wgsl.contains("load_joint_matrix"),
                options.needs_joints(),
                "variant {options:?}"
            );
            assert_eq!(
                wgsl.contains("load_morph_weight"),
                options.needs_morphs(),
                "variant {options:?}"
            );
            assert_eq!(
                wgsl.contains("load_morph_delta"),
                options.needs_morphs(),
                "variant {options:?}"
            );
            // The path decides the resource type: storage buffers when the
            // device has them, textures when it does not. Neither path ever
            // leaves the other's declarations behind.
            let reads_an_array = options.needs_metadata() || options.needs_joints();
            if options.uses_texel_arrays() {
                assert_eq!(
                    wgsl.matches("var<storage, read>").count(),
                    0,
                    "variant {options:?} kept a storage array"
                );
            }
            assert_eq!(
                wgsl.contains("texel_of"),
                options.uses_texel_arrays() && reads_an_array,
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
            assert_eq!(
                vertex_input.contains("joints:") && vertex_input.contains("weights:"),
                options.needs_joints(),
                "variant {options:?}"
            );
            // A deforming draw addresses its pose through the instance
            // stream, so it always declares that input.
            assert_eq!(
                vertex_input.contains("pose:"),
                flags.contains(UnlitFlags::VERTEX_INSTANCE),
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
            // The pose base comes from the instance stream, so a deforming
            // variant without one could not address its joints or weights.
            UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_JOINTS,
            UnlitFlags::VERTEX_POSITION | UnlitFlags::MORPH_POSITIONS,
        ] {
            let result = std::panic::catch_unwind(|| {
                compose_builtin(&UnlitOptions {
                    flags,
                    ..UnlitOptions::standard_shape()
                })
            });
            assert!(result.is_err(), "flags {flags:?} must be rejected");
        }
    }

    #[test]
    fn vertex_strides_follow_the_attribute_formats() {
        let layouts = UnlitOptions {
            flags: UnlitFlags::VERTEX_POSITION
                | UnlitFlags::VERTEX_UV
                | UnlitFlags::VERTEX_INSTANCE,
            ..UnlitOptions::standard_shape()
        }
        .vertex_buffer_layouts();
        let position = layouts[POSITION_SLOT as usize]
            .as_ref()
            .expect("position slot");
        assert_eq!(position.array_stride, wgpu::VertexFormat::Snorm16x4.size());

        let uv_color = layouts[UV_COLOR_SLOT as usize].as_ref().expect("uv slot");
        assert_eq!(uv_color.array_stride, wgpu::VertexFormat::Snorm16x2.size());

        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot");
        // One `vec4` per attribute, with the pose base integer rather than
        // float — the stride sums the formats rather than assuming they agree.
        let expected: u64 = instance
            .attributes
            .iter()
            .map(|attribute| attribute.format.size())
            .sum();
        assert_eq!(instance.array_stride, expected);
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
            ..UnlitOptions::standard_shape()
        };
        assert!(!options.needs_metadata());

        let layouts = options.vertex_buffer_layouts();
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
            (UnlitOptions {
                flags,
                ..UnlitOptions::standard_shape()
            })
            .vertex_buffer_layouts()[UV_COLOR_SLOT as usize]
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
            (UnlitOptions {
                flags,
                ..UnlitOptions::standard_shape()
            })
            .vertex_buffer_layouts()[POSITION_SLOT as usize]
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

    /// The instance slot's GPU attribute offsets and stride must match the
    /// CPU struct. This is the vertex-buffer equivalent of the shader-layout
    /// validation that `const_shader_layout` gives the uniform and storage
    /// structs: vertex attributes are addressed by the offsets declared here,
    /// not by WGSL alignment rules.
    #[test]
    fn instance_vertex_layout_matches_the_struct() {
        use crate::mesh::MeshInstance;
        use core::mem::{offset_of, size_of};

        let layouts = UnlitOptions {
            flags: UnlitFlags::VERTEX_INSTANCE,
            ..UnlitOptions::standard_shape()
        }
        .vertex_buffer_layouts();
        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot");

        assert_eq!(
            instance.array_stride,
            size_of::<MeshInstance>() as u64,
            "the instance stride must step exactly one MeshInstance"
        );
        // The record has to be tightly packed: `IntoBytes` rejects a type with
        // padding, so the sum of the formats is the whole stride and the last
        // attribute ends where the record does.
        let formats: u64 = instance
            .attributes
            .iter()
            .map(|attribute| attribute.format.size())
            .sum();
        assert_eq!(
            formats, instance.array_stride,
            "the instance record carries no padding"
        );

        let model = offset_of!(MeshInstance, model) as u64;
        let vec4 = size_of::<[f32; 4]>() as u64;
        let expected = [
            model,
            model + vec4,
            model + 2 * vec4,
            offset_of!(MeshInstance, base_color) as u64,
            offset_of!(MeshInstance, pose) as u64,
            offset_of!(MeshInstance, metadata_index) as u64,
        ];
        let actual: Vec<u64> = instance.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(
            actual, expected,
            "attribute offsets must line up with the MeshInstance fields"
        );

        // The shader reads three matrix columns followed by the base color, the
        // pose base and the metadata index.
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
                location::BASE_COLOR,
                location::POSE,
                location::METADATA_INDEX,
            ]
        );
        // The matrix columns are floats, the color is quantized, and the pose
        // base and metadata index are integers: one indexes the frame's pose
        // arrays, the other the frame's metadata array.
        let expected_formats = [
            wgpu::VertexFormat::Float32x4,
            wgpu::VertexFormat::Float32x4,
            wgpu::VertexFormat::Float32x4,
            wgpu::VertexFormat::Unorm8x4,
            wgpu::VertexFormat::Uint32x2,
            wgpu::VertexFormat::Uint32,
        ];
        let formats: Vec<wgpu::VertexFormat> =
            instance.attributes.iter().map(|a| a.format).collect();
        assert_eq!(formats, expected_formats, "{instance:?}");
    }
}
