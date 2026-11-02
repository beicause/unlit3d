//! A frame source that draws screen-space line segments of constant pixel
//! width.
//!
//! A line is not a mesh: the width a caller asks for is in pixels and a mesh
//! vertex is in world space, so the two ends of a segment have to be projected
//! before the quad covering them can be built. A
//! [LineSource](crate::line::LineSource) therefore collects the [Line]s of its
//! scope into a per-frame instance stream and lets the vertex stage derive each
//! quad from the vertex index; nothing is expanded on the CPU and no vertex
//! buffer describes a segment corner.
//!
//! The fragment measures the distance from its own pixel to the segment and
//! turns it into coverage, so an edge is antialiased without a post-processing
//! pass. The two ends carry their own half width and their own end shape, which
//! is what lets a stroke taper and what gives a hand-drawn line round ends.
//!
//! # Framing
//!
//! The source frames the segments it finds through the first active
//! [Camera](crate::components::Camera) its scope sees, the rule
//! [MeshSource](crate::mesh_source::MeshSource) follows for meshes. A source
//! sees its own subtree plus the world shared subtree, so a world that draws
//! meshes and lines together puts the camera in
//! [World::shared](unlit_ecs::World::shared) rather than beside one of them.
//!
//! # Depth
//!
//! A stroke either blends over the frame or covers it, and
//! [Line::alpha_blend] is the caller's word for which. A blended stroke tests
//! against the depth the pass already holds and writes none of its own, so a
//! mesh in front of it hides it and it hides nothing behind it. An opaque one
//! writes its depth, so a stroke drawn earlier in the same frame can hide
//! whatever a later one would put behind it. Both test against what the meshes
//! wrote, which is why the source records after the mesh source either way: a
//! mesh recorded after a stroke would paint over it wherever the two overlap.
//! See [FrameSourceOrder::LINE](crate::source::FrameSourceOrder::LINE).
//!
//! Blended strokes are recorded farthest first, so a frame's translucent
//! strokes read in the order their colours layer in.

use glam::Vec3;
use unlit_ecs::Scope;
use unlit_wgpu::globals::Camera;
use unlit_wgpu::instance_stream::{InstanceBuffer, InstanceStreamDesc};
use unlit_wgpu::pipeline::{
    GLOBAL_GROUP, GlobalBindings, global_bind_group_layout, supports_storage_buffers,
};
use unlit_wgpu::render_attachments::default_depth_stencil_format;
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use unlit_wgpu::specialize::{
    FragmentStateDesc, PipelineDescriptor, PipelineVariant, RenderPipelineDesc, SurfaceKey,
    SurfaceTarget, Variants, VertexAttributes, VertexBufferLayoutDesc, VertexStateDesc,
};
use zerocopy::IntoBytes;

use crate::components::{InstanceVisibility, Transform, frame_camera};
use crate::global_uniforms::{CameraKey, UniformGroups, ViewKey};
use crate::pass::PassInfo;
use crate::pipeline::{GlobalRecipe, uniform_entries};
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};

/// The WESL module holding the line pipeline vertex and fragment stages.
const SHADER_MODULE: &str = "line";
/// The entry point of the vertex stage.
const VERTEX_ENTRY: &str = "vs_main";
/// The entry point of the fragment stage.
const FRAGMENT_ENTRY: &str = "fs_main";
/// The vertex-buffer slot the instance stream binds at.
///
/// A line has no per-vertex buffer at all: the six corners of a segment quad
/// come from the vertex index, so the stream is the only buffer the pipeline
/// declares and it takes the first slot.
const SEGMENT_SLOT: u32 = 0;
/// How many vertices one segment quad is drawn with.
const QUAD_VERTICES: u32 = 6;

