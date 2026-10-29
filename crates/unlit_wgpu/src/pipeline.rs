//! The built-in unlit pipeline and the WESL composition behind it.
//!
//! [`SpecializedLitPipeline`] composes an [`LitVariant`]'s entry shader —
//! the built-in `lit.wesl`, or the caller's own when
//! [`LitOptions::shader`] carries one — builds the layouts the shader
//! expects, and exposes the vertex-buffer layouts the caller declares when
//! creating meshes. Each variant contains exactly the bindings and attributes
//! the selected channels need, so nothing unused reaches the GPU.

#[cfg(feature = "lit")]
use crate::mesh::{
    ChannelEncoding, PositionStreamChannels, PositionStreamWriter, SecondaryFlags,
    SecondaryStreamWriter,
};
#[cfg(feature = "lit")]
use crate::render_attachments::default_depth_stencil_format;
#[cfg(feature = "lit")]
use crate::specialize::{
    PipelineDescriptor, PipelineVariant, SurfaceKey, SurfaceTarget, VertexBufferLayoutDesc,
};
#[cfg(feature = "lit")]
use crate::util::Hashed;
#[cfg(feature = "lit")]
use std::sync::Arc;

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
/// starts at [`crate::mesh::MeshInstance::joints_base`].
pub const JOINTS_BINDING: u32 = 3;
/// Binding slot of the frame's morph weights in the global bind group.
///
/// Like the joint matrices this is one array for the whole frame, and an
/// instance's own slice starts at [`crate::mesh::MeshInstance::morph_base`].
pub const MORPH_WEIGHTS_BINDING: u32 = 4;
/// Binding slot of the frame's morph position displacements in the global bind
/// group.
///
/// A mesh's displacements are its own geometry, but one array holds them for
/// the whole frame and the mesh names its slice through the `morph_deltas_offset`
/// of its [`MeshMetadata`](crate::mesh::MeshMetadata) entry. Pooling them here
/// rather than in a per-mesh buffer is what lets a draw bind no mesh group.
pub const MORPH_DELTAS_BINDING: u32 = 5;
/// Binding slot of the frame's lights in the global bind group.
///
/// One record per light, in view space, packed by the source every frame. The
/// fragment stage reads it, so it is not visible to a vertex stage.
pub const LIGHTS_BINDING: u32 = 6;
/// Binding slot of the frame's cluster cells in the global bind group.
///
/// Each cell's record lists the lights assigned to it; see
/// [`crate::light::CELL_WORDS`].
pub const LIGHT_CELLS_BINDING: u32 = 7;
/// Binding slot of the frame's polygon-light vertices in the global bind group.
///
/// A light whose shape has corners names its slice through its record's
/// `kind.y` and `kind.z`.
pub const LIGHT_VERTICES_BINDING: u32 = 8;
/// Binding slot of the cluster uniform in the global bind group.
///
/// The cluster grid's shape and depth mapping: what a fragment needs to find the
/// cell it is in.
pub const CLUSTER_BINDING: u32 = 9;
/// Binding slot of the frame-wide lighting uniform in the global bind group.
///
/// The ambient term and the frame's directional lights, which reach every
/// fragment: what a fragment needs to light itself before it asks the grid.
pub const LIGHTING_BINDING: u32 = 10;
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

/// Vertex-buffer slot carrying the primary stream: positions and, for a skinned
/// mesh, the joint indices and weights that deform them.
pub const PRIMARY_SLOT: u32 = 0;
/// Vertex-buffer slot carrying the secondary stream: the UV, color and normal a
/// mesh carries beside its position.
pub const SECONDARY_SLOT: u32 = 1;
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
    /// Per-instance joint-matrix base (`Uint32`), read from the instance
    /// stream.
    ///
    /// The instance's first joint matrix in the frame's shared joint array; the
    /// shader reads a joint at this base plus its own joint index.
    pub const JOINTS_BASE: u32 = 9;
    /// Per-instance index of the mesh metadata entry (`Uint32`), read from the
    /// instance stream.
    pub const METADATA_INDEX: u32 = 10;
    /// Per-instance alpha cutoff (`Float32`), read from the instance stream.
    ///
    /// A [`LitOptions::alpha_cutoff`](crate::pipeline::LitOptions::alpha_cutoff)
    /// variant compares a fragment's alpha against it and discards the
    /// fragment when it is below.
    pub const CUTOFF: u32 = 11;
    /// Per-instance morph-weight base (`Uint32`), read from the instance
    /// stream.
    ///
    /// The instance's first morph weight in the frame's shared weight array;
    /// the shader reads a target's weight at this base plus its own target
    /// index. It is a location of its own rather than a lane of
    /// [`JOINTS_BASE`] because skinning and morphing are independent — a
    /// variant that reads one has no business declaring the other.
    pub const MORPH_BASE: u32 = 12;
    /// Vertex normal (`Snorm16x2`), read from the secondary stream.
    ///
    /// Octahedral-encoded, so the shader decodes it on its own; see
    /// [`crate::mesh::compress_normals`].
    pub const NORMAL: u32 = 13;
}

/// Entry point name of the built-in shader's vertex stage.
#[cfg(feature = "lit")]
pub const VS_MAIN: &str = "vs_main";
/// Entry point name of the built-in shader's fragment stage.
#[cfg(feature = "lit")]
pub const FS_MAIN: &str = "fs_main";
/// Label of the built-in entry shader: its shader module's label, and the name
/// a composition error is reported under.
#[cfg(feature = "lit")]
const BUILT_IN_SHADER_LABEL: &str = "unlit_wgpu::lit";

/// Name of the WESL module the built-in entry shader is composed from: the file
/// `shaders/lit.wesl` of this crate's package.
#[cfg(feature = "lit")]
const BUILT_IN_MODULE: &str = "lit";

/// The per-vertex channels the built-in shader variant reads.
///
/// These are not part of [`LitOptions`]: they are a property of the geometry
/// rather than of anything a caller picks, so a pipeline reads them off the
/// vertex buffers its draws are recorded with — every draw of a mesh carries
/// the channels that mesh's buffers hold, so there is nothing to specialize on
/// and nothing for a caller to get wrong. A family resolves them into
/// [`LitVariant::channels`].
///
/// The two halves are [`crate::mesh`]'s own channel types, so the attributes a
/// pipeline declares and the bytes a packed mesh writes come from one
/// description and cannot drift apart.
#[cfg(feature = "lit")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LitVertexChannels {
    /// The position stream: whether it carries a position at all — and in
    /// which encoding — plus the joint pair that rides along with it.
    ///
    /// A stream with no position leaves the geometry a single point at the
    /// instance origin, which is what point, particle and impostor draws want:
    /// the per-instance data alone places the vertex.
    pub position: PositionStreamChannels,
    /// The secondary stream's channels: a UV in either encoding, a vertex
    /// color, a normal, or any combination.
    pub secondary: SecondaryFlags,
}

#[cfg(feature = "lit")]
impl LitVertexChannels {
    /// The channels of a layout that carries none: no position, no joints, and
    /// an empty secondary stream.
    ///
    /// A variant built from this reads no per-vertex attribute at all — the
    /// geometry is a single point at the instance origin — which is what point,
    /// particle and impostor draws want. The instance record is unconditional,
    /// so a `VertexInput` struct always has members.
    pub const fn empty() -> Self {
        Self {
            position: PositionStreamChannels {
                position: None,
                joints: false,
            },
            secondary: SecondaryFlags::empty(),
        }
    }

