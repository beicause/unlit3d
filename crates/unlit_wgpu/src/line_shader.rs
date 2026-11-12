//! The caller's own entry shader, composed in place of the built-in `line.wesl`.
//!
//! The built-in stroke shader is a pair of entry points over the package's stage
//! modules, so a caller composes its own stroke rather than text being inserted
//! into a source it cannot see: a module that imports
//! [`shader::line_vertex`](crate::shader) and
//! [`shader::line_fragment`](crate::shader) calls the parts it keeps and replaces
//! the rest. It reaches the same bindings, the same instance record and the same
//! helpers the built-in shader does, under the variant's conditional-compilation
//! flags, and its diagnostics point at the line it was written on.
//!
//! The parts a stroke is built from, and what a caller does with each:
//!
//! * `line_bindings` declares the camera and the viewport the stages read.
//! * `line_vertex` carries `VertexInput`, `VertexOutput`, the near-plane cut and
//!   the viewport mapping, and `vertex_out`, which places one corner of the quad
//!   covering a segment.
//! * `line_fragment` measures a pixel's `coverage` and turns it into the colour
//!   the stroke writes, cutting at half coverage where the variant sets
//!   `HARD_EDGE`.
//!
//! The layouts, the instance record and the entry-point names stay the
//! variant's, exactly as they are for the built-in shader: a shader changes what
//! a stage computes, not what the pipeline reads. A module that declares a
//! binding of its own therefore fails pipeline creation rather than drawing
//! something else; one that declares functions, types or constants over the
//! bindings every variant already declares works.
//!
//! The label names the shader: it is the shader module's label, and a
//! composition error is reported under it.

use crate::pipeline::{ShaderCache, ShaderRecipe};
use crate::specialize::{PipelineDescriptor, SurfaceTarget};

/// Name of the WESL module the built-in stroke shader is composed from: the
/// file `shaders/line.wesl` of this crate's package.
const LINE_MODULE: &str = "line";

/// The byte width of one instance the line pipeline reads: the sum of the
/// attribute sizes below.
///
/// A constant rather than a computed sum because a constant is what a
/// caller's record type can assert its own size against at compile time.
pub const SEGMENT_STRIDE: u64 = 44;

/// The instance-step layout a line pipeline reads its segment records through.
///
/// The stride is [SEGMENT_STRIDE], the sum of the declared attribute sizes,
/// so a caller's segment record is tightly packed by the same tokens: a type
/// feeding the stream asserts its own size against the constant.
pub fn segment_layout() -> crate::specialize::VertexBufferLayoutDesc {
    const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Unorm8x4,
        3 => Unorm8x4,
        4 => Float32x2,
        5 => Uint32,
    ];
    crate::specialize::VertexBufferLayoutDesc {
        array_stride: SEGMENT_STRIDE,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: crate::specialize::VertexAttributes::from_slice(&ATTRIBUTES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constant the record types assert against stays the sum of the
    /// attribute sizes, so a layout edit cannot quietly desync the two.
    #[test]
    fn the_stride_constant_is_the_attribute_sizes() {
        const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Unorm8x4,
            3 => Unorm8x4,
            4 => Float32x2,
            5 => Uint32,
        ];
        let summed: u64 = ATTRIBUTES
            .iter()
            .map(|attribute| attribute.format.size())
            .sum();
        assert_eq!(SEGMENT_STRIDE, summed);
        assert_eq!(segment_layout().array_stride, summed);
    }

    /// The one flag a line variant carries is the options' hard-edge word.
    #[test]
    fn the_features_carry_the_hard_edge() {
        let surface = crate::specialize::SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 1,
        };
        let variant = LineVariant {
            surface,
            options: LineOptions::opaque(),
        };
        assert_eq!(variant.features(), [("HARD_EDGE", true)]);

        let variant = LineVariant {
            surface,
            options: LineOptions::blended(),
        };
        assert_eq!(variant.features(), [("HARD_EDGE", false)]);

        // A multisampled target antialiases through alpha-to-coverage, so the
        // options' hard-edge word yields there.
        let multisampled = crate::specialize::SurfaceKey {
            sample_count: 4,
            ..surface
        };
        let variant = LineVariant {
            surface: multisampled,
            options: LineOptions::opaque(),
        };
        assert_eq!(variant.features(), [("HARD_EDGE", false)]);
    }
}