/// One line segment, as the vertex stage reads it.
///
/// The record is the segment rather than a vertex: the six corners a segment
/// covers are derived from its two ends in the shader, so a caller writes one
/// record per segment and no corners at all.
///
/// The colours are linear, as every colour the raster pipelines read is: a
/// target that encodes sRGB does the encoding at the end of the fragment stage,
/// so a caller hands over the colour it means and never a pre-encoded one.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, PartialEq, zerocopy::FromBytes, zerocopy::Immutable, zerocopy::IntoBytes,
)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct LineSegment {
    /// Where the segment starts, in world space.
    pub a: [f32; 3],
    /// Where it ends, in world space.
    pub b: [f32; 3],
    /// The linear colour at [a](Self::a); its alpha is coverage.
    pub color_a: [u8; 4],
    /// The linear colour at [b](Self::b); its alpha is coverage.
    pub color_b: [u8; 4],
    /// Half the line width at each end, in physical pixels.
    ///
    /// The two differ when a stroke tapers; the fragment interpolates between
    /// them along the segment.
    pub half_width: [f32; 2],
    /// How the two ends are shaped, as a bit set of [Self::ROUND_A] and
    /// [Self::ROUND_B].
    ///
    /// A round end reaches half a width past its station and draws as a disc; a
    /// flat one stops at it. The two ends are separate because a stroke's outer
    /// ends are round while its inner joints are flat, which is what keeps a
    /// translucent stroke from darkening where two segments meet.
    pub caps: u32,
}

impl LineSegment {
    /// The bit of [caps](Self::caps) that rounds the [a](Self::a) end.
    pub const ROUND_A: u32 = 1 << 0;
    /// The bit of [caps](Self::caps) that rounds the [b](Self::b) end.
    pub const ROUND_B: u32 = 1 << 1;

    /// An opaque white segment of one physical pixel between two points.
    pub const fn new(a: [f32; 3], b: [f32; 3]) -> Self {
        Self {
            a,
            b,
            color_a: [255; 4],
            color_b: [255; 4],
            half_width: [0.5; 2],
            caps: 0,
        }
    }

    /// The same segment with both ends in 'color'.
    ///
    /// The common case by far: a stroke that does not fade sets one colour for
    /// the whole segment instead of assigning the two ends separately.
    #[must_use]
    pub const fn with_color(mut self, color: [u8; 4]) -> Self {
        self.color_a = color;
        self.color_b = color;
        self
    }

    /// The same segment with both ends 'half_width' pixels wide.
    #[must_use]
    pub const fn with_half_width(mut self, half_width: f32) -> Self {
        self.half_width = [half_width; 2];
        self
    }
}

// An upload moves whole records and a buffer copy is measured in alignment
// units, so a record that straddled one would leave a tail of stale bytes
// behind every upload.
const _: () = assert!(
    core::mem::size_of::<LineSegment>().is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize),
    "a segment record has to be a whole number of copy-alignment units long"
);

/// A stroke an entity wears: one [LineSegment] and how it lands on the frame.
///
/// A segment is the geometry; this component adds the one decision the
/// geometry cannot carry - whether the stroke blends over what the frame
/// already holds or covers it as an opaque colour - because [LineSegment] is
/// the GPU's byte record and a GPU record has no room for a CPU's booleans.
///
/// An entity without a [Transform](crate::components::Transform) draws its
/// segment where the segment's own points put it; one with a transform has both
/// ends placed by it.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct Line {
    /// The segment to draw.
    pub segment: LineSegment,
    /// Whether the stroke blends over the frame where it covers it only
    /// partially, rather than covering it.
    ///
    /// A blended stroke tests depth and writes none, so a mesh in front of it
    /// hides it and it hides nothing behind it. An opaque one writes its depth,
    /// which is what lets two strokes in one frame hide each other. A stroke's
    /// colour alpha is its coverage either way; the flag decides what the
    /// fragment's output does with it, not what it means.
    pub alpha_blend: bool,
}

impl Line {
    /// The segment as an opaque stroke.
    #[must_use]
    pub const fn opaque(segment: LineSegment) -> Self {
        Self {
            segment,
            alpha_blend: false,
        }
    }

    /// The segment as a blended stroke.
    #[must_use]
    pub const fn blended(segment: LineSegment) -> Self {
        Self {
            segment,
            alpha_blend: true,
        }
    }
}

