//! The built-in lit pipeline and the WESL composition behind it.
//!
//! [`SpecializedLitPipeline`] composes an [`LitVariant`]'s entry shader —
//! the built-in `lit.wesl`, or the caller's own when
//! [`LitOptions::shader`] carries one — builds the layouts the shader
//! expects, and exposes the vertex-buffer layouts the caller declares when
//! creating meshes. Each variant contains exactly the bindings and attributes
//! the selected channels need, so nothing unused reaches the GPU.

use crate::mesh::{
    ChannelEncoding, PositionStreamChannels, PositionStreamWriter, SecondaryFlags,
    SecondaryStreamWriter,
};
use crate::specialize::{PipelineDescriptor, PipelineVariant, SurfaceKey, VertexBufferLayoutDesc};
use crate::util::Hashed;
use std::sync::Arc;

/// Binding slot of the camera uniform in the global bind group.
pub const CAMERA_BINDING: u32 = 0;
/// Binding slot of the frame-globals uniform in the global bind group.
pub const FRAME_BINDING: u32 = 1;
/// Binding slot of the pass's view uniform in the global bind group.
///
/// It carries the region of the target the pass draws through, which is what a
/// shader measuring in pixels divides by. The camera and this differ in who
/// owns them: a camera is the frame's, a view is the pass's, so one camera drawn
/// through two viewports is two views.
pub const VIEW_BINDING: u32 = 2;
/// Binding slot of the mesh-metadata storage buffer in the global bind group.
pub const MESH_METADATA_BINDING: u32 = 3;
/// Binding slot of the frame's joint matrices in the global bind group.
///
/// The array holds the pose of every visible instance, so it is per-frame data
/// the source packs rather than anything a mesh owns; an instance's own slice
/// starts at [`crate::mesh::MeshInstance::joints_base`].
pub const JOINTS_BINDING: u32 = 4;
/// Binding slot of the frame's morph weights in the global bind group.
///
/// Like the joint matrices this is one array for the whole frame, and an
/// instance's own slice starts at [`crate::mesh::MeshInstance::morph_base`].
pub const MORPH_WEIGHTS_BINDING: u32 = 5;
/// Binding slot of the frame's morph position displacements in the global bind
/// group.
///
/// A mesh's displacements are its own geometry, but one array holds them for
/// the whole frame and the mesh names its slice through the `morph_deltas_offset`
/// of its [`MeshMetadata`](crate::mesh::MeshMetadata) entry. Pooling them here
/// rather than in a per-mesh buffer is what lets a draw bind no mesh group.
pub const MORPH_DELTAS_BINDING: u32 = 6;
/// Binding slot of the frame's lights in the global bind group.
///
/// One record per light, in view space, packed by the source every frame. The
/// fragment stage reads it, so it is not visible to a vertex stage.
pub const LIGHTS_BINDING: u32 = 7;
/// Binding slot of the frame's cluster cells in the global bind group.
///
/// Each cell's record lists the lights assigned to it; see
/// [`crate::light::CELL_WORDS`].
pub const LIGHT_CELLS_BINDING: u32 = 8;
/// Binding slot of the frame's polygon-light vertices in the global bind group.
///
/// A light whose shape has corners names its slice through its record's
/// `kind.y` and `kind.z`.
pub const LIGHT_VERTICES_BINDING: u32 = 9;
/// Binding slot of the cluster uniform in the global bind group.
///
/// The cluster grid's shape and depth mapping: what a fragment needs to find the
/// cell it is in.
pub const CLUSTER_BINDING: u32 = 10;
/// Binding slot of the frame-wide lighting uniform in the global bind group.
///
/// The ambient term and the frame's directional lights, which reach every
/// fragment: what a fragment needs to light itself before it asks the grid.
pub const LIGHTING_BINDING: u32 = 11;
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
/// The built-in lit variants bind nothing here: every input they read is in
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
    /// Per-instance inverted-hull width (`Float32`), read from the instance
    /// stream.
    ///
    /// The distance the shader pushes a vertex out along its normal. An
    /// [`LitOptions::expanded`](crate::pipeline::LitOptions::expanded) variant
    /// reads it; every other variant leaves the field unread, and an instance
    /// that is not a hull carries zero.
    pub const HULL_WIDTH: u32 = 14;
}

/// Entry point name of the built-in shader's vertex stage.
pub const VS_MAIN: &str = "vs_main";
/// Entry point name of the built-in shader's fragment stage.
pub const FS_MAIN: &str = "fs_main";
/// Label of the built-in entry shader: its shader module's label, and the name
/// a composition error is reported under.
const BUILT_IN_SHADER_LABEL: &str = "unlit_wgpu::lit";