/// The policy a caller decides about a built-in line pipeline.
///
/// Everything a draw resolves is absent: the instance record, the render
/// target and the device's storage-buffer answer are facts a draw folds into
/// an [`LineVariant`] when it composes a pipeline, not choices a caller can
/// make. The target owns the color format, the sample count and whether
/// there is a depth attachment at all; what is left is what a caller actually
/// picks.
///
/// The zero value is the usual stroke: an antialiased edge blended over the
/// frame, testing against the depth the pass holds without writing its own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LineOptions {
    /// The entry shader this policy draws with, or `None` for the built-in
    /// `line.wesl`.
    ///
    /// A caller's module is composed in place of the built-in source and
    /// changes nothing else: the layouts, the instance record and the
    /// entry-point names stay the variant's. See
    /// [`Shader`](crate::pipeline::Shader).
    ///
    /// The shader takes part in equality and hashing whole, label included:
    /// two options whose shaders differ at all — a name among the
    /// differences — build two pipelines, which is the observable
    /// difference.
    pub shader: Option<crate::pipeline::Shader>,
    /// Whether the fragment stage cuts a pixel whose coverage falls below
    /// one half instead of blending its partial colour.
    ///
    /// An antialiased edge is a veil of partial pixels; a stroke that covers
    /// the frame, as an opaque one does, has no colour to veil the background
    /// with, so it cuts cleanly at the pixel it covers instead. The cut is a
    /// whole-pixel decision, so it works at any sample count — but on a
    /// multisampled target the pipeline drops the cut and leaves the edge to
    /// the samples: state [a2c_hint](Self::a2c_hint) to have the alpha turned
    /// into the sample mask, which smooths the edge the cut would jag. See
    /// [LineVariant].
    pub hard_edge: bool,
    /// How the pipeline tests and writes the depth attachment, for a target
    /// that has one.
    ///
    /// A target without a depth attachment runs the pipeline without a
    /// depth-stencil state at all, whatever this field says. The format is
    /// the attachment's own, which the options cannot know: it is the
    /// target's word, and the variant carries the target.
    ///
    /// The default states the stroke convention: the comparison is the
    /// renderer's reverse-z one, and a stroke writes its own depth, which is
    /// what lets strokes cover one another. A stroke that blends over the
    /// frame instead states the default with the write cleared, so it hides
    /// nothing behind it.
    pub depth_stencil: crate::pipeline::DepthStencilPolicy,
    /// The blend state the fragment output composites with, or `None` to
    /// write it unblended.
    ///
    /// The common stroke composites with the target's alpha: the fragment's
    /// coverage rides its alpha, so `ALPHA_BLENDING` is what makes an edge
    /// antialiased rather than jagged. A stroke under a caller's own policy —
    /// an additive energy line, say — states its own.
    pub blend: Option<wgpu::BlendState>,
    /// Which color channels the pipeline writes to the target.
    pub write_mask: wgpu::ColorWrites,
    /// Whether the fragment's alpha states coverage, on a multisampled
    /// target.
    ///
    /// Alpha-to-coverage turns the fragment's alpha into the sample mask, so
    /// an antialiased edge lands as partially covered samples instead of a
    /// blend. That is only honest when the alpha *is* coverage: a wash whose
    /// alpha is transparency, blended over the frame, would have its
    /// contribution squared — masked to a fraction of the samples and then
    /// blended by the same fraction again — so the hint defaults to off and a
    /// stroke states it when its alpha means coverage. Nothing happens on a
    /// single-sampled target, where there are no samples to spread over.
    pub a2c_hint: bool,
}

impl Default for LineOptions {
    /// A stroke blended over the frame, testing depth without writing it.
    fn default() -> Self {
        Self {
            shader: None,
            hard_edge: false,
            depth_stencil: crate::pipeline::DepthStencilPolicy {
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Greater,
            },
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
            a2c_hint: false,
        }
    }
}

impl LineOptions {
    /// A stroke that covers the frame: no blend, the depth written.
    ///
    /// The cut at half coverage is what keeps its edge from veiling the
    /// background with a colour it does not own; see
    /// [hard_edge](Self::hard_edge). On a multisampled target the cut yields
    /// to alpha-to-coverage, which keeps the edge smooth there.
    pub const fn opaque() -> Self {
        Self {
            shader: None,
            hard_edge: true,
            depth_stencil: crate::pipeline::DepthStencilPolicy {
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Greater,
            },
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
            a2c_hint: true,
        }
    }

    /// A stroke blended over the frame, testing depth without writing it.
    pub const fn blended() -> Self {
        Self {
            shader: None,
            hard_edge: false,
            depth_stencil: crate::pipeline::DepthStencilPolicy {
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Greater,
            },
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
            a2c_hint: false,
        }
    }
}