    /// The channels one vertex-buffer slot carries.
    ///
    /// A channel is identified by its shader location, which is stable across
    /// variants. Only the built-in pipeline's own slots are read —
    /// [`PRIMARY_SLOT`] and [`SECONDARY_SLOT`] — so any other slot, the
    /// per-instance one included, contributes nothing.
    pub fn for_vertex_buffer(slot: u32, layout: &VertexBufferLayoutDesc) -> Self {
        let mut channels = Self::empty();
        match slot {
            PRIMARY_SLOT => {
                for attribute in &layout.attributes {
                    if attribute.shader_location == location::POSITION {
                        channels.position.position = Some(match attribute.format {
                            wgpu::VertexFormat::Float32x3 => ChannelEncoding::UncompressedPosition,
                            wgpu::VertexFormat::Float32x2 => ChannelEncoding::PlanarPosition,
                            _ => ChannelEncoding::CompressedPosition,
                        });
                    }
                    // The joint pair is part of this stream rather than a
                    // stream of its own: the indices are the evidence, and the
                    // weights follow them.
                    if attribute.shader_location == location::JOINTS {
                        channels.position.joints = true;
                    }
                }
            }
            SECONDARY_SLOT => {
                for attribute in &layout.attributes {
                    match attribute.shader_location {
                        location::UV => {
                            channels.secondary |= SecondaryFlags::UV;
                            if attribute.format == wgpu::VertexFormat::Float32x2 {
                                channels.secondary |= SecondaryFlags::UNCOMPRESSED_UV;
                            }
                        }
                        location::COLOR => channels.secondary |= SecondaryFlags::COLOR,
                        location::NORMAL => channels.secondary |= SecondaryFlags::NORMAL,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        channels
    }

    /// The channels a whole vertex layout carries: the union of
    /// [`Self::for_vertex_buffer`] over `layout`'s slots.
    pub fn for_vertex_layout(layout: &[(u32, VertexBufferLayoutDesc)]) -> Self {
        let mut channels = Self::empty();
        for (slot, slot_layout) in layout {
            let slot_channels = Self::for_vertex_buffer(*slot, slot_layout);
            if slot_channels.position.position.is_some() {
                channels.position.position = slot_channels.position.position;
            }
            channels.position.joints |= slot_channels.position.joints;
            channels.secondary |= slot_channels.secondary;
        }
        channels
    }
}

/// How a sampled base-color texel expands into the fragment's color.
#[cfg(feature = "lit")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BaseColorChannels {
    /// The texel is a color in all four channels.
    #[default]
    Rgba,
    /// The texel is luminance: one channel, expanded into every color channel.
    ///
    /// The sampled value is decoded from sRGB on the way, because a
    /// one-channel image cannot be uploaded in an sRGB format: WebGPU offers
    /// the transfer function only for the four-channel `Rgba8UnormSrgb` and
    /// `Bgra8UnormSrgb`, so a luminance texture carries the encoded value and
    /// the shader decodes it.
    Luminance,
    /// The texel is luminance and alpha: two channels holding `l` and `a`,
    /// sampled into the color and the alpha channel.
    ///
    /// The luminance half decodes from sRGB exactly as [`Self::Luminance`]
    /// does; alpha is coverage, so it never converts.
    LuminanceAlpha,
}

/// A caller's own shader code, spliced into the built-in `lit.wesl`.
///
/// The built-in entry shader keeps three marks, each a comment naming the place
/// it sits: one at module scope, one at the end of the vertex stage and one at
/// the end of the fragment stage. A shader is spliced in at those marks while the
/// module is still WESL, before anything is compiled, so each snippet is ordinary
/// WESL: it sees every binding, flag and helper the place it lands in sees, it
/// takes part in the conditional compilation, and its diagnostics point at the
/// line it was written on rather than at a fragment of generated WGSL.
///
/// The marks sit where the shader still has something left to do, which is what
/// keeps a snippet short:
///
/// * The module mark is after every built-in binding, so a snippet can declare
///   the functions, types and constants the stages call, and read the built-in
///   bindings from them.
/// * The vertex mark is after the instance transform, before the clip position
///   is built. The vertex is a `var world` there, and everything downstream —
///   the clip position, the world position the lights are addressed from, the
///   normal and the cluster cell — follows the vertex the snippet leaves.
/// * The fragment mark is after the base color is lit and cut off, before the
///   target's encoding. The color is a `var color` there, so a snippet can
///   rewrite it in place, and what it leaves is what the target receives.
///
/// The layouts and the vertex streams stay the variant's, exactly as they are
/// for the built-in shader: a snippet changes what a stage computes, not what the
/// variant reads. A snippet that declares a binding of its own therefore fails
/// pipeline creation rather than drawing something else; one that declares
/// functions, types or constants over the bindings every variant of that shape
/// already declares works.
///
/// The label names the shader: it is the shader module's label, and a
/// composition error is reported under it.
#[cfg(feature = "lit")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LitShader {
    label: Arc<str>,
    module: Arc<str>,
    vertex: Arc<str>,
    fragment: Arc<str>,
}

#[cfg(feature = "lit")]
impl LitShader {
    /// A shader named `label` whose `module`, `vertex` and `fragment` snippets
    /// are spliced into the built-in entry shader.
    ///
    /// Any snippet may be empty, which leaves its place exactly as the built-in
    /// shader has it.
    pub fn new(
        label: impl Into<Arc<str>>,
        module: impl Into<Arc<str>>,
        vertex: impl Into<Arc<str>>,
        fragment: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            label: label.into(),
            module: module.into(),
            vertex: vertex.into(),
            fragment: fragment.into(),
        }
    }

    /// The name this shader is reported under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The WESL declarations spliced in at module scope.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// The WESL statements spliced into the vertex stage.
    pub fn vertex(&self) -> &str {
        &self.vertex
    }

    /// The WESL statements spliced into the fragment stage.
    pub fn fragment(&self) -> &str {
        &self.fragment
    }
}

#[cfg(feature = "lit")]
impl core::fmt::Debug for LitShader {
    /// Prints the label and each snippet's length rather than the snippets
    /// themselves, so an [`LitOptions`] dump stays readable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LitShader")
            .field("label", &self.label)
            .field("module", &format_args!("{} bytes", self.module.len()))
            .field("vertex", &format_args!("{} bytes", self.vertex.len()))
            .field("fragment", &format_args!("{} bytes", self.fragment.len()))
            .finish()
    }
}

/// The policy a caller decides about a built-in unlit pipeline.
///
/// Everything a draw resolves is absent: the vertex channels, whether the draw
/// samples a material, whether its mesh morphs, the render target and the
/// device's array path are facts a family folds into an [`LitVariant`] when
/// it resolves a draw, not choices a caller can make. What is left is what a
/// caller actually picks.
///
/// [`LitOptions::standard`] is the usual starting point: back-face culling,
/// a filtering base-color texture binding and a four-sample sRGB target with a
/// depth attachment.
#[cfg(feature = "lit")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LitOptions {
    /// The entry shader this policy draws with, or `None` for the built-in
    /// `lit.wesl`.
    ///
    /// A caller's snippets extend the built-in source and nothing else: the
    /// layouts, vertex streams and entry-point names stay the variant's. See
    /// [`LitShader`].
    pub shader: Option<LitShader>,
    /// The color target is sRGB-aware: it encodes the values written to it, so
    /// the fragment converts them from sRGB to linear first.
    ///
    /// The shader multiplies the caller's colors in whichever space they
    /// arrive in — premultiplied sRGB for a user-interface pass, say — and a
    /// target that encodes sRGB would otherwise encode them a second time.
    /// Alpha is coverage rather than color, so it never converts.
    pub srgb_to_linear_output: bool,
    /// How a sampled base-color texel expands into the fragment's color.
    ///
    /// Only meaningful for a variant that samples a base-color texture:
    /// without one there is no texel to expand.
    pub base_color: BaseColorChannels,
    /// Discard a fragment whose alpha is below the instance's cutoff.
    ///
    /// The cutoff itself rides the instance stream rather than the material
    /// group: it is per-instance state, and this switch only selects a code
    /// path. An instance that carries no cutoff leaves the value at zero, which
    /// no alpha falls below, so the path costs a compare and nothing else.
    ///
    /// An untextured fragment can be cut off too: its alpha is the instance
    /// color's, and the cutoff is explicit per instance either way.
    pub alpha_cutoff: bool,
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
    /// ([`crate::render_attachments::default_depth_stencil_format`]) when built
    /// with [`Self::standard`]. [`Self::standard`] is the renderer's reverse-z
    /// convention: depth is cleared to the far plane, so nearer geometry
    /// carries the greater value.
    ///
    /// `wgpu` requires this to agree with the render pass: a pass with a depth
    /// attachment needs a state naming that attachment's format, and a pass
    /// without one needs `None`. [`SurfaceTarget`] therefore follows the
    /// target rather than leaving a stale format behind, so a pipeline always
    /// matches the pass it is recorded into.
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

#[cfg(feature = "lit")]
impl LitOptions {
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
    pub fn standard(device: &wgpu::Device) -> Self {
        let mut options = Self::standard_shape();
        if let Some(depth_stencil) = &mut options.depth_stencil {
            depth_stencil.format = default_depth_stencil_format(device);
        }
        options
    }

    /// The standard variant's device-independent fields: the policy, primitive
    /// state, color target and multisample state, with a placeholder
    /// depth-stencil format that [`Self::standard`] replaces with the device's
    /// default.
    ///
    /// Available crate-wide so tests and builders that have no device can
    /// construct the standard configuration without one.
    pub(crate) fn standard_shape() -> Self {
        Self {
            shader: None,
            srgb_to_linear_output: false,
            base_color: BaseColorChannels::Rgba,
            alpha_cutoff: false,
            texture_filtering: true,
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
        }
    }

    /// The material bind-group layout this policy's variants declare.
    ///
    /// The layout holds the base-color texture and its sampler; whether a
    /// variant actually declares it is the draw's answer
    /// ([`LitVariant::base_color_texture`]), so this is only the shape of the
    /// group. Building it here rather than from a variant lets a caller
    /// allocate a material without having resolved a draw first.
    pub fn material_bind_group_layout(&self, device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let mut entries = arrayvec::ArrayVec::<wgpu::BindGroupLayoutEntry, 2>::new();
        entries.push(wgpu::BindGroupLayoutEntry {
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
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: BASE_COLOR_SAMPLER_BINDING,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: if self.texture_filtering {
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
            } else {
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering)
            },
            count: None,
        });
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unlit_wgpu::lit::material"),
            entries: &entries,
        })
    }
}

/// The complete identity of a built-in unlit pipeline, and its blueprint.
///
/// A family resolves one from an entity's [`LitOptions`] and the facts its
/// draw carries: the mesh's vertex channels, whether the draw binds a material,
/// whether the mesh morphs, the mesh's index format and the render target. Two
/// draws that agree on all of it share one compiled pipeline; because the
/// variant is the complete description of the blueprint, the cache key is
/// canonical by construction.
#[cfg(feature = "lit")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LitVariant {
    /// The caller's policy, with its hash computed once. The variant is the
    /// cache key, so the stored word is what every per-frame lookup hashes;
    /// rewrite the policy through [`Hashed::update`] rather than the value
    /// itself, so the word follows the change.
    pub options: Hashed<LitOptions>,
    /// The render target the pipeline is built for.
    pub surface: SurfaceKey,
    /// The per-vertex channels the mesh's layout carries.
    pub channels: LitVertexChannels,
    /// Whether the draw binds a material group and so samples a base-color
    /// texture.
    pub base_color_texture: bool,
    /// Whether the mesh carries morph displacements.
    pub morph: bool,
    /// The strip index format the mesh's index buffer requires, if any.
    pub strip_index_format: Option<wgpu::IndexFormat>,
    /// Whether the variant skips the frame's lights and draws its base color
    /// unchanged.
    ///
    /// A variant that is not unlit reads the light arrays and the cluster
    /// uniform from the global group and multiplies its base color by what they
    /// sum to. An unlit one declares none of those bindings, so the capability
    /// costs it nothing. Lighting is the common case, so the flag is stated as
    /// the exception the caller asks for.
    pub unlit: bool,
}