impl Default for Line {
    /// A white one-pixel stroke, blending, because a stroke that draws over
    /// whatever is already there is the common one.
    fn default() -> Self {
        Self::blended(LineSegment::new([0.0; 3], [0.0; 3]))
    }
}

impl From<LineSegment> for Line {
    fn from(segment: LineSegment) -> Self {
        Self::blended(segment)
    }
}

/// The pipeline variant a line draw resolves to.
///
/// The target decides the surface, and the stroke decides the rest: a blended
/// stroke builds its colour up over the frame, an opaque one covers it and
/// writes its depth.
///
/// The antialiasing of an opaque stroke follows from the target's sample count.
/// A stroke's own edge lies a pixel inside the quad that carries it, where no
/// multisampler reaches, so the multisampler itself cannot draw the edge; a
/// single-sampled opaque stroke therefore cuts at half its coverage instead,
/// and a multisampled one carries the edge's coverage in its alpha and lets the
/// blend place it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LineVariant {
    /// The target the stroke is drawn into.
    surface: SurfaceKey,
    /// Whether the stroke blends over the frame.
    alpha_blend: bool,
}

impl LineVariant {
    /// The conditional-compilation flags the variant's shader module composes
    /// with.
    ///
    /// The cut is an opaque stroke's decision, and only where no blending will
    /// place the edge anyway: a multisampled target's opaque stroke rides its
    /// blend, as does every blended stroke.
    fn features(&self) -> [(&'static str, bool); 1] {
        [(
            "HARD_EDGE",
            !self.alpha_blend && self.surface.sample_count == 1,
        )]
    }

    /// The blend the variant's colour target composes with, 'None' writing the
    /// stroke's colour whole.
    fn blend(&self) -> Option<wgpu::BlendState> {
        if self.alpha_blend || self.surface.sample_count > 1 {
            Some(wgpu::BlendState::ALPHA_BLENDING)
        } else {
            None
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for LineVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        *self
    }
}

impl PipelineDescriptor<wgpu::RenderPipeline> for LineVariant {
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        let layout = global_bind_group_layout(
            device,
            GlobalBindings::default(),
            !supports_storage_buffers(device),
        );
        pipeline(device, &layout, *self)
    }
}

/// The GPU state one target specialization needs.
struct Gpu {
    /// The pipelines, specialized on the strokes each frame collects.
    variants: Variants<wgpu::RenderPipeline, LineVariant>,
    /// How the global group is built. The camera, the globals and the view are
    /// the frame's records, so one pipeline drawn into two viewports draws with
    /// a group per viewport.
    recipe: GlobalRecipe,
    /// The groups built from the recipe so far, one per
    /// [crate::global_uniforms::UniformsKey].
    groups: UniformGroups,
    /// The instance stream the segments are uploaded into, allocated on the
    /// first non-empty frame.
    segments: InstanceBuffer,
}

/// A frame source drawing the [Line]s of its scope.
///
/// The strokes belong to the entities: the source collects the [Line]s its
/// scope sees every frame it is asked to draw, so a caller adds, moves or
/// removes strokes by spawning entities and moving their components, the same
/// way meshes work.
pub struct LineSource {
    /// The per-target GPU state.
    ///
    /// A source is recorded once per pass reaching it, and the passes of a frame
    /// need not share a specialization key - a multisampled offscreen pass and a
    /// single-sampled swap chain differ in sample count - so the pipelines are
    /// kept per key instead of being rebuilt as the passes alternate.
    gpus: Vec<(SurfaceKey, Gpu)>,
    /// The records collected this frame, packed for the instance stream.
    ///
    /// A scratch rather than a field the caller fills: a source draws many
    /// frames, and reusing the allocation is the point of a source that owns
    /// its stream.
    records: Vec<LineSegment>,
}

impl LineSource {
    /// A source with no strokes.
    pub fn new() -> Self {
        Self {
            gpus: Vec::new(),
            records: Vec::new(),
        }
    }