/// Name of the WESL module the built-in entry shader is composed from: the file
/// `shaders/lit.wesl` of this crate's package.
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
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LitShader {
    label: Arc<str>,
    module: Arc<str>,
    vertex: Arc<str>,
    fragment: Arc<str>,
}

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

/// The policy a caller decides about a built-in lit pipeline.
///
/// Everything a draw resolves is absent: the vertex channels, whether the draw
/// samples a material, whether its mesh morphs, the render target and the
/// device's array path are facts a family folds into an [`LitVariant`] when
/// it resolves a draw, not choices a caller can make. The target owns the
/// color format, the sample count and whether there is a depth attachment at
/// all; what is left is what a caller actually picks.
///
/// [`LitOptions::standard`] is the usual starting point: back-face culling,
/// a filtering base-color texture binding and reverse-z depth.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LitOptions {
    /// The entry shader this policy draws with, or `None` for the built-in
    /// `lit.wesl`.
    ///
    /// A caller's snippets extend the built-in source and nothing else: the
    /// layouts, vertex streams and entry-point names stay the variant's. See
    /// [`LitShader`].
    pub shader: Option<LitShader>,
    /// Whether the fragment stage returns its base color unchanged instead of
    /// multiplying it by the frame's lights.
    ///
    /// An unlit variant declares none of the lighting bindings, so the lights a
    /// source clusters cost it nothing; a lit one reads the light records, the
    /// cell array, the polygon vertices and the cluster uniform from the global
    /// group. Lighting is the common case, so this states the exception and
    /// [`Self::standard`] leaves it off.
    pub unlit: bool,
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
    /// Push every vertex out along its normal by the instance's hull width.
    ///
    /// This is the geometry half of the inverted-hull outline: a mesh drawn
    /// with this variant expands into a shell, and a mesh drawn behind it with
    /// the ordinary front-facing variant covers the shell except where it
    /// pokes out past the silhouette, which is the line. Both halves draw the
    /// same mesh with the same buffers — the shell reads the very same vertex
    /// and index streams — so an outline costs one extra draw and no second
    /// mesh.
    ///
    /// The width rides the instance stream
    /// ([`MeshInstance::hull_width`](crate::mesh::MeshInstance::hull_width)),
    /// so two instances of one mesh can be shells of different thicknesses,
    /// and a mesh that is not a hull carries zero. A variant with this set
    /// reads the mesh's normals and declares the
    /// [`HULL_WIDTH`](crate::pipeline::location::HULL_WIDTH) attribute, which
    /// a mesh with no normal channel cannot supply — without a normal there is
    /// no direction to expand along.
    ///
    /// Expansion happens in the vertex stage, so it moves the clip position,
    /// the world position the lights are addressed from and the shading normal
    /// together. Which faces are drawn is the primitive state's business:
    /// [`LitOptions::inverted_hull`] culls the front and writes no depth,
    /// which is what leaves the ordinary variant's nearer surface in front.
    pub expanded: bool,
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
    /// built-in lit family fills it from the mesh's index buffer, so the
    /// field is a caller's concern only for a pipeline a mesh does not
    /// describe.
    pub primitive: wgpu::PrimitiveState,
    /// How the pipeline tests and writes the depth attachment, for a target
    /// that has one.
    ///
    /// A target without a depth attachment runs the pipeline without a
    /// depth-stencil state at all, whatever this field says, because `wgpu`
    /// rejects a pipeline that declares one against a pass that has none. The
    /// format is the attachment's own, which the options cannot know: it is
    /// the target's word, and the variant carries the target.
    ///
    /// [`Self::standard`] states the renderer's reverse-z convention: depth
    /// is cleared to the far plane, so nearer geometry carries the greater
    /// value.
    pub depth_stencil: DepthStencilPolicy,
    /// The blend state the fragment output composites with, or `None` to
    /// write it unblended.
    pub blend: Option<wgpu::BlendState>,
    /// Which color channels the pipeline writes to the target.
    pub write_mask: wgpu::ColorWrites,
}

/// The depth-test policy of a lit pipeline, without the attachment's format.
///
/// The format belongs to the target a pipeline is specialized for
/// ([`SurfaceKey::depth_stencil_format`]), not to the policy: one policy
/// serves every target, and the variant's descriptor fills the format in when
/// it builds the pipeline. The options could not carry it and stay a
/// policy — a pipeline built for one attachment's format cannot bind against a
/// pass recording into another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthStencilPolicy {
    /// Whether the pipeline writes the depth of the fragments it draws, or
    /// leaves the attachment's records to the draws that wrote them.
    pub depth_write_enabled: bool,
    /// The comparison a fragment's depth passes against the attachment's
    /// record with.
    pub depth_compare: wgpu::CompareFunction,
}