#[cfg(feature = "lit")]
impl LitVariant {
    /// The WESL `@if` features the variant asks for, paired with their state.
    ///
    /// Every name appears, so the composed variant never sees a name it does
    /// not know. The per-vertex names come from [`Self::channels`], the material
    /// one from [`Self::base_color_texture`] and the policy ones from
    /// [`Self::options`]. The per-instance record is not conditional at all, so
    /// it contributes no names here: the built-in vertex stream always
    /// declares and binds an instance buffer.
    ///
    /// `texel_arrays` is the device's answer rather than the variant's, so it
    /// is passed in from the caller that read the device.
    fn features(&self, texel_arrays: bool) -> [(&'static str, bool); 16] {
        [
            ("VERTEX_POSITION", self.channels.position.position.is_some()),
            (
                "UNCOMPRESSED_POSITION",
                matches!(
                    self.channels.position.position,
                    Some(ChannelEncoding::UncompressedPosition | ChannelEncoding::PlanarPosition)
                ),
            ),
            (
                "PLANAR_POSITION",
                matches!(
                    self.channels.position.position,
                    Some(ChannelEncoding::PlanarPosition)
                ),
            ),
            (
                "VERTEX_UV",
                self.channels.secondary.contains(SecondaryFlags::UV),
            ),
            (
                "UNCOMPRESSED_UV",
                self.channels
                    .secondary
                    .contains(SecondaryFlags::UNCOMPRESSED_UV),
            ),
            (
                "VERTEX_COLOR",
                self.channels.secondary.contains(SecondaryFlags::COLOR),
            ),
            (
                "VERTEX_NORMAL",
                self.channels.secondary.contains(SecondaryFlags::NORMAL),
            ),
            ("BASE_COLOR_TEXTURE", self.base_color_texture),
            ("SRGB_TO_LINEAR_OUTPUT", self.options.srgb_to_linear_output),
            ("VERTEX_JOINTS", self.channels.position.joints),
            ("MORPH_POSITIONS", self.morph),
            ("TEXEL_ARRAY", texel_arrays),
            (
                "BASE_COLOR_LUMINANCE",
                self.options.base_color == BaseColorChannels::Luminance,
            ),
            (
                "BASE_COLOR_LUMINANCE_ALPHA",
                self.options.base_color == BaseColorChannels::LuminanceAlpha,
            ),
            ("ALPHA_CUTOFF", self.options.alpha_cutoff),
            ("LIT", !self.unlit),
        ]
    }

    /// Whether the variant deforms its vertices by joint matrices and so reads
    /// the global group's array of them.
    pub fn needs_joints(&self) -> bool {
        self.channels.position.joints
    }

    /// Whether the variant reads the global group's morph arrays: the
    /// per-instance weights and the per-mesh displacements.
    pub fn needs_morphs(&self) -> bool {
        self.morph
    }

    /// Whether the variant reads the global group's mesh-metadata array: the
    /// per-mesh decode parameters and the addressing a morph reads.
    ///
    /// Mirrors the shader's metadata condition, so the layout and the composed
    /// variant agree on whether the binding exists. A morph needs the array
    /// even when every channel is uncompressed, because the struct also carries
    /// the vertex offset and morph count its displacements are addressed by.
    pub fn needs_metadata(&self) -> bool {
        let compressed_position = matches!(
            self.channels.position.position,
            Some(ChannelEncoding::CompressedPosition)
        );
        let compressed_uv = self.channels.secondary.contains(SecondaryFlags::UV)
            && !self
                .channels
                .secondary
                .contains(SecondaryFlags::UNCOMPRESSED_UV);
        compressed_position || compressed_uv || self.needs_morphs()
    }

    /// The secondary vertex stream this variant expects.
    ///
    /// The stream is described in [`crate::mesh`]'s own terms: this variant's
    /// channels are translated into the ones that make up a vertex stream, so
    /// packing meshes for this pipeline needs nothing specific to it.
    pub fn secondary_stream(&self) -> SecondaryStreamWriter {
        SecondaryStreamWriter {
            flags: self.channels.secondary,
        }
    }

    /// The position vertex stream this variant expects.
    ///
    /// The joint indices and weights the variant deforms with belong to this
    /// stream rather than one of their own, so a skinned variant's stream is
    /// wider by the joint pair and nothing else changes.
    pub fn position_stream(&self) -> PositionStreamWriter {
        PositionStreamWriter {
            channels: self.channels.position,
        }
    }

    /// Reject a variant whose array path the device cannot serve.
    ///
    /// The texel path works on every device — a texture read is universal — so
    /// forcing it on a device that also has storage buffers is allowed. The
    /// reverse is not: a bind-group layout naming a storage buffer is rejected
    /// outright by a device without them, so the failure is caught here with the
    /// reason rather than left to wgpu's validation.
    ///
    /// A variant that declares no array binding at all — the UI's, which reads
    /// only full-precision vertex attributes — has nothing to serve either way,
    /// so it is not asked to pick a path.
    ///
    /// # Panics
    ///
    /// If the variant reads storage buffers on a device that has none.
    fn assert_device_supports_arrays(&self, device: &wgpu::Device, texel_arrays: bool) {
        let reads_an_array = self.needs_metadata() || self.needs_joints() || !self.unlit;
        assert!(
            !reads_an_array || texel_arrays || supports_storage_buffers(device),
            "this variant reads its arrays from storage buffers, but the device has none \
             (its `max_storage_buffers_per_shader_stage` is 0, as WebGL2's is): start from \
             `LitOptions::standard`, which picks the device's array path"
        );
    }
}

#[cfg(feature = "lit")]
impl LitVariant {
    /// Build the bind-group layouts this variant declares, without compiling
    /// the pipeline.
    ///
    /// The layouts come from the same variant a pipeline is built from, so the
    /// two agree: the global group always exists, and the material group only
    /// for a variant that samples a base-color texture. The mesh group is
    /// always `None` — the built-in variants bind nothing there.
    ///
    /// The array path is the device's answer rather than the variant's, so it
    /// is read here: a cache is built for one device, which is why the path
    /// never enters the variant key.
    pub fn bind_group_layouts(&self, device: &wgpu::Device) -> LitBindGroupLayouts {
        let texel_arrays = !supports_storage_buffers(device);
        self.assert_device_supports_arrays(device, texel_arrays);
        // The group holds the frame's shared inputs: the camera, the frame
        // globals, the mesh-metadata decode parameters, the pose arrays every
        // instance slices into, and the morph displacements every mesh slices
        // into. A variant that reads none of the optional ones declares no
        // binding for them. The builder is the same one a caller's own pipeline
        // uses, so the built-in variants carry no private layout path.
        let bindings = GlobalBindings {
            metadata: self.needs_metadata(),
            joints: self.needs_joints(),
            morphs: self.needs_morphs(),
            // A lit variant reads the frame's lights, its cells and its
            // polygon vertices; an unlit one declares no such binding.
            lights: !self.unlit,
        };
        let global = global_bind_group_layout(device, bindings, texel_arrays);

        let material = self
            .base_color_texture
            .then(|| self.options.material_bind_group_layout(device));

        // The built-in variants bind nothing at the mesh group: every input
        // they read is in the global group or on the instance stream. The
        // index remains a general extension point a caller's pipeline can use.
        let mesh = None;

        LitBindGroupLayouts {
            global,
            material,
            mesh,
        }
    }

    /// The vertex-buffer declarations the built-in shader expects, in slot
    /// order.
    ///
    /// The first two slots follow the variant's channels; the third is always
    /// the instance stream, which every built-in variant binds. A slot whose
    /// variant declares no attributes is reported as `None`, so a variant never
    /// binds a buffer the shader does not declare.
    pub fn vertex_buffer_layouts(&self) -> [Option<VertexBufferLayoutDesc>; 3] {
        // Each stream describes its own layout, so the attributes a pipeline
        // declares and the bytes a packed mesh writes come from one
        // description and cannot drift apart.
        let position = self.position_stream().channels.channels();
        let secondary = self.secondary_stream().channels();
        [
            (!position.is_empty()).then(|| position.layout()),
            (!secondary.is_empty()).then(|| secondary.layout()),
            Some(self.instance_layout()),
        ]
    }

    /// The instance stream every built-in variant expects.
    ///
    /// One record layout serves every variant of a frame — the source uploads a
    /// single `&[MeshInstance]` buffer and binds it at [`INSTANCE_SLOT`] for
    /// every draw — so the stride is always the whole [`MeshInstance`]. The
    /// declared attributes mirror the shader's own input declarations: the
    /// model matrix, the base color, the pose bases and the metadata index
    /// unconditionally, the alpha cutoff under the feature that declares it.
    /// A pipeline never declares an attribute the shader does not, which the
    /// validation layers would otherwise flag on every draw.
    ///
    /// [`MeshInstance`]: crate::mesh::MeshInstance
    fn instance_layout(&self) -> VertexBufferLayoutDesc {
        let mut attributes = crate::specialize::VertexAttributes::new();
        let field = |offset: usize| offset as u64;
        let model = core::mem::offset_of!(crate::mesh::MeshInstance, model);
        let column = core::mem::size_of::<[f32; 4]>() as u64;
        let mut fields = vec![
            (
                location::MODEL_0,
                wgpu::VertexFormat::Float32x4,
                field(model),
            ),
            (
                location::MODEL_1,
                wgpu::VertexFormat::Float32x4,
                field(model) + column,
            ),
            (
                location::MODEL_2,
                wgpu::VertexFormat::Float32x4,
                field(model) + 2 * column,
            ),
            (
                location::BASE_COLOR,
                wgpu::VertexFormat::Unorm8x4,
                field(core::mem::offset_of!(crate::mesh::MeshInstance, base_color)),
            ),
            (
                location::JOINTS_BASE,
                wgpu::VertexFormat::Uint32,
                field(core::mem::offset_of!(
                    crate::mesh::MeshInstance,
                    joints_base
                )),
            ),
            (
                location::METADATA_INDEX,
                wgpu::VertexFormat::Uint32,
                field(core::mem::offset_of!(
                    crate::mesh::MeshInstance,
                    metadata_index
                )),
            ),
        ];
        // The one conditional instance input: the shader declares the cutoff
        // only under ALPHA_CUTOFF, so a variant that never cuts must not
        // declare it either — the validation layers flag an attribute the
        // vertex stage never touches.
        if self.options.alpha_cutoff {
            fields.push((
                location::CUTOFF,
                wgpu::VertexFormat::Float32,
                field(core::mem::offset_of!(crate::mesh::MeshInstance, cutoff)),
            ));
        }
        fields.push((
            location::MORPH_BASE,
            wgpu::VertexFormat::Uint32,
            field(core::mem::offset_of!(crate::mesh::MeshInstance, morph_base)),
        ));
        for (shader_location, format, offset) in fields {
            attributes.push(wgpu::VertexAttribute {
                format,
                offset,
                shader_location,
            });
        }
        VertexBufferLayoutDesc {
            // The stride is the record's whole size rather than the sum of the
            // declared formats, so a variant that declares a subset still steps
            // over the fields it omits, which keeps every instance aligned in
            // one shared buffer.
            array_stride: core::mem::size_of::<crate::mesh::MeshInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes,
        }
    }
}

#[cfg(feature = "lit")]
impl PipelineVariant<wgpu::RenderPipeline> for LitVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        self.clone()
    }
}

#[cfg(feature = "lit")]
impl PipelineDescriptor<wgpu::RenderPipeline> for LitVariant {
    /// Compose this variant's entry shader and build the pipeline, so a variant
    /// compiles through the same [PipelineDescriptor] mechanism a caller's own
    /// descriptor does.
    ///
    /// The entry shader is the built-in `lit.wesl`, with the options' own
    /// snippets ([`LitOptions::shader`]) spliced into it at its marks.
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        let texel_arrays = !supports_storage_buffers(device);
        let label = match &self.options.shader {
            Some(shader) => shader.label(),
            None => BUILT_IN_SHADER_LABEL,
        };
        let wgsl = compose(self, texel_arrays).unwrap_or_else(|error| panic!("{label}: {error}"));
        let module = crate::util::shader_module(device, &wgsl, label);

