use super::*;

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
    /// A caller's module composes the stages out of the same package modules the
    /// built-in one imports, and nothing else changes: the layouts, vertex
    /// streams and entry-point names stay the variant's. See [`Shader`].
    pub shader: Option<Shader>,
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
    pub(super) fn state(self, format: wgpu::TextureFormat) -> wgpu::DepthStencilState {
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