impl Default for DepthStencilPolicy {
    /// The renderer's reverse-z convention: depth is cleared to the far plane,
    /// so nearer geometry carries the greater value, and writes its own.
    fn default() -> Self {
        Self {
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Greater,
        }
    }
}

impl DepthStencilPolicy {
    /// The wgpu state this policy names, recording into a depth attachment of
    /// `format`.
    fn state(self, format: wgpu::TextureFormat) -> wgpu::DepthStencilState {
        wgpu::DepthStencilState {
            format,
            depth_write_enabled: Some(self.depth_write_enabled),
            depth_compare: Some(self.depth_compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }
    }
}

impl LitOptions {
    /// The usual starting point: the full compressed mesh variant — positions,
    /// UVs, vertex colors, a base-color texture and per-instance transforms —
    /// with reverse-z depth.
    ///
    /// The policy names no attachment format: the target the variant is
    /// specialized for does. Back faces are culled, because the geometry this
    /// variant is for is closed meshes wound counter-clockwise, whose insides
    /// are never meant to show.
    pub fn standard() -> Self {
        Self {
            shader: None,
            unlit: false,
            srgb_to_linear_output: false,
            base_color: BaseColorChannels::Rgba,
            alpha_cutoff: false,
            expanded: false,
            texture_filtering: true,
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: DepthStencilPolicy::default(),
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        }
    }