        let layouts = self.bind_group_layouts(device);
        let layout_refs = [
            Some(&layouts.global),
            layouts.material.as_ref(),
            layouts.mesh.as_ref(),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unlit_wgpu::lit::layout"),
            bind_group_layouts: &layout_refs,
            immediate_size: 0,
        });

        // The caller's policy is the base; the target and the mesh's strip
        // index format are applied here, because the variant carries them
        // rather than the caller. The rewrite starts from a plain clone: the
        // variant's word describes the policy it was keyed on, and the target
        // is applied after hashing rather than into it.
        let mut options = (*self.options).clone();
        options.set_surface(self.surface);
        options.primitive.strip_index_format = self.strip_index_format;

        let vertex_buffers = self.vertex_buffer_layouts();
        let vertex_buffers: Vec<Option<wgpu::VertexBufferLayout<'_>>> = vertex_buffers
            .iter()
            .map(|layout| layout.as_ref().map(VertexBufferLayoutDesc::as_wgpu))
            .collect();
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unlit_wgpu::lit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(VS_MAIN),
                compilation_options: Default::default(),
                buffers: &vertex_buffers,
            },
            primitive: options.primitive,
            depth_stencil: options.depth_stencil.clone(),
            multisample: options.multisample,
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(FS_MAIN),
                compilation_options: Default::default(),
                targets: &[Some(options.color_target.clone())],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

/// The module path a variant's entry shader is composed at.
///
/// The entry shader is always the built-in `lit` module of the crate's package:
/// a caller's snippets are spliced into that module rather than replacing it,
/// so the path does not depend on the options.
#[cfg(feature = "lit")]
fn entry_shader_path() -> wesl::syntax::ModulePath {
    wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("unlit_wgpu".to_owned()),
        vec![BUILT_IN_MODULE.to_owned()],
    )
}

/// The comment the built-in entry shader marks its module-scope splice with.
#[cfg(feature = "lit")]
const MODULE_HOOK: &str = "// unlit_wgpu::hook module";

/// The comment the built-in entry shader marks its vertex-stage splice with.
#[cfg(feature = "lit")]
const VERTEX_HOOK: &str = "// unlit_wgpu::hook vertex";

/// The comment the built-in entry shader marks its fragment-stage splice with.
#[cfg(feature = "lit")]
const FRAGMENT_HOOK: &str = "// unlit_wgpu::hook fragment";

/// The source of the built-in entry shader, exactly as the package bundles it.
///
/// It is the file `shaders/lit.wesl` itself, so it carries the hook marks a
/// snippet is spliced at.
#[cfg(feature = "lit")]
fn built_in_entry_source() -> &'static str {
    crate::shader::lit::MODULE.source
}

/// `source` with each hook mark replaced by the snippet belonging to it.
///
/// The splice happens on WESL, before anything is parsed or compiled, so a
/// snippet takes part in the same conditional compilation as the code around it
/// and a diagnostic inside one points at the line it was written on.
///
/// # Panics
///
/// If the built-in source does not carry all three marks. They are checked
/// rather than trusted because a mark is a bare comment: losing one while
/// editing the shader would otherwise silently drop every snippet aimed at it.
#[cfg(feature = "lit")]
fn splice(source: &str, shader: &LitShader) -> String {
    let spliced = splice_at(source, MODULE_HOOK, shader.module());
    let spliced = splice_at(&spliced, VERTEX_HOOK, shader.vertex());
    let spliced = splice_at(&spliced, FRAGMENT_HOOK, shader.fragment());
    assert!(
        !spliced.contains(MODULE_HOOK)
            && !spliced.contains(VERTEX_HOOK)
            && !spliced.contains(FRAGMENT_HOOK),
        "the built-in entry shader carries all three hook marks"
    );
    spliced
}

/// `source` with the line carrying the first `mark` replaced by `snippet`.
///
/// The whole line goes, indentation included, so the snippet stands where the
/// mark stood and an empty snippet leaves a blank line rather than an indented
/// tail. A missing mark leaves the source untouched; [`splice`] is what
/// rejects one.
#[cfg(feature = "lit")]
fn splice_at(source: &str, mark: &str, snippet: &str) -> String {
    let Some(at) = source.find(mark) else {
        return source.to_owned();
    };
    let line_start = source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let line_end = source[at..]
        .find('\n')
        .map_or(source.len(), |newline| at + newline);
    let mut spliced = String::with_capacity(source.len() + snippet.len());
    spliced.push_str(&source[..line_start]);
    spliced.push_str(snippet);
    spliced.push_str(&source[line_end..]);
    spliced
}

/// Compose `variant`'s entry shader into one WGSL module.
///
/// The main module is the built-in `lit.wesl`, with the options' snippets
/// spliced into it at its hook marks. It is composed as the package's own
/// `lit` module, so a snippet that names `package::light` or
/// `unlit_wgpu::light` reaches the modules the shader around it imports, and
/// sees the variant's flags.
///
/// `texel_arrays` is the device answer rather than the variant one, so it is
/// passed in from whoever read the device; it selects the shader array path
/// and never enters the variant key, because a cache is built for one device.
///
/// # Panics
///
/// If the variant contradicts itself: the base-color texture is sampled with a
/// per-vertex UV that is not read, a deformation displaces a position that is
/// not read, or a luminance layout expands a texel that is never sampled.
///
/// The per-vertex channels cannot contradict each other the way flags once
/// could: an encoding is a property of the channel it encodes, so
/// [`LitVertexChannels`] has no state in which a channel is uncompressed
/// without being read. The per-instance fields cannot either: the instance
/// stream is bound for every variant, so there is no state in which one is
/// read without the stream that carries it.
#[cfg(feature = "lit")]
fn compose(variant: &LitVariant, texel_arrays: bool) -> Result<String, ComposeError> {
    let main_path = entry_shader_path();
    assert!(
        !variant.base_color_texture || variant.channels.secondary.contains(SecondaryFlags::UV),
        "the base-color texture is sampled with the per-vertex UV, so sampling one requires a UV channel"
    );
    assert!(
        !variant.channels.position.joints || variant.channels.position.position.is_some(),
        "the joint pair is part of the position stream, so a skinned variant needs a position to deform"
    );
    assert!(
        !variant.morph || variant.channels.position.position.is_some(),
        "a morph target displaces the position, so `MORPH_POSITIONS` requires a position channel"
    );
    assert!(
        variant.options.base_color == BaseColorChannels::Rgba || variant.base_color_texture,
        "the luminance flags describe how a sampled base-color texel expands, so `BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` require a sampled base-color texture"
    );

    let mut compile_options = wesl::CompileOptions::default();
    for (name, enabled) in variant.features(texel_arrays) {
        compile_options.features.set(name, enabled);
    }
    compile_options.keep_main = true;

    let mut packages = wesl::resolver::PackageResolver::new();
    packages.add_package(&crate::shader::PACKAGE);

    let composed = match &variant.options.shader {
        None => {
            wesl::Compiler::new_with_resolver(compile_options, packages).compile_module(&main_path)
        }
        Some(shader) => {
            // The spliced source replaces the package's own `lit` module, and
            // nothing else: mounted at that one path, it keeps the imports the
            // package resolves -- `unlit_wgpu::` and the shader's own
            // `package::` alike -- and the module name it is reported under.
            //
            // The router hands a mounted resolver the path below the mount
            // point, so mounting at the entry path leaves the virtual module
            // the empty path -- and leaves every sibling (`unlit_wgpu::light`
            // and the rest) to the package that owns it.
            let mut modules: wesl::resolver::VirtualResolver<'static> =
                wesl::resolver::VirtualResolver::new();
            modules.add_module(
                wesl::syntax::ModulePath::new(wesl::syntax::PathOrigin::Absolute, Vec::new()),
                splice(built_in_entry_source(), shader).into(),
            );
            let mut resolver = wesl::resolver::Router::new();
            resolver.mount_resolver(main_path.clone(), modules);
            resolver.mount_fallback_resolver(packages);
            wesl::Compiler::new_with_resolver(compile_options, resolver).compile_module(&main_path)
        }
    };
    composed
        .map(|result| result.syntax.to_string())
        .map_err(ComposeError::Compile)
}
/// Failure reasons reported by [`compose`].
#[cfg(feature = "lit")]
#[derive(Debug)]
enum ComposeError {
    /// The WESL compiler rejected the module.
    Compile(wesl::Error),
}

#[cfg(feature = "lit")]
impl core::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "WESL compilation failed: {error}"),
        }
    }
}

#[cfg(feature = "lit")]
impl std::error::Error for ComposeError {}

/// Which of a source's global buffers a pipeline reads.
///
/// The camera and frame-globals uniforms are always bound; the three arrays are
/// optional and a pipeline declares only the ones it reads. A caller's own
/// pipeline passes the same description to `global_bind_group_layout` as the
/// built-in unlit variants do, so binding the frame's shared inputs is not a
/// privilege of the built-in shader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GlobalBindings {
    /// Whether the pipeline reads the frame's mesh-metadata array.
    pub metadata: bool,
    /// Whether the pipeline reads the frame's joint matrices.
    pub joints: bool,
    /// Whether the pipeline reads the frame's morph weights and displacements.
    pub morphs: bool,
    /// Whether the pipeline reads the frame's lights: the bounded-light arrays,
    /// the cluster uniform and the frame-wide lighting uniform.
    ///
    /// One flag for all five bindings: a pipeline that lights its fragments
    /// reads the three arrays and the two uniforms together, so a finer split
    /// would only describe combinations no pipeline can have.
    pub lights: bool,
}