    /// The entry for 'surface', building it on first use.
    fn target(&mut self, scope: &Scope<'_>, ctx: RenderContext, surface: SurfaceKey) -> usize {
        if let Some(index) = self.gpus.iter().position(|(key, _)| *key == surface) {
            return index;
        }
        let device = scope
            .query_one::<&wgpu::Device>(ctx.device)
            .expect("the context device resource exists")
            .get()
            .clone();
        let recipe = GlobalRecipe::new(move |device: &wgpu::Device, graph, uniforms| {
            let layout = global_bind_group_layout(
                device,
                GlobalBindings::default(),
                !supports_storage_buffers(device),
            );
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("unlit3d::line::globals"),
                layout: &layout,
                entries: &uniform_entries(graph, uniforms),
            })
        });
        self.gpus.push((
            surface,
            Gpu {
                variants: Variants::new(&device),
                recipe,
                groups: UniformGroups::new(),
                segments: InstanceBuffer::new(InstanceStreamDesc {
                    slot: SEGMENT_SLOT,
                    array_stride: core::mem::size_of::<LineSegment>() as u32,
                    label: Some("unlit3d::line::segments"),
                }),
            },
        ));
        self.gpus.len() - 1
    }
}

impl Default for LineSource {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSource for LineSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // The graph settles on every path out of this build, so the uniforms
        // and the group a first frame registers are realised when the scope
        // drops, whatever that frame goes on to do.
        let maintain = ctx.maintain_scope(*scope);

        // A scope with no active camera draws nothing, exactly as a mesh source
        // with none does.
        let Some((camera_entity, camera)) = frame_camera(scope) else {
            return;
        };

        // Collect the strokes the scope sees. The opaque ones keep their entity
        // order - depth writes decide their overlap, so the draw order only
        // picks which one owns a pixel they all cover, and any of them would
        // own it, being the colour the caller asked for. The blended ones are
        // re-ordered farthest first, which is the order their colours layer in.
        self.records.clear();
        let mut blended: Vec<(f32, LineSegment)> = Vec::new();
        for (line, transform, visibility) in scope
            .query::<(&Line, Option<&Transform>, Option<&InstanceVisibility>)>()
            .iter()
        {
            if visibility.is_some_and(|visibility| !visibility.visible) {
                continue;
            }
            let mut segment = line.segment;
            if let Some(transform) = transform {
                let world_from_local = transform.compute_matrix();
                segment.a = world_from_local
                    .transform_point3(Vec3::from(segment.a))
                    .to_array();
                segment.b = world_from_local
                    .transform_point3(Vec3::from(segment.b))
                    .to_array();
            }
            if line.alpha_blend {
                // The mid-point is where the eye reads a stroke's colour from,
                // and the depth a positive view axis gives is what the sort
                // keys on.
                let middle = Vec3::from(segment.a).lerp(Vec3::from(segment.b), 0.5);
                blended.push((camera.view_depth(middle), segment));
            } else {
                self.records.push(segment);
            }
        }
        blended.sort_by(|(a_depth, _), (b_depth, _)| b_depth.total_cmp(a_depth));
        let opaque_count = self.records.len();
        self.records
            .extend(blended.into_iter().map(|(_, segment)| segment));
        if self.records.is_empty() {
            return;
        }

        let index = self.target(scope, ctx, info.surface);
        let (width, height) = info
            .viewport
            .map_or((info.width as f32, info.height as f32), |viewport| {
                (viewport.width, viewport.height)
            });

        // The camera and the view are the frame's records: two passes through
        // one camera, or into one viewport, read the same two buffers, and the
        // frame globals -- which this shader does not read -- are the
        // renderer's to advance.
        let record = Camera::new(
            camera.view_from_world,
            camera.clip_from_view,
            camera.position(),
        );

