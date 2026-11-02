//! The caller's own shader code, spliced into the built-in `line.wesl`.
//!
//! The built-in entry shader keeps three marks, each a comment naming the
//! place it sits: one at module scope, one at the end of the vertex stage and
//! one at the end of the fragment stage. A shader is spliced in at those marks
//! while the module is still WESL, before anything is compiled, so each
//! snippet is ordinary WESL: it sees every binding, flag and helper the place
//! it lands in sees, it takes part in the conditional compilation, and its
//! diagnostics point at the line it was written on rather than at a fragment
//! of generated WGSL.
//!
//! The marks sit where the stages still have something left to do, which is
//! what keeps a snippet short:
//!
//! * The module mark is after every built-in binding and helper, so a snippet
//!   can declare the functions, types and constants the stages call.
//! * The vertex mark is after the quad corner is built, before it is returned.
//!   The output is a `var out` there, so a snippet can rewrite any varying —
//!   the coverage color among them — in place.
//! * The fragment mark is after the coverage is applied to the colour, before
//!   the hard-edge cut and the return. The colour is a `var color` there, so
//!   a snippet can rewrite it in place, and what it leaves is what the target
//!   receives — a snippet of its own may return instead, which is how a
//!   caller replaces the shading the stroke ends with.
//!
//! The layouts, the instance record and the entry-point names stay the
//! variant's, exactly as they are for the built-in shader: a snippet changes
//! what a stage computes, not what the pipeline reads. A snippet that declares
//! a binding of its own therefore fails pipeline creation rather than drawing
//! something else; one that declares functions, types or constants over the
//! bindings every variant already declares works.
//!
//! The label names the shader: it is the shader module's label, and a
//! composition error is reported under it.

use std::sync::Arc;

use crate::specialize::{PipelineDescriptor, SurfaceTarget};

/// The WESL declarations spliced in at module scope.
pub const MODULE_HOOK: &str = "// unlit_wgpu::hook module";
/// The WESL statements spliced into the vertex stage, before its return.
pub const VERTEX_HOOK: &str = "// unlit_wgpu::hook vertex";
/// The WESL statements spliced into the fragment stage, before its cut.
pub const FRAGMENT_HOOK: &str = "// unlit_wgpu::hook fragment";

/// A caller's own shader code, spliced into the built-in `line.wesl`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LineShader {
    label: Arc<str>,
    module: Arc<str>,
    vertex: Arc<str>,
    fragment: Arc<str>,
}

impl LineShader {
    /// A shader named `label` whose `module`, `vertex` and `fragment`
    /// snippets are spliced into the built-in entry shader.
    ///
    /// Any snippet may be empty, which leaves its place exactly as the
    /// built-in shader has it.
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

impl core::fmt::Debug for LineShader {
    /// Prints the label and each snippet's length rather than the snippets
    /// themselves, so a [`LineOptions`] dump stays readable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LineShader")
            .field("label", &self.label)
            .field("module", &format_args!("{} bytes", self.module.len()))
            .field("vertex", &format_args!("{} bytes", self.vertex.len()))
            .field("fragment", &format_args!("{} bytes", self.fragment.len()))
            .finish()
    }
}

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

/// The source of the built-in entry shader, exactly as the package bundles it.
///
/// It is the file `shaders/line.wesl` itself, so it carries the hook marks a
/// snippet is spliced at.
fn built_in_entry_source() -> &'static str {
    crate::shader::line::MODULE.source
}

/// `source` with each hook mark replaced by the snippet belonging to it.
///
/// The splice happens on WESL, before anything is parsed or compiled, so a
/// snippet takes part in the same conditional compilation as the code around
/// it and a diagnostic inside one points at the line it was written on.
///
/// # Panics
///
/// If the built-in source does not carry all three marks. They are checked
/// rather than trusted because a mark is a bare comment: losing one while
/// editing the shader would otherwise silently drop every snippet aimed at it.
pub(crate) fn splice(source: &str, shader: &LineShader) -> String {
    let spliced = splice_at(source, MODULE_HOOK, shader.module());
    let spliced = splice_at(&spliced, VERTEX_HOOK, shader.vertex());
    let spliced = splice_at(&spliced, FRAGMENT_HOOK, shader.fragment());
    assert!(
        !spliced.contains(MODULE_HOOK)
            && !spliced.contains(VERTEX_HOOK)
            && !spliced.contains(FRAGMENT_HOOK),
        "the built-in line entry shader carries all three hook marks"
    );
    spliced
}