/// The layout entry binding an array of `element_size`-byte elements.
///
/// On the storage path it is a read-only storage buffer whose binding minimum is
/// one element, so the array may grow without invalidating the layout. On the
/// texel path it is a non-filterable float texture, which is what
/// `textureLoad` reads.
pub fn array_entry(
    binding: u32,
    element_size: u64,
    visibility: wgpu::ShaderStages,
    texel_arrays: bool,
) -> wgpu::BindGroupLayoutEntry {
    if texel_arrays {
        return crate::texel_array::TexelArrayLayout::binding(binding, visibility);
    }
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
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

/// Build the global bind-group layout a pipeline with `bindings` declares.
///
/// The camera and frame-globals uniforms are always present; the arrays are
/// present exactly when `bindings` names them. The array path is the device's
/// answer rather than the pipeline's, so `texel_arrays` is passed in: a device
/// with storage buffers binds each array as one, a device without them — WebGL2
/// — binds the same bytes as a texture.
///
/// The built-in unlit variants build their global layout here, so a caller's own
/// pipeline declares the same group by the same rules.
pub fn global_bind_group_layout(
    device: &wgpu::Device,
    bindings: GlobalBindings,
    texel_arrays: bool,
) -> wgpu::BindGroupLayout {
    let mut entries = arrayvec::ArrayVec::<wgpu::BindGroupLayoutEntry, 11>::new();
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: CAMERA_BINDING,
        // The near plane and the eye position are view values a fragment stage
        // may want to reason with: the lit pipeline derives its depth slices
        // and its fallback geometric normal from them.
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
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
    if bindings.metadata {
        entries.push(array_entry(
            MESH_METADATA_BINDING,
            <crate::mesh::MeshMetadata as const_shader_layout::ShaderLayout>::SIZE.get(),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
    }
    if bindings.joints {
        entries.push(array_entry(
            JOINTS_BINDING,
            <crate::mesh::JointMatrix as const_shader_layout::ShaderLayout>::SIZE.get(),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
    }
    if bindings.morphs {
        entries.push(array_entry(
            MORPH_WEIGHTS_BINDING,
            wgpu::BufferAddress::from(core::mem::size_of::<f32>() as u64),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
        // One position component of one target: like every other array entry
        // the storage binding minimum is a single element, so a mesh joining or
        // leaving the pool never invalidates the layout.
        entries.push(array_entry(
            MORPH_DELTAS_BINDING,
            wgpu::BufferAddress::from(core::mem::size_of::<f32>() as u64),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
    }
    if bindings.lights {
        // Lighting happens in the fragment stage, where the view-space position
        // and normal are; the vertex stage reads none of these, so declaring
        // them there would only spend the vertex stage's array quota.
        let visibility = wgpu::ShaderStages::FRAGMENT;
        entries.push(array_entry(
            LIGHTS_BINDING,
            <crate::light::LightGpu as const_shader_layout::ShaderLayout>::SIZE.get(),
            visibility,
            texel_arrays,
        ));
        // One cell is a fixed number of words rather than one: the record lists
        // the lights assigned to the cell, so the binding minimum is a whole
        // record and a fragment reads the cell's own slice.
        entries.push(array_entry(
            LIGHT_CELLS_BINDING,
            wgpu::BufferAddress::from(
                (crate::light::CELL_WORDS * core::mem::size_of::<u32>()) as u64,
            ),
            visibility,
            texel_arrays,
        ));
        entries.push(array_entry(
            LIGHT_VERTICES_BINDING,
            wgpu::BufferAddress::from(core::mem::size_of::<glam::Vec4>() as u64),
            visibility,
            texel_arrays,
        ));
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: CLUSTER_BINDING,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: Some(
                    <crate::light::ClusterUniform as const_shader_layout::ShaderLayout>::SIZE,
                ),
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: LIGHTING_BINDING,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: Some(
                    <crate::light::LightingUniform as const_shader_layout::ShaderLayout>::SIZE,
                ),
            },
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit_wgpu::lit::globals"),
        entries: &entries,
    })
}

/// The bind-group layouts a [`SpecializedLitPipeline`] variant declares.
///
/// Built by [`LitVariant::bind_group_layouts`] from the same variant a
/// pipeline is built from, so the two agree: the global group always exists and
/// the material group only for a variant that samples a base-color texture. The
/// mesh group is always `None` — the built-in variants bind nothing there — but
/// stays in the structure because the index is a general extension point.
#[cfg(feature = "lit")]
#[derive(Clone, Debug)]
pub struct LitBindGroupLayouts {
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
/// with the [`LitVariant`] it was built from.
///
/// [`LitVariant`] is its own [`PipelineVariant`] and [`PipelineDescriptor`],
/// so the built-in shader compiles through the same cache a caller's own
/// variant does, and the layouts it declares are derived from the variant
/// rather than stored beside the pipeline.
#[cfg(feature = "lit")]
pub type SpecializedLitPipeline =
    crate::specialize::SpecializedPipeline<wgpu::RenderPipeline, LitVariant>;

/// Whether `device` can read a storage buffer from a shader.
///
/// WebGL2 cannot: it has no storage buffers at all, and a bind-group layout
/// naming one is rejected outright. Such a device reports a limit of zero,
/// which is the only signal available — [`wgpu::Device`] exposes its limits and
/// features but not the downlevel capabilities that would say so directly.
///
/// A device that can bind storage buffers reports a non-zero limit, so this is
/// what decides between the built-in shader's buffer and texel array paths.
#[cfg(feature = "lit")]
pub fn supports_storage_buffers(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    limits.max_storage_buffers_per_shader_stage > 0 && limits.max_storage_buffer_binding_size > 0
}

#[cfg(feature = "lit")]
impl SurfaceTarget for LitOptions {
    /// Rewrites exactly the three fields the target reaches: the color format,
    /// the sample count and the depth-stencil format. The shader variant,
    /// primitive state, blend state and write mask stay the caller's.
    ///
    /// [`LitOptions::srgb_to_linear_output`] is deliberately left alone: it
    /// describes the fragment's input encoding, not the target format.
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

#[cfg(feature = "lit")]
impl SurfaceTarget for LitVariant {
    /// The variant's policy is hashed, so the rewrite goes through
    /// [`Hashed::update`] rather than a mutable borrow of the options: the
    /// stored word follows the changed fields, and the variant stays its own
    /// valid cache key.
    fn set_surface(&mut self, surface: SurfaceKey) {
        self.options.update(|options| options.set_surface(surface));
    }
}

/// The depth-stencil state a target with a depth attachment starts from: the
/// reverse-z convention, with the format filled in from the attachment.
#[cfg(feature = "lit")]
fn default_depth_stencil_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Greater),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

#[cfg(all(test, feature = "lit"))]
mod tests {
    use super::*;
    use crate::specialize::Variants;

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

    /// A snippet is spliced into the built-in entry shader rather than
    /// replacing it: the built-in stages still run, and the snippet's own
    /// statements are what the marks were replaced with.
    #[test]
    fn a_caller_shader_is_spliced_into_the_built_in_source() {
        let shader = LitShader::new(
            "caller",
            "fn marker_module() -> f32 { return 0.5; }",
            "let marker_vertex = marker_module() * 0.25; world = world + vec3<f32>(0.0, marker_vertex, 0.0);",
            "let marker_fragment = 0.25; color = vec4<f32>(marker_fragment, color.g, color.b, color.a);",
        );
        let mut options = LitOptions::standard_shape();
        options.shader = Some(shader);
        let candidate = variant(
            options,
            channels(
                Some(ChannelEncoding::UncompressedPosition),
                false,
                SecondaryFlags::empty(),
            ),
            false,
            false,
        );

        let wgsl = compose(&candidate, false).expect("the caller's snippets compose");
        // The built-in stages are still there: a snippet takes part in them
        // rather than standing in for them.
        assert!(wgsl.contains("fn vs_main"));
        assert!(wgsl.contains("fn fs_main"));
        // Each snippet landed in the place it belongs to -- module scope before
        // either stage, the vertex stage before the fragment one -- and the
        // marks are gone rather than left in the composed module. A module
        // snippet declares what the stages call, which is the whole reason it
        // exists: here the vertex snippet uses a function it declared.
        let module_at = wgsl
            .find("fn marker_module")
            .expect("the module snippet spliced");
        let vertex_at = wgsl
            .find("marker_vertex")
            .expect("the vertex snippet spliced");
        let fragment_at = wgsl
            .find("marker_fragment")
            .expect("the fragment snippet spliced");
        assert!(module_at < vertex_at);
        assert!(vertex_at < fragment_at);
        assert!(!wgsl.contains(MODULE_HOOK));
        assert!(!wgsl.contains(VERTEX_HOOK));
        assert!(!wgsl.contains(FRAGMENT_HOOK));
    }

    /// An empty snippet leaves its stage exactly as the built-in shader has it.
    #[test]
    fn an_empty_snippet_leaves_the_built_in_stage_alone() {
        let built_in = variant(
            LitOptions::standard_shape(),
            channels(
                Some(ChannelEncoding::UncompressedPosition),
                false,
                SecondaryFlags::empty(),
            ),
            false,
            false,
        );
        let mut options = LitOptions::standard_shape();
        options.shader = Some(LitShader::new("empty", "", "", ""));
        let with_snippets = variant(
            options,
            channels(
                Some(ChannelEncoding::UncompressedPosition),
                false,
                SecondaryFlags::empty(),
            ),
            false,
            false,
        );

        assert_eq!(
            code_lines(&compose(&built_in, false).expect("the built-in shader composes")),
            code_lines(&compose(&with_snippets, false).expect("empty snippets compose")),
            "an empty snippet adds nothing to either stage"
        );
    }

    /// The non-blank lines of a composed module, in a fixed order.
    ///
    /// The order is not the module's: the composed declarations come out of a
    /// hash map, so two compilations of the same source can list them
    /// differently. Line sets compare what a snippet adds or takes away, which
    /// is the question here.
    fn code_lines(wgsl: &str) -> Vec<&str> {
        let mut lines: Vec<&str> = wgsl
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        lines.sort_unstable();
        lines
    }

    /// The render target the test variants are built for.
    fn surface() -> SurfaceKey {
        SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: Some(wgpu::TextureFormat::Depth24PlusStencil8),
            sample_count: 4,
        }
    }

    /// A variant from its caller policy and the facts a draw carries.
    fn variant(
        options: LitOptions,
        channels: LitVertexChannels,
        base_color_texture: bool,
        morph: bool,
    ) -> LitVariant {
        LitVariant {
            options: Hashed::new(options),
            surface: surface(),
            channels,
            base_color_texture,
            morph,
            unlit: true,
            strip_index_format: None,
        }
    }

    /// The vertex channels with a compressed position and nothing else.
    fn channels(
        position: Option<ChannelEncoding>,
        joints: bool,
        secondary: SecondaryFlags,
    ) -> LitVertexChannels {
        LitVertexChannels {
            position: PositionStreamChannels { position, joints },
            secondary,
        }
    }

    /// Every vertex-channel and policy combination that composes.
    ///
    /// A sampled base-color texture implies a UV, and the joint and morph
    /// channels imply the position they deform, so the invalid combinations are
    /// skipped. Each surviving combination is then enumerated once per luminance
    /// layout, because those describe the sampled texel rather than the vertex
    /// layout and so are orthogonal to everything above — but only where a texel
    /// is sampled at all.
    ///
    /// The instance stream is not a dimension: every built-in variant binds one,
    /// so a per-instance field never has to be declared separately. The array path
    /// is not one either: it is the device's answer and is passed to
    /// [`compose`] rather than carried by the variant.
    fn all_variants() -> Vec<LitVariant> {
        // A position is absent — leaving the geometry a point at the instance
        // origin — or present in one of the three encodings.
        let positions: &[Option<ChannelEncoding>] = &[
            None,
            Some(ChannelEncoding::CompressedPosition),
            Some(ChannelEncoding::UncompressedPosition),
            Some(ChannelEncoding::PlanarPosition),
        ];
        // The secondary stream carries a UV in either encoding, a color, a
        // normal, or any combination; an uncompressed UV is still a UV.
        let secondaries: &[SecondaryFlags] = &[
            SecondaryFlags::empty(),
            SecondaryFlags::UV,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV,
            SecondaryFlags::COLOR,
            SecondaryFlags::UV | SecondaryFlags::COLOR,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
            SecondaryFlags::NORMAL,
            SecondaryFlags::UV | SecondaryFlags::NORMAL,
            SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
            SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
        ];
        let mut variants = Vec::new();
        for position in positions {
            for joints in [false, true] {
                for secondary in secondaries {
                    for base_color_texture in [false, true] {
                        for morph in [false, true] {
                            // The joint stream is part of the position stream,
                            // and a morph target displaces the position.
                            if (joints || morph) && position.is_none() {
                                continue;
                            }
                            // The base-color texture is sampled with the
                            // per-vertex UV.
                            if base_color_texture && !secondary.contains(SecondaryFlags::UV) {
                                continue;
                            }
                            let base = variant(
                                LitOptions::standard_shape(),
                                channels(*position, joints, *secondary),
                                base_color_texture,
                                morph,
                            );
                            // A texel is expanded by one of the two luminance
                            // layouts, or not at all, and only a sampled texel
                            // can be expanded.
                            let luminance_layouts: &[BaseColorChannels] = if base_color_texture {
                                &[
                                    BaseColorChannels::Rgba,
                                    BaseColorChannels::Luminance,
                                    BaseColorChannels::LuminanceAlpha,
                                ]
                            } else {
                                &[BaseColorChannels::Rgba]
                            };
                            for base_color in luminance_layouts {
                                // A cutoff compares its instance's alpha against
                                // the instance's own cutoff, so any variant can
                                // cut — textured or not.
                                for alpha_cutoff in [false, true] {
                                    let mut candidate = base.clone();
                                    candidate.options.update(|options| {
                                        options.base_color = *base_color;
                                        options.alpha_cutoff = alpha_cutoff;
                                    });
                                    variants.push(candidate);
                                }
                            }
                        }
                    }
                }
            }
        }
        variants
    }

    /// `srgb_to_linear_output` decides what the fragment writes: plain values
    /// when the inputs are already linear, a linear encoding when they are
    /// sRGB-encoded. Both variants carry the same single entry point.
    #[test]
    fn srgb_output_flag_switches_the_conversion() {
        let composed = |srgb_to_linear_output: bool| {
            let mut options = LitOptions::standard_shape();
            options.srgb_to_linear_output = srgb_to_linear_output;
            compose(
                &variant(options, LitVertexChannels::empty(), false, false),
                false,
            )
            .expect("compose")
        };

        let plain = composed(false);
        assert!(plain.contains("fn fs_main"), "one entry point either way");
        assert!(
            !plain.contains("srgb_to_linear(color.r)"),
            "a plain target stores what it is given"
        );

        let converted = composed(true);
        // Only RGB converts: alpha is coverage, not color.
        assert!(converted.contains("srgb_to_linear(color.r)"));
        assert!(converted.contains("srgb_to_linear(color.g)"));
        assert!(converted.contains("srgb_to_linear(color.b)"));
        assert!(converted.contains("color.a"), "alpha passes through");
        assert!(!converted.contains("srgb_to_linear(color.a)"));
    }

    /// The two luminance layouts say how a sampled texel expands, so they reach
    /// the fragment as the expansion itself — and only the one texel the variant
    /// samples, never the color the target writes.
    #[test]
    fn luminance_layouts_expand_a_sampled_texel() {
        let composed = |base_color: BaseColorChannels| {
            let mut options = LitOptions::standard_shape();
            options.base_color = base_color;
            compose(
                &variant(
                    options,
                    channels(
                        Some(ChannelEncoding::CompressedPosition),
                        false,
                        SecondaryFlags::UV,
                    ),
                    true,
                    false,
                ),
                false,
            )
            .expect("compose")
        };

        let full = composed(BaseColorChannels::Rgba);
        assert!(
            !full.contains("srgb_to_linear(texel.r)"),
            "a four-channel texture is uploaded in an sRGB format, so the \
             sampler decodes it"
        );

        let luma = composed(BaseColorChannels::Luminance);
        assert!(
            luma.contains("vec3<f32>(srgb_to_linear(texel.r))"),
            "a luminance texel feeds every color channel, decoded"
        );
        assert!(
            luma.contains("texel.a"),
            "a luminance texture has no alpha, so the channel reads as one"
        );

        let luma_alpha = composed(BaseColorChannels::LuminanceAlpha);
        assert!(luma_alpha.contains("vec3<f32>(srgb_to_linear(texel.r))"));
        assert!(
            luma_alpha.contains("texel.g") && !luma_alpha.contains("texel.a"),
            "the second channel of a luminance-alpha texture is its alpha, and \
             coverage never converts"
        );
    }

    /// The cutoff rides the instance stream, so the fragment that discards reads
    /// the instance attribute. Every built-in variant declares the instance
    /// stream, so the attribute is present whether or not the variant cuts.
    #[test]
    fn alpha_cutoff_flag_discards_a_fragment_below_the_cutoff() {
        let composed = |alpha_cutoff: bool| {
            let mut options = LitOptions::standard_shape();
            options.alpha_cutoff = alpha_cutoff;
            compose(
                &variant(
                    options,
                    channels(
                        Some(ChannelEncoding::CompressedPosition),
                        false,
                        SecondaryFlags::empty(),
                    ),
                    false,
                    false,
                ),
                false,
            )
            .expect("compose")
        };

        let plain = composed(false);
        assert!(
            !plain.contains("discard"),
            "an opaque material keeps every fragment it is given"
        );
        assert!(
            !plain.contains("cutoff"),
            "a variant that never cuts declares no cutoff input"
        );

        let cut = composed(true);
        let layouts = variant(
            {
                let mut options = LitOptions::standard_shape();
                options.alpha_cutoff = true;
                options
            },
            channels(
                Some(ChannelEncoding::CompressedPosition),
                false,
                SecondaryFlags::empty(),
            ),
            false,
            false,
        )
        .vertex_buffer_layouts();
        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("a built-in variant always reads the instance stream");
        assert!(
            instance
                .attributes
                .iter()
                .any(|attribute| attribute.shader_location == location::CUTOFF
                    && attribute.format == wgpu::VertexFormat::Float32),
            "the cutoff is one instance's own value, so it rides the instance stream"
        );
        assert!(
            cut.contains("@location(11)\n    cutoff: f32"),
            "the shader reads the cutoff from its instance attribute"
        );
        assert!(
            cut.contains("if color.a < in.cutoff"),
            "a fragment below the cutoff is dropped rather than blended"
        );
        assert!(cut.contains("discard"), "dropped fragments write nothing");
    }

    /// A luminance layout describes a sampled texel, so it is meaningless without
    /// one. The two layouts can no longer be selected together, because the
    /// channel count is an enum rather than two independent flags.
    #[test]
    #[should_panic(
        expected = "`BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` require \
                    a sampled base-color texture"
    )]
    fn a_luminance_layout_without_a_texture_is_rejected() {
        let mut options = LitOptions::standard_shape();
        options.base_color = BaseColorChannels::Luminance;
        let _ = compose(
            &variant(
                options,
                channels(
                    Some(ChannelEncoding::CompressedPosition),
                    false,
                    SecondaryFlags::UV,
                ),
                false,
                false,
            ),
            false,
        );
    }

    #[test]
    fn set_surface_rewrites_only_the_target() {
        let surface = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            depth_stencil_format: Some(wgpu::TextureFormat::Depth24Plus),
            sample_count: 1,
        };
        let mut options = LitOptions::standard_shape();
        let srgb_to_linear_output = options.srgb_to_linear_output;
        let base_color = options.base_color;
        let alpha_cutoff = options.alpha_cutoff;
        let primitive = options.primitive;
        let blend = options.color_target.blend;

        options.set_surface(surface);

        assert_eq!(options.color_target.format, surface.color_format);
        assert_eq!(
            options
                .depth_stencil
                .expect("a depth attachment keeps a state")
                .format,
            surface.depth_stencil_format.unwrap()
        );
        assert_eq!(options.multisample.count, surface.sample_count);

        assert_eq!(options.srgb_to_linear_output, srgb_to_linear_output);
        assert_eq!(options.base_color, base_color);
        assert_eq!(options.alpha_cutoff, alpha_cutoff);
        assert_eq!(options.primitive, primitive);
        assert_eq!(options.color_target.blend, blend);
    }

    /// A target with no depth attachment yields a pipeline with no depth state,
    /// so `wgpu`'s compatibility check against the pass cannot fail: a pass with
    /// no depth attachment rejects any pipeline that declares one, regardless of
    /// what that state compares or writes.
    #[test]
    fn a_target_without_a_depth_attachment_clears_the_base() {
        let mut options = LitOptions::standard_shape();
        assert!(
            options.depth_stencil.is_some(),
            "the base options start with a depth state"
        );

        options.set_surface(SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 1,
        });

        assert!(
            options.depth_stencil.is_none(),
            "a target without depth needs a pipeline without depth"
        );
    }

    /// A target that gains a depth attachment after the state was dropped gets
    /// the reverse-z convention back, so specialization is not one-way.
    #[test]
    fn a_target_with_a_depth_attachment_restores_the_state() {
        let mut options = LitOptions::standard_shape();
        options.set_surface(SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 1,
        });
        assert!(options.depth_stencil.is_none());

        options.set_surface(SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: Some(wgpu::TextureFormat::Depth32Float),
            sample_count: 1,
        });

        let state = options
            .depth_stencil
            .expect("the attachment brings one back");
        assert_eq!(state.format, wgpu::TextureFormat::Depth32Float);
        assert_eq!(state.depth_write_enabled, Some(true));
        assert_eq!(state.depth_compare, Some(wgpu::CompareFunction::Greater));
    }

    /// The variant is a cache key like any other: one pipeline per target,
    /// reused when the same target comes back.
    #[test]
    fn a_surface_variant_caches_one_pipeline_per_target() {
        let (device, _queue) = crate::util::test::noop_device();
        let surface = SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: Some(wgpu::TextureFormat::Depth24Plus),
            sample_count: 4,
        };
        let make = |surface: SurfaceKey| {
            let mut candidate = variant(
                LitOptions::standard(&device),
                LitVertexChannels::empty(),
                false,
                false,
            );
            candidate.surface = surface;
            candidate
        };

        let mut variants = Variants::<wgpu::RenderPipeline, LitVariant>::new(&device);
        let first = variants.specialize(make(surface));
        assert_eq!(
            variants.specialize(make(surface)),
            first,
            "the same target reuses its pipeline"
        );

        // A different color format is a different target, so it compiles its own
        // variant rather than reusing the first one.
        let other_format = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            ..surface
        };
        assert_ne!(variants.specialize(make(other_format)), first);
        assert_eq!(
            variants.get(first).descriptor().surface.color_format,
            surface.color_format
        );
    }

    #[test]
    fn vertex_channels_for_vertex_buffer_maps_each_slot() {
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
        let channels = |slot, layout: &VertexBufferLayoutDesc| {
            LitVertexChannels::for_vertex_buffer(slot, layout)
        };
        let position_of =
            |slot, layout: &VertexBufferLayoutDesc| channels(slot, layout).position.position;

        // The position slot: presence, and the compressed/uncompressed split.
        let compressed = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION)],
        );
        assert_eq!(
            position_of(PRIMARY_SLOT, &compressed),
            Some(ChannelEncoding::CompressedPosition)
        );
        let uncompressed = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Float32x3, location::POSITION)],
        );
        assert_eq!(
            position_of(PRIMARY_SLOT, &uncompressed),
            Some(ChannelEncoding::UncompressedPosition)
        );

        // The joint pair rides the position stream, so it is this slot's answer
        // rather than a slot of its own.
        let skinned_layout = layout(
            wgpu::VertexStepMode::Vertex,
            &[
                attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION),
                attribute(wgpu::VertexFormat::Uint32, location::JOINTS),
            ],
        );
        let skinned = channels(PRIMARY_SLOT, &skinned_layout);
        assert_eq!(
            skinned.position.position,
            Some(ChannelEncoding::CompressedPosition)
        );
        assert!(
            skinned.position.joints,
            "the joint indices are the evidence"
        );

        // The secondary slot: each channel is independent.
        let secondary = layout(
            wgpu::VertexStepMode::Vertex,
            &[
                attribute(wgpu::VertexFormat::Snorm16x2, location::UV),
                attribute(wgpu::VertexFormat::Unorm8x4, location::COLOR),
                attribute(wgpu::VertexFormat::Snorm16x2, location::NORMAL),
            ],
        );
        assert_eq!(
            channels(SECONDARY_SLOT, &secondary).secondary,
            SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL
        );
        let normal_only = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Snorm16x2, location::NORMAL)],
        );
        assert_eq!(
            channels(SECONDARY_SLOT, &normal_only).secondary,
            SecondaryFlags::NORMAL
        );
        let uncompressed_uv = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Float32x2, location::UV)],
        );
        assert_eq!(
            channels(SECONDARY_SLOT, &uncompressed_uv).secondary,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV
        );

        // The instance slot contributes nothing: the per-instance record is not
        // a vertex channel.
        let instance = layout(
            wgpu::VertexStepMode::Instance,
            &[attribute(
                wgpu::VertexFormat::Uint32,
                location::METADATA_INDEX,
            )],
        );
        assert_eq!(
            channels(INSTANCE_SLOT, &instance),
            LitVertexChannels::empty(),
            "the per-instance record is not a vertex channel"
        );

        // A whole layout is the union of its slots, and a slot that is not one of
        // the pipeline's own contributes nothing either way.
        let foreign = layout(
            wgpu::VertexStepMode::Vertex,
            &[attribute(wgpu::VertexFormat::Float32x3, 9)],
        );
        let whole = LitVertexChannels::for_vertex_layout(&[
            (PRIMARY_SLOT, skinned_layout),
            (SECONDARY_SLOT, uncompressed_uv),
            (INSTANCE_SLOT, instance),
            (7, foreign),
        ]);
        assert_eq!(
            whole.position.position,
            Some(ChannelEncoding::CompressedPosition)
        );
        assert!(whole.position.joints);
        assert_eq!(
            whole.secondary,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV
        );
    }

    #[test]
    fn composes_each_variant_to_valid_wgsl() {
        for mut candidate in all_variants() {
            // Lighting is the one dimension the variant's channels do not already
            // cover: a lit draw reads the frame's lights and clusters with them,
            // which is why both are compiled here.
            for unlit in [false, true] {
                candidate.unlit = unlit;
                // The array path is the device's, so both are exercised here.
                for texel_arrays in [false, true] {
                    let wgsl = compose(&candidate, texel_arrays)
                        .unwrap_or_else(|error| panic!("variant {candidate:?} failed: {error}"));
                    assert!(wgsl.contains("fn vs_main"), "variant {candidate:?}");
                    assert!(wgsl.contains("fn fs_main"), "variant {candidate:?}");
                    // Disabled features must leave no conditional attributes behind.
                    assert!(!wgsl.contains("@if"), "variant {candidate:?} kept @if");

                    let vertex = candidate.channels;
                    // Each channel appears exactly when the geometry carries it.
                    assert_eq!(
                        wgsl.contains("base_color_tex"),
                        candidate.base_color_texture,
                        "variant {candidate:?}"
                    );
                    let compressed_uv = vertex.secondary.contains(SecondaryFlags::UV)
                        && !vertex.secondary.contains(SecondaryFlags::UNCOMPRESSED_UV);
                    assert_eq!(
                        wgsl.contains("decode_uv"),
                        compressed_uv,
                        "variant {candidate:?}"
                    );
                    let compressed_position = matches!(
                        vertex.position.position,
                        Some(ChannelEncoding::CompressedPosition)
                    );
                    assert_eq!(
                        wgsl.contains("decode_position"),
                        compressed_position,
                        "variant {candidate:?}"
                    );
                    // A normal is decoded only where it shades, and only a lit
                    // fragment shades with one.
                    assert_eq!(
                        wgsl.contains("decode_normal"),
                        !candidate.unlit && vertex.secondary.contains(SecondaryFlags::NORMAL),
                        "variant {candidate:?}"
                    );
                    // The light arrays, the cluster lookup and the shading terms are
                    // a lit draw's own.
                    assert_eq!(
                        wgsl.contains("fragment_cluster_index"),
                        !candidate.unlit,
                        "variant {candidate:?}"
                    );
                    assert_eq!(
                        wgsl.contains("load_cell_word") && wgsl.contains("load_light"),
                        !candidate.unlit,
                        "variant {candidate:?}"
                    );
                    assert_eq!(
                        wgsl.contains("polygon_form_factor"),
                        !candidate.unlit,
                        "variant {candidate:?}"
                    );
                    // Every array read goes through an accessor, whose name survives
                    // assembly unmangled as a suffix: the binding it reads is mangled
                    // with its module path, so the accessor is what a variant can be
                    // checked against.
                    assert_eq!(
                        wgsl.contains("load_mesh_metadata"),
                        candidate.needs_metadata(),
                        "variant {candidate:?}"
                    );
                    // The pose arrays are the frame's, so they live in the global
                    // group alongside the camera and the metadata.
                    assert_eq!(
                        wgsl.contains("load_joint_matrix"),
                        candidate.needs_joints(),
                        "variant {candidate:?}"
                    );
                    assert_eq!(
                        wgsl.contains("load_morph_weight"),
                        candidate.needs_morphs(),
                        "variant {candidate:?}"
                    );
                    assert_eq!(
                        wgsl.contains("load_morph_delta"),
                        candidate.needs_morphs(),
                        "variant {candidate:?}"
                    );
                    // The path decides the resource type: storage buffers when the
                    // device has them, textures when it does not. Neither path ever
                    // leaves the other's declarations behind.
                    let reads_an_array =
                        candidate.needs_metadata() || candidate.needs_joints() || !candidate.unlit;
                    if texel_arrays {
                        assert_eq!(
                            wgsl.matches("var<storage, read>").count(),
                            0,
                            "variant {candidate:?} kept a storage array"
                        );
                    }
                    assert_eq!(
                        wgsl.contains("texel_of"),
                        texel_arrays && reads_an_array,
                        "variant {candidate:?}"
                    );
                    // Without a position stream the vertex input declares no position
                    // attribute, so the shader cannot read one.
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
                        vertex.position.position.is_some(),
                        "variant {candidate:?}"
                    );
                    assert_eq!(
                        vertex_input.contains("joints:") && vertex_input.contains("weights:"),
                        candidate.needs_joints(),
                        "variant {candidate:?}"
                    );
                    assert_eq!(
                        vertex_input.contains("normal:"),
                        vertex.secondary.contains(SecondaryFlags::NORMAL),
                        "variant {candidate:?}"
                    );
                    // The instance record is unconditional, so its fields are
                    // declared as plain attributes with no condition on them.
                    assert!(
                        vertex_input.contains("joints_base:"),
                        "variant {candidate:?} declares no instance stream"
                    );
                    assert!(
                        vertex_input.contains("morph_base:"),
                        "variant {candidate:?} declares no instance stream"
                    );
                }
            }
        }
    }

    /// A variant that contradicts itself must be rejected rather than composed
    /// into a shader that cannot work.
    ///
    /// The impossible states are no longer flags disagreeing with each other: an
    /// encoding is a property of the channel it encodes, and the instance stream
    /// is always bound, so the rejections left are the ones where a resource is
    /// read that the variant does not carry — a texture sampled without a UV, a
    /// deformation without a position, and a luminance layout without a texel.
    #[test]
    fn contradictory_variants_are_rejected() {
        let shape = LitOptions::standard_shape;
        let compressed = Some(ChannelEncoding::CompressedPosition);
        let mut luminance_without_a_texture = shape();
        luminance_without_a_texture.base_color = BaseColorChannels::Luminance;
        for candidate in [
            // The base-color texture is sampled with the per-vertex UV.
            variant(
                shape(),
                channels(compressed, false, SecondaryFlags::empty()),
                true,
                false,
            ),
            // A deformation displaces the position.
            variant(
                shape(),
                channels(None, true, SecondaryFlags::empty()),
                false,
                false,
            ),
            variant(
                shape(),
                channels(None, false, SecondaryFlags::empty()),
                false,
                true,
            ),
            // A luminance layout describes a texel that is sampled.
            variant(
                luminance_without_a_texture,
                channels(compressed, false, SecondaryFlags::UV),
                false,
                false,
            ),
        ] {
            let result = std::panic::catch_unwind(|| compose(&candidate, false));
            assert!(result.is_err(), "variant {candidate:?} must be rejected");
        }
    }

    #[test]
    fn vertex_strides_follow_the_attribute_formats() {
        let layouts = variant(
            LitOptions::standard_shape(),
            channels(
                Some(ChannelEncoding::CompressedPosition),
                false,
                SecondaryFlags::UV,
            ),
            false,
            false,
        )
        .vertex_buffer_layouts();
        let position = layouts[PRIMARY_SLOT as usize]
            .as_ref()
            .expect("position slot");
        assert_eq!(position.array_stride, wgpu::VertexFormat::Snorm16x4.size());

        let secondary = layouts[SECONDARY_SLOT as usize].as_ref().expect("uv slot");
        assert_eq!(secondary.array_stride, wgpu::VertexFormat::Snorm16x2.size());

        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot");
        // Every variant reads the same instance buffer, so the stride is the
        // whole record even when the variant declares only some of its fields.
        assert_eq!(
            instance.array_stride,
            std::mem::size_of::<crate::mesh::MeshInstance>() as u64
        );
        assert_eq!(instance.step_mode, wgpu::VertexStepMode::Instance);
    }

    /// An uncompressed channel widens its slot to the full-precision format and
    /// drops the metadata bindings it no longer needs — but the instance stream
    /// is still bound, because every built-in variant binds one.
    #[test]
    fn uncompressed_channels_widen_their_slots() {
        let candidate = variant(
            LitOptions::standard_shape(),
            channels(
                Some(ChannelEncoding::UncompressedPosition),
                false,
                SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
            ),
            true,
            false,
        );
        assert!(!candidate.needs_metadata());

        let layouts = candidate.vertex_buffer_layouts();
        let position = layouts[PRIMARY_SLOT as usize]
            .as_ref()
            .expect("position slot");
        assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x3.size());
        assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x3);

        let secondary = layouts[SECONDARY_SLOT as usize].as_ref().expect("uv slot");
        assert_eq!(
            secondary.array_stride,
            wgpu::VertexFormat::Float32x2.size() + wgpu::VertexFormat::Unorm8x4.size()
        );

        // The instance stream is bound for every variant, so the slot exists even
        // though these channels are read entirely from full-precision attributes.
        assert!(layouts[INSTANCE_SLOT as usize].is_some());
    }

    /// A planar channel narrows its slot to the two-component format and needs
    /// no metadata either, exactly as any other full-precision position.
    #[test]
    fn planar_channels_narrow_their_slots() {
        let candidate = variant(
            LitOptions::standard_shape(),
            channels(
                Some(ChannelEncoding::PlanarPosition),
                false,
                SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
            ),
            true,
            false,
        );
        assert!(!candidate.needs_metadata());

        let layouts = candidate.vertex_buffer_layouts();
        let position = layouts[PRIMARY_SLOT as usize]
            .as_ref()
            .expect("position slot");
        assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x2.size());
        assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x2);
    }

    #[test]
    fn secondary_slot_follows_its_channels() {
        let stride = |secondary: SecondaryFlags| {
            variant(
                LitOptions::standard_shape(),
                channels(None, false, secondary),
                false,
                false,
            )
            .vertex_buffer_layouts()[SECONDARY_SLOT as usize]
                .as_ref()
                .map(|layout| layout.array_stride)
        };

        let uv = wgpu::VertexFormat::Snorm16x2.size();
        let uv_uncompressed = wgpu::VertexFormat::Float32x2.size();
        let color = wgpu::VertexFormat::Unorm8x4.size();
        let normal = wgpu::VertexFormat::Snorm16x2.size();

        // No channel: the slot disappears entirely, so a variant never binds a
        // buffer the shader does not declare.
        assert_eq!(stride(SecondaryFlags::empty()), None);
        assert_eq!(stride(SecondaryFlags::UV), Some(uv));
        assert_eq!(
            stride(SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV),
            Some(uv_uncompressed)
        );
        assert_eq!(stride(SecondaryFlags::COLOR), Some(color));
        assert_eq!(
            stride(SecondaryFlags::UV | SecondaryFlags::COLOR),
            Some(uv + color)
        );
        assert_eq!(
            stride(SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR),
            Some(uv_uncompressed + color)
        );
        // A normal is always the same two lanes, whatever else the stream
        // carries, and it sits in attribute order last.
        assert_eq!(stride(SecondaryFlags::NORMAL), Some(normal));
        assert_eq!(
            stride(SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL),
            Some(uv + color + normal)
        );
    }

    /// The position slot follows its channels, so a position-less variant binds
    /// no position buffer at all.
    #[test]
    fn position_slot_follows_its_channels() {
        let position = |position: Option<ChannelEncoding>, joints: bool| {
            variant(
                LitOptions::standard_shape(),
                channels(position, joints, SecondaryFlags::empty()),
                false,
                false,
            )
            .vertex_buffer_layouts()[PRIMARY_SLOT as usize]
                .as_ref()
                .map(|layout| (layout.array_stride, layout.step_mode))
        };

        assert_eq!(
            position(None, false),
            None,
            "no position stream means no position slot"
        );
        assert_eq!(
            position(Some(ChannelEncoding::CompressedPosition), false),
            Some((
                wgpu::VertexFormat::Snorm16x4.size(),
                wgpu::VertexStepMode::Vertex
            ))
        );
        assert_eq!(
            position(Some(ChannelEncoding::UncompressedPosition), false),
            Some((
                wgpu::VertexFormat::Float32x3.size(),
                wgpu::VertexStepMode::Vertex
            ))
        );
    }

    /// The instance slot's GPU attribute offsets and stride must match the CPU
    /// struct. This is the vertex-buffer equivalent of the shader-layout
    /// validation that `const_shader_layout` gives the uniform and storage
    /// structs: vertex attributes are addressed by the offsets declared here, not
    /// by WGSL alignment rules.
    #[test]
    fn instance_vertex_layout_matches_the_struct() {
        use crate::mesh::MeshInstance;
        use core::mem::{offset_of, size_of};

        let layouts = variant(
            LitOptions::standard_shape(),
            channels(
                Some(ChannelEncoding::CompressedPosition),
                false,
                SecondaryFlags::empty(),
            ),
            false,
            false,
        )
        .vertex_buffer_layouts();
        let instance = layouts[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot");

        assert_eq!(
            instance.array_stride,
            size_of::<MeshInstance>() as u64,
            "the instance stride must step exactly one MeshInstance"
        );

        let model = offset_of!(MeshInstance, model) as u64;
        let vec4 = size_of::<[f32; 4]>() as u64;
        // The record has to be tightly packed: `IntoBytes` rejects a type with
        // padding, so the sum of the record's field formats is the whole
        // stride. This variant cuts nothing, so its layout omits the cutoff
        // the shader would not declare; the declared formats plus that omitted
        // word still cover the record exactly.
        let declared: u64 = instance
            .attributes
            .iter()
            .map(|attribute| attribute.format.size())
            .sum();
        assert_eq!(
            declared + wgpu::VertexFormat::Float32.size(),
            instance.array_stride,
            "the instance record carries no padding"
        );

        let expected = [
            model,
            model + vec4,
            model + 2 * vec4,
            offset_of!(MeshInstance, base_color) as u64,
            offset_of!(MeshInstance, joints_base) as u64,
            offset_of!(MeshInstance, metadata_index) as u64,
            offset_of!(MeshInstance, morph_base) as u64,
        ];
        let actual: Vec<u64> = instance.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(
            actual, expected,
            "attribute offsets must line up with the MeshInstance fields"
        );

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
                location::JOINTS_BASE,
                location::METADATA_INDEX,
                location::MORPH_BASE,
            ]
        );
        // The matrix columns are floats, the color is quantized, and the two
        // bases and the metadata index are integers: each addresses a slot in
        // one of the frame's shared arrays.
        let expected_formats = [
            wgpu::VertexFormat::Float32x4,
            wgpu::VertexFormat::Float32x4,
            wgpu::VertexFormat::Float32x4,
            wgpu::VertexFormat::Unorm8x4,
            wgpu::VertexFormat::Uint32,
            wgpu::VertexFormat::Uint32,
            wgpu::VertexFormat::Uint32,
        ];
        let formats: Vec<wgpu::VertexFormat> =
            instance.attributes.iter().map(|a| a.format).collect();
        assert_eq!(formats, expected_formats, "{instance:?}");
    }

    /// A cutting variant's instance layout declares the cutoff; a plain one
    /// omits it, matching the shader's own conditional input declaration. The
    /// other instance inputs are unconditional in the shader, so the layout
    /// declares them for every variant.
    #[test]
    fn instance_layout_declares_the_cutoff_only_when_it_cuts() {
        let locations = |alpha_cutoff: bool| {
            let mut options = LitOptions::standard_shape();
            options.alpha_cutoff = alpha_cutoff;
            variant(
                options,
                channels(None, false, SecondaryFlags::COLOR),
                false,
                false,
            )
            .vertex_buffer_layouts()[INSTANCE_SLOT as usize]
                .as_ref()
                .expect("instance slot")
                .attributes
                .iter()
                .map(|a| a.shader_location)
                .collect::<Vec<u32>>()
        };

        let plain = [
            location::MODEL_0,
            location::MODEL_1,
            location::MODEL_2,
            location::BASE_COLOR,
            location::JOINTS_BASE,
            location::METADATA_INDEX,
            location::MORPH_BASE,
        ];
        let mut cutting = plain.to_vec();
        cutting.insert(6, location::CUTOFF);

        assert_eq!(
            locations(false),
            plain,
            "a plain variant declares no cutoff"
        );
        assert_eq!(
            locations(true),
            cutting,
            "a cutting variant declares the cutoff"
        );
    }
}