        let bind_group = {
            let device = scope
                .query_one::<&wgpu::Device>(ctx.device)
                .expect("the context device resource exists")
                .get()
                .clone();
            let uniforms = {
                let mut graph_holder = ctx.graph(scope);
                let mut uniforms_holder = ctx.uniforms(scope);
                uniforms_holder.get().uniforms(
                    &device,
                    graph_holder.get(),
                    encoder,
                    CameraKey::Entity(camera_entity),
                    &record,
                    ViewKey::new(width, height),
                )
            };
            let gpu = &mut self.gpus[index].1;
            let recipe = gpu.recipe.clone();
            let mut graph_holder = ctx.graph(scope);
            let graph = graph_holder.get();
            let group = gpu
                .groups
                .group(graph, uniforms.key(), uniforms.frame, |graph| {
                    recipe.insert(&device, graph, &uniforms)
                });
            gpu.groups.prune(uniforms.frame);
            // The segments ride the frame's own encoder, so a stroke costs no
            // submission of its own.
            gpu.segments
                .upload(&device, encoder, self.records.as_bytes());
            group
        };

        // Drop the scope so the graph maintains before the group is read back
        // out: a group a first frame made has to be visible to its own draw.
        drop(maintain);
        let mut graph_holder = ctx.graph(scope);
        let graph = graph_holder.get();
        let gpu = &mut self.gpus[index].1;

        // Each bucket specializes only when it drew something this frame: a
        // frame of opaque strokes alone never builds the blended pipeline. A
        // second frame reuses what the first built.
        let opaque = (opaque_count > 0).then(|| {
            gpu.variants.specialize(LineVariant {
                surface: info.surface,
                alpha_blend: false,
            })
        });
        let blended = (self.records.len() > opaque_count).then(|| {
            gpu.variants.specialize(LineVariant {
                surface: info.surface,
                alpha_blend: true,
            })
        });
        let bind_group = graph
            .get(&bind_group)
            .expect("the bind group is registered in the graph");
        let target = gpu
            .segments
            .buffer()
            .expect("the segments were uploaded above");

        // The pass letterboxes by narrowing the viewport, and the segments were
        // placed in pixels of that same rectangle, so the draw is confined to it
        // the way a mesh draw is.
        scene.viewport = info.viewport;
        if let Some(ordinal) = opaque {
            scene.draws.push(
                DrawEntry::new(
                    &gpu.variants.get(ordinal).pipeline,
                    DrawRange::vertices(0..QUAD_VERTICES).with_instances(0..opaque_count as u32),
                )
                .with_bind_group(GLOBAL_GROUP, bind_group)
                .with_vertex_buffer(SEGMENT_SLOT, target),
            );
        }
        if let Some(ordinal) = blended {
            scene.draws.push(
                DrawEntry::new(
                    &gpu.variants.get(ordinal).pipeline,
                    DrawRange::vertices(0..QUAD_VERTICES)
                        .with_instances(opaque_count as u32..self.records.len() as u32),
                )
                .with_bind_group(GLOBAL_GROUP, bind_group)
                .with_vertex_buffer(SEGMENT_SLOT, target),
            );
        }
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::LINE
    }
}

/// The instance-step layout a line pipeline reads its segments through.
fn segment_layout() -> VertexBufferLayoutDesc {
    const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Unorm8x4,
        3 => Unorm8x4,
        4 => Float32x2,
        5 => Uint32,
    ];
    VertexBufferLayoutDesc {
        // The stride is the record whole size rather than the sum of the
        // declared formats, so a field the shader stops reading does not shift
        // the ones after it.
        array_stride: core::mem::size_of::<LineSegment>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: VertexAttributes::from_slice(&ATTRIBUTES),
    }
}