/// `source` with the line carrying the first `mark` replaced by `snippet`.
///
/// The whole line goes, indentation included, so the snippet stands where the
/// mark stood and an empty snippet leaves a blank line rather than an indented
/// tail. A missing mark leaves the source untouched; [`splice`] is what
/// rejects one.
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
    /// A caller's snippets extend the built-in source and nothing else: the
    /// layouts, the instance record and the entry-point names stay the
    /// variant's. See [`LineShader`].
    ///
    /// This field does not take part in equality or hashing beyond the
    /// snippets' own: two options whose shaders differ only in name build
    /// two pipelines, which is the observable difference.
    pub shader: Option<LineShader>,
    /// Whether the fragment stage cuts a pixel whose coverage falls below
    /// one half instead of blending its partial colour.
    ///
    /// An antialiased edge is a veil of partial pixels; a stroke that covers
    /// the frame, as an opaque one does, has no colour to veil the background
    /// with, so it cuts cleanly at the pixel it covers instead. The cut is a
    /// whole-pixel decision, so it works at any sample count — but on a
    /// multisampled target it yields to the target's own antialiasing:
    /// alpha-to-coverage turns the same coverage into the sample mask, which
    /// smooths the edge the cut would jag, so the cut only applies to a
    /// single-sampled one. See [LineVariant].
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
        }
    }
}

/// The pipeline variant a line draw resolves to.
///
/// The target decides the surface, and the options decide the rest. Two
/// options that differ only where the pipeline cannot see — the snippets'
/// names among them — are two variants, because a pipeline built for one
/// draws differently than one built for the other.
///
/// One difference the target does reach is the hard-edge cut: on a
/// multisampled target the variant turns the fragment's coverage into the
/// alpha-to-coverage sample mask, which antialiases the edge the cut would
/// jag, so an options' hard-edge word holds only on a single-sampled one.
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
    /// yields where the target antialiases on its own: on a multisampled
    /// target the alpha-to-coverage mask spreads the same coverage the cut
    /// would threshold, so the stroke keeps its smooth edge and the options'
    /// hard-edge word is left to a single-sampled one.
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
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        pipeline(device, self)
    }
}

/// Compose the variant's entry shader into one WGSL module.
///
/// The main module is the built-in `line.wesl`, with the options' snippets
/// spliced into it at its hook marks. It is composed as the package's own
/// `line` module, so a snippet that names `package::view` or
/// `unlit_wgpu::view` reaches the modules the shader around it imports, and
/// sees the variant's flags.
fn compose(variant: &LineVariant) -> Result<String, wesl::Error> {
    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("unlit_wgpu".to_owned()),
        vec!["line".to_owned()],
    );
    let mut compile_options = wesl::CompileOptions::default();
    for (name, enabled) in variant.features() {
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
            // The spliced source replaces the package's own `line` module,
            // and nothing else: mounted at that one path, it keeps the
            // imports the package resolves and the module name it is
            // reported under.
            let mut modules = wesl::resolver::VirtualResolver::new();
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
    composed.map(|result| result.syntax.to_string())
}

/// The pipeline a line variant draws with.
fn pipeline(device: &wgpu::Device, variant: &LineVariant) -> wgpu::RenderPipeline {
    let label = variant
        .options
        .shader
        .as_ref()
        .map_or("unlit_wgpu::line", |shader| shader.label());
    let wgsl = compose(variant).unwrap_or_else(|error| panic!("{label}: {error}"));
    let module = crate::util::shader_module(device, &wgsl, label);
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
        // Alpha-to-coverage needs no feature gate of its own; it only means
        // anything on a multisampled target, where it turns the fragment's
        // coverage colour into the sample mask.
        multisample: wgpu::MultisampleState {
            count: variant.surface.sample_count,
            alpha_to_coverage_enabled: variant.surface.sample_count > 1,
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
    descriptor.create(device)
}