/// The pipeline variant a line draw resolves to.
///
/// The target decides the surface, and the options decide the rest. Two
/// options that differ only where the pipeline cannot see — the shader's
/// name among them — are two variants, because a pipeline built for one
/// draws differently than one built for the other.
///
/// A difference that would change nothing is folded instead: on a
/// single-sampled target the antialiasing hint says nothing, so two
/// options that differ only in it share the one variant.
///
/// One difference the target does reach is the antialiasing: on a
/// multisampled target the variant drops the hard-edge cut, and it turns the
/// fragment's alpha into the alpha-to-coverage sample mask when the options
/// hint that the alpha states coverage, so an options' hard-edge word holds
/// only on a single-sampled one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LineVariant {
    /// The target the stroke is drawn into.
    pub surface: crate::specialize::SurfaceKey,
    /// The caller's policy for the stroke.
    pub options: LineOptions,
}

impl LineVariant {
    /// The conditional-compilation flags the variant's shader module composes
    /// with. The hard-edge cut is the one flag a line variant carries, and it
    /// holds only where no samples antialias in its place: a multisampled
    /// target leaves the edge to its samples — smoothed by
    /// alpha-to-coverage when the options hint at it — so the cut is left to
    /// a single-sampled one.
    fn features(&self) -> [(&'static str, bool); 1] {
        [(
            "HARD_EDGE",
            self.options.hard_edge && self.surface.sample_count == 1,
        )]
    }
}

impl crate::specialize::PipelineVariant<wgpu::RenderPipeline> for LineVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        self.clone()
    }
}

impl crate::specialize::PipelineDescriptor<wgpu::RenderPipeline> for LineVariant {
    fn create(&self, device: &wgpu::Device, cache: &mut ShaderCache) -> wgpu::RenderPipeline {
        pipeline(device, self, cache)
    }
}

/// The pipeline a line variant draws with.
fn pipeline(
    device: &wgpu::Device,
    variant: &LineVariant,
    cache: &mut ShaderCache,
) -> wgpu::RenderPipeline {
    let shader = variant.options.shader.as_ref();
    let label = shader.map_or("unlit_wgpu::line", |shader| shader.label());
    let features = variant.features();
    // The stroke's shader is composed once per flag set, not once per variant:
    // the blend, the write mask and the depth state a variant also carries do
    // not reach the source.
    let key = ShaderRecipe::new(LINE_MODULE, &features, shader);
    let module = cache.get_or_create(device, &key, label, || {
        let replacement = shader.map(|shader| shader.source());
        crate::compose::compose(LINE_MODULE, &features, replacement)
            .unwrap_or_else(|error| panic!("{label}: {error}"))
    });
    let layout = crate::pipeline::global_bind_group_layout(
        device,
        crate::pipeline::GlobalBindings::default(),
        !crate::pipeline::supports_storage_buffers(device),
    );
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit_wgpu::line::pipeline_layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let mut descriptor = crate::specialize::RenderPipelineDesc {
        label: Some("unlit_wgpu::line".to_owned()),
        layout: Some(pipeline_layout),
        vertex: crate::specialize::VertexStateDesc {
            module: module.clone(),
            entry_point: Some("vs_main".to_owned()),
            compilation_options: Default::default(),
            buffers: vec![Some(segment_layout())],
        },
        // A line is a flat quad seen from whichever side the camera is on,
        // and its winding depends on which way the segment happens to run, so
        // neither face is culled.
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        // The target decides whether there is a depth state at all; the
        // options decide what it does.
        depth_stencil: variant
            .surface
            .depth_stencil_format
            .map(|format| wgpu::DepthStencilState {
                format,
                depth_write_enabled: Some(variant.options.depth_stencil.depth_write_enabled),
                depth_compare: Some(variant.options.depth_stencil.depth_compare),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        // Alpha-to-coverage needs no feature gate of its own; it means
        // anything only on a multisampled target, and only where the options
        // state that the alpha is coverage rather than transparency.
        multisample: wgpu::MultisampleState {
            count: variant.surface.sample_count,
            alpha_to_coverage_enabled: variant.options.a2c_hint && variant.surface.sample_count > 1,
            ..Default::default()
        },
        fragment: Some(crate::specialize::FragmentStateDesc {
            module,
            entry_point: Some("fs_main".to_owned()),
            compilation_options: Default::default(),
            targets: vec![Some(wgpu::ColorTargetState {
                format: variant.surface.color_format,
                blend: variant.options.blend,
                write_mask: variant.options.write_mask,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    descriptor.set_surface(variant.surface);
    descriptor.create(device, cache)
}