/// The pipeline a line variant draws with.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    variant: LineVariant,
) -> wgpu::RenderPipeline {
    let module = crate::builtin::shader_module(
        device,
        SHADER_MODULE,
        "unlit3d::line::shader",
        &variant.features(),
    );
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d::line::pipeline_layout"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    let mut descriptor = RenderPipelineDesc {
        label: Some("unlit3d::line".to_owned()),
        layout: Some(pipeline_layout),
        vertex: VertexStateDesc {
            module: module.clone(),
            entry_point: Some(VERTEX_ENTRY.to_owned()),
            compilation_options: Default::default(),
            buffers: vec![Some(segment_layout())],
        },
        // A line is a flat quad seen from whichever side the camera is on, and
        // its winding depends on which way the segment happens to run, so
        // neither face is culled.
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        // A stroke tests against the depth the pass holds. A blended one leaves
        // it alone, so two strokes crossing blend rather than hide each other;
        // an opaque one writes its own, so strokes cover each other the way
        // meshes do. The format and the presence of the state are settled by
        // set_surface, which drops it for a target that has no depth at all.
        depth_stencil: Some(wgpu::DepthStencilState {
            format: default_depth_stencil_format(device),
            depth_write_enabled: Some(!variant.alpha_blend),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(FragmentStateDesc {
            module,
            entry_point: Some(FRAGMENT_ENTRY.to_owned()),
            compilation_options: Default::default(),
            targets: vec![Some(wgpu::ColorTargetState {
                format: variant.surface.color_format,
                // None keeps the stroke's own colour whole, which is what
                // makes it opaque on a single-sampled target, where the cut at
                // half coverage draws its edge. Everywhere else a stroke's
                // coverage rides its alpha into the blend.
                blend: variant.blend(),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    descriptor.set_surface(variant.surface);
    descriptor.create(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared attributes sit where the record fields do, so a record
    /// whose fields are reordered cannot silently feed the shader another.
    #[test]
    fn the_segment_layout_matches_the_struct() {
        let layout = segment_layout();
        assert_eq!(
            layout.array_stride,
            core::mem::size_of::<LineSegment>() as u64
        );
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        let expected = [
            (0, core::mem::offset_of!(LineSegment, a)),
            (1, core::mem::offset_of!(LineSegment, b)),
            (2, core::mem::offset_of!(LineSegment, color_a)),
            (3, core::mem::offset_of!(LineSegment, color_b)),
            (4, core::mem::offset_of!(LineSegment, half_width)),
            (5, core::mem::offset_of!(LineSegment, caps)),
        ];
        let declared: Vec<(u32, usize)> = layout
            .attributes
            .iter()
            .map(|attribute| (attribute.shader_location, attribute.offset as usize))
            .collect();
        assert_eq!(declared, expected);
    }

    /// A record carries one flag per end, and the constructor the common case
    /// goes through leaves both ends flat.
    #[test]
    fn the_end_flags_are_one_bit_each() {
        assert_eq!(LineSegment::ROUND_A & LineSegment::ROUND_B, 0);
        let segment = LineSegment::new([0.0; 3], [1.0; 3]);
        assert_eq!(
            segment.caps & (LineSegment::ROUND_A | LineSegment::ROUND_B),
            0
        );
    }

    /// The flags and the blend the variant settles on: the opaque cut is a
    /// single-sampled target's decision, and a blended stroke never takes it.
    #[test]
    fn the_opaque_cut_is_a_single_sampled_choice() {
        let surface = |sample_count: u32| SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            depth_stencil_format: None,
            sample_count,
        };
        let opaque = LineVariant {
            surface: surface(1),
            alpha_blend: false,
        };
        assert_eq!(opaque.features(), [("HARD_EDGE", true)]);
        assert_eq!(opaque.blend(), None);

        // A multisampled target's opaque stroke cannot cut: its edge lies
        // inside the quad, where no multisampler reaches, so it rides the
        // blend like a blended stroke does.
        let opaque_multisampled = LineVariant {
            surface: surface(4),
            alpha_blend: false,
        };
        assert_eq!(opaque_multisampled.features(), [("HARD_EDGE", false)]);
        assert_eq!(
            opaque_multisampled.blend(),
            Some(wgpu::BlendState::ALPHA_BLENDING)
        );

        let blended = LineVariant {
            surface: surface(1),
            alpha_blend: true,
        };
        assert_eq!(blended.features(), [("HARD_EDGE", false)]);
        assert_eq!(blended.blend(), Some(wgpu::BlendState::ALPHA_BLENDING));
    }
}