    /// The outline shell that goes behind a mesh drawn with [`Self::standard`].
    ///
    /// The inverted-hull trick draws the same mesh twice: once as itself, and
    /// once expanded along its normals, with its front faces culled and its
    /// backwards-facing surface — which is now the outside of a slightly
    /// larger copy — drawn where the real mesh does not cover it. What shows
    /// is a band the width of the expansion, all the way around the
    /// silhouette, including the silhouette's own inner folds, which a
    /// per-triangle line cannot find.
    ///
    /// The returned options differ from the standard ones in what makes the
    /// shell a shell rather than a copy:
    ///
    /// - [`Self::expanded`] is set, so the vertex stage pushes each vertex out
    ///   along its normal by the instance's hull width.
    /// - The front faces are culled, so the far side of the shell is what
    ///   covers the near side of the mesh.
    /// - Depth is not written, so the shell leaves the depth buffer to the
    ///   meshes drawn around it — the surface it belongs to still occludes and
    ///   is occluded exactly as if the shell were not there.
    /// - It is unlit, so the band is one flat color rather than a shaded copy
    ///   of the mesh, and it declares none of the lighting bindings.
    ///
    /// The expansion width and the band's color are per-instance values, so
    /// these options cannot hold them: the shell entity carries them as its
    /// [`MeshInstance::hull_width`](crate::mesh::MeshInstance::hull_width) and
    /// its base color, which is the same record every other instance is packed
    /// from.
    ///
    /// Both options hash into the same `LitPipelineKey`, so the pair
    /// compiles to two pipelines and needs no family of its own, no second
    /// mesh and no second pass: the two draws sort against each other by depth
    /// like any others, and the shell's unwritten depth is what keeps the
    /// order between them stable.
    pub fn inverted_hull() -> Self {
        Self {
            unlit: true,
            expanded: true,
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: DepthStencilPolicy {
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Greater,
            },
            ..Self::standard()
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

/// The complete identity of a built-in lit pipeline, and its blueprint.
///
/// A family resolves one from an entity's [`LitOptions`] and the facts its
/// draw carries: the mesh's vertex channels, whether the draw binds a material,
/// whether the mesh morphs, the mesh's index format and the render target. Two
/// draws that agree on all of it share one compiled pipeline; because the
/// variant is the complete description of the blueprint, the cache key is
/// canonical by construction.
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
}

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
    fn features(&self, texel_arrays: bool) -> [(&'static str, bool); 17] {
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
            ("EXPANDED", self.options.expanded),
            ("LIT", !self.options.unlit),
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
        let reads_an_array = self.needs_metadata() || self.needs_joints() || !self.options.unlit;
        assert!(
            !reads_an_array || texel_arrays || supports_storage_buffers(device),
            "this variant reads its arrays from storage buffers, but the device has none \
             (its `max_storage_buffers_per_shader_stage` is 0, as WebGL2's is): compose \
             the variant with `texel_arrays` set, the way the built-in family does"
        );
    }
}

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
            lights: !self.options.unlit,
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
    /// unconditionally, the alpha cutoff and the hull width under the features
    /// that declare them.
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
        // The cutoff is conditional: the shader declares it only under
        // ALPHA_CUTOFF, so a variant that never cuts must not declare it
        // either — the validation layers flag an attribute the vertex stage
        // never touches.
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
        // The other conditional instance input, on the same terms: only an
        // expanding variant's shader declares the hull width.
        if self.options.expanded {
            fields.push((
                location::HULL_WIDTH,
                wgpu::VertexFormat::Float32,
                field(core::mem::offset_of!(crate::mesh::MeshInstance, hull_width)),
            ));
        }
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

impl PipelineVariant<wgpu::RenderPipeline> for LitVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        self.clone()
    }
}

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
        let wgsl =
            compose_shader(self, texel_arrays).unwrap_or_else(|error| panic!("{label}: {error}"));
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
        // rather than the caller. The policy is read field by field rather
        // than rewritten in place: the options are keyed on, and the target is
        // applied after hashing rather than into it.
        let options = &*self.options;
        let depth_stencil = self
            .surface
            .depth_stencil_format
            .map(|format| options.depth_stencil.state(format));
        let color_target = wgpu::ColorTargetState {
            format: self.surface.color_format,
            blend: options.blend,
            write_mask: options.write_mask,
        };

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
            primitive: {
                let mut primitive = options.primitive;
                primitive.strip_index_format = self.strip_index_format;
                primitive
            },
            depth_stencil,
            multisample: wgpu::MultisampleState {
                count: self.surface.sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(FS_MAIN),
                compilation_options: Default::default(),
                targets: &[Some(color_target)],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
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
fn compose_shader(
    variant: &LitVariant,
    texel_arrays: bool,
) -> Result<String, crate::compose::ComposeError> {
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
    assert!(
        !variant.options.expanded || variant.channels.secondary.contains(SecondaryFlags::NORMAL),
        "an expanding variant pushes the vertex along its normal, so `EXPANDED` requires a normal channel"
    );

    let features = variant.features(texel_arrays);
    let replacement = variant.options.shader.as_ref().map(|shader| {
        crate::compose::splice(
            crate::shader::lit::MODULE.source,
            shader.module(),
            shader.vertex(),
            shader.fragment(),
        )
    });
    crate::compose::compose(BUILT_IN_MODULE, &features, replacement.as_deref())
}

/// Which of a source's global buffers a pipeline reads.
///
/// The camera, frame-globals and view uniforms are always bound; the arrays are
/// optional and a pipeline declares only the ones it reads. A caller's own
/// pipeline passes the same description to `global_bind_group_layout` as the
/// built-in lit variants do, so binding the frame's shared inputs is not a
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
/// The camera, frame-globals and view uniforms are always present; the arrays
/// are present exactly when `bindings` names them. The array path is the
/// device's answer rather than the pipeline's, so `texel_arrays` is passed in: a
/// device with storage buffers binds each array as one, a device without them —
/// WebGL2 — binds the same bytes as a texture.
///
/// The built-in lit variants build their global layout here, so a caller's own
/// pipeline declares the same group by the same rules.
pub fn global_bind_group_layout(
    device: &wgpu::Device,
    bindings: GlobalBindings,
    texel_arrays: bool,
) -> wgpu::BindGroupLayout {
    let mut entries = arrayvec::ArrayVec::<wgpu::BindGroupLayoutEntry, 12>::new();
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
                <crate::globals::Camera as const_shader_layout::ShaderLayout>::SIZE,
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
    // The pass's view. Every pipeline declares it beside the camera and the
    // frame clock, because the group is the frame's shared one and the viewport
    // belongs to it as much as the other two: a pipeline that never measures in
    // pixels simply does not name the binding in its shader.
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: VIEW_BINDING,
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

/// The built-in lit pipeline: a compiled [wgpu::RenderPipeline] together
/// with the [`LitVariant`] it was built from.
///
/// [`LitVariant`] is its own [`PipelineVariant`] and [`PipelineDescriptor`],
/// so the built-in shader compiles through the same cache a caller's own
/// variant does, and the layouts it declares are derived from the variant
/// rather than stored beside the pipeline.
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
pub fn supports_storage_buffers(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    limits.max_storage_buffers_per_shader_stage > 0 && limits.max_storage_buffer_binding_size > 0
}

#[cfg(test)]
mod tests;
