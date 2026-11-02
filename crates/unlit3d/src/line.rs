//! A frame source that draws screen-space line segments of constant pixel
//! width.
//!
//! A line is not a mesh: the width a caller asks for is in pixels and a mesh
//! vertex is in world space, so the two ends of a segment have to be projected
//! before the quad covering them can be built. A
//! [LineSource](crate::line::LineSource) therefore collects the
//! [Line](crate::line::Line)s of its
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
//! # Policy
//!
//! What a stroke does to the frame is the caller's
//! [LineOptions](unlit_wgpu::line_shader::LineOptions): the entry shader and
//! the snippets spliced into it, the hard-edge cut, the depth-stencil policy,
//! the blend and the write mask. [Line](crate::line::Line) carries one; two
//! strokes with the same
//! options draw together, and two with different options draw as separate
//! draws, each specialized on its own policy and the target. The common
//! strokes state themselves through [Line::opaque](crate::line::Line::opaque)
//! and [Line::blended](crate::line::Line::blended), and `From<LineSegment>`
//! is a blended one.
//!
//! # Depth
//!
//! Every stroke tests against the depth the pass already holds, which is why
//! the source records after the mesh source: a mesh recorded after a stroke
//! would paint over it wherever the two overlap. See
//! [FrameSourceOrder::LINE](crate::source::FrameSourceOrder::LINE). What a
//! stroke writes is its own policy's word: one whose depth-stencil policy
//! writes keeps the depth it covers, and one that does not write hides nothing
//! behind it.
//!
//! A stroke whose policy writes the depth draws in the sequenced tier of the
//! pass's composite, batched with the strokes that share it: the depth test
//! decides their overlap, so either order would read the same, and the
//! sequenced tier is where the batching their instances share is preserved. A
//! stroke whose policy leaves the depth alone joins the pass's depth-sorted
//! pool — one draw each, keyed on the depth of the stroke's mid-point along
//! the camera's view axis — where it sorts against the blended meshes and the
//! strokes of every other source, farthest first. See
//! [DrawOrder::Sorted](unlit_wgpu::scene::DrawOrder::Sorted).

use glam::Vec3;
use unlit_ecs::Scope;
use unlit_wgpu::globals::Camera;
use unlit_wgpu::instance_stream::{InstanceBuffer, InstanceStreamDesc};
use unlit_wgpu::line_shader::{LineOptions, LineVariant};
use unlit_wgpu::pipeline::{
    GLOBAL_GROUP, GlobalBindings, global_bind_group_layout, supports_storage_buffers,
};
use unlit_wgpu::scene::{DrawEntry, DrawOrder, DrawRange, Scene};
use unlit_wgpu::specialize::{SurfaceKey, Variants};
use zerocopy::IntoBytes;

use crate::components::{InstanceVisibility, Transform, frame_camera};
use crate::global_uniforms::{CameraKey, UniformGroups, ViewKey};
use crate::pass::PassInfo;
use crate::pipeline::{GlobalRecipe, uniform_entries};
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};

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

// The shader reads the record through the sum of the attribute sizes, so a
// record that grew a field without the layout declaring it would feed the
// pipeline a stride too short for its own tail.
const _: () = assert!(
    core::mem::size_of::<LineSegment>() as u64 == unlit_wgpu::line_shader::SEGMENT_STRIDE,
    "a segment record matches the layout the line pipeline reads it through"
);

/// A stroke an entity wears: one [LineSegment] and the policy it lands with.
///
/// A segment is the geometry; this component adds what the geometry cannot
/// carry - the [LineOptions] the stroke draws with - because [LineSegment] is
/// the GPU's byte record and a GPU record has no room for a CPU's policy.
///
/// The reflection is a proxy over the segment alone: the options are GPU
/// policy, not a value a JSON bridge reads or writes, and a stroke decoded
/// from JSON comes back wearing the default options.
///
/// An entity without a [Transform] draws its
/// segment where the segment's own points put it; one with a transform has both
/// ends placed by it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    feature = "reflect",
    derive(facet::Facet),
    facet(opaque, proxy = crate::reflect::LineProxy)
)]
pub struct Line {
    /// The segment to draw.
    pub segment: LineSegment,
    /// How the stroke draws: the entry shader and its snippets, the hard-edge
    /// cut, the depth-stencil policy, the blend and the write mask. See
    /// [LineOptions].
    #[cfg_attr(feature = "reflect", facet(skip))]
    pub options: LineOptions,
}

impl Line {
    /// The segment as a stroke that covers the frame: no blend, the depth
    /// written, the edge cut at half coverage.
    #[must_use]
    pub fn opaque(segment: LineSegment) -> Self {
        Self {
            segment,
            options: LineOptions::opaque(),
        }
    }

    /// The segment as a stroke blended over the frame, testing depth without
    /// writing it.
    #[must_use]
    pub fn blended(segment: LineSegment) -> Self {
        Self {
            segment,
            options: LineOptions::blended(),
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
    ///
    /// The stream holds one frame's collection at a time: a source recorded
    /// into a second pass uploads the same records again, overwriting bytes
    /// the first pass's draws still name. That is safe because a source's
    /// records come from its own scope, not from the pass — two uploads
    /// carry the same bytes — but a per-pass record would break it, and the
    /// repeat upload is spent work. See
    /// [InstanceBuffer::upload](unlit_wgpu::instance_stream::InstanceBuffer::upload).
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
    /// The depth each record in [records](Self::records) was collected
    /// with, at the same index.
    ///
    /// A scratch too, for the same reason the records are: the depths are
    /// only read when a group's draws are pushed, but keeping them beside
    /// the records lets a blended stroke's draw key on its own depth without
    /// a second pass over the collected strokes.
    depths: Vec<f32>,
    /// The groups collected this frame, parallel to the runs they describe:
    /// the options they wear and the half-open record range they cover.
    ///
    /// A scratch too, for the same reason the records are. The ranges are
    /// what the draws read — the options carry no strokes of their own — so
    /// a steady frame reuses every allocation: the records, the depths, the
    /// ranges and the options' own slots all survive from frame to frame.
    groups: Vec<(LineOptions, u32, u32)>,
}

impl LineSource {
    /// A source with no strokes.
    pub fn new() -> Self {
        Self {
            gpus: Vec::new(),
            records: Vec::new(),
            depths: Vec::new(),
            groups: Vec::new(),
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

    /// The strokes this frame's scope sees, packed into the records, the
    /// depths and the group ranges the draws read.
    ///
    /// A run of strokes wearing equal options is one group; a different
    /// policy opens a new one, because one pipeline wears one policy. A group
    /// that writes the depth keeps its entity order, because the depth
    /// decides its overlap and the order only picks which stroke owns a pixel
    /// they all cover; a group that leaves the depth alone keeps its
    /// discovery order too, because the pass's composite sorts its draws
    /// against every other source's, and this source need not guess.
    fn collect(&mut self, scope: &Scope<'_>, camera: &crate::components::Camera) {
        self.records.clear();
        self.depths.clear();
        self.groups.clear();
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

            // The mid-point is where the eye reads a stroke's colour from, and
            // the depth a positive view axis gives is what the sort keys on.
            let middle = Vec3::from(segment.a).lerp(Vec3::from(segment.b), 0.5);
            let depth = camera.view_depth(middle);

            match self.groups.last_mut() {
                Some((options, _, end)) if *options == line.options => *end += 1,
                _ => {
                    let start = self.records.len() as u32;
                    self.groups.push((line.options.clone(), start, start + 1));
                }
            }
            self.records.push(segment);
            self.depths.push(depth);
        }
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

        self.collect(scope, &camera);
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

        // A draw specializes only when it drew something this frame: a frame
        // of one policy alone never builds another's pipeline. A second frame
        // reuses what the first built.
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
        for (options, start, end) in &self.groups {
            let (start, end) = (*start, *end);
            let ordinal = gpu.variants.specialize(LineVariant {
                surface: info.surface,
                options: options.clone(),
            });
            let pipeline = &gpu.variants.get(ordinal).pipeline;
            if options.depth_stencil.depth_write_enabled {
                // The policy writes the depth, so the depth test decides the
                // group's overlap and its draw batches in the sequenced tier,
                // where the strokes that share it stay one draw.
                scene.draws.push(
                    DrawEntry::new(
                        pipeline,
                        DrawRange::vertices(0..QUAD_VERTICES).with_instances(start..end),
                    )
                    .with_bind_group(GLOBAL_GROUP, bind_group)
                    .with_vertex_buffer(SEGMENT_SLOT, target),
                );
            } else {
                // One draw per stroke: the composite keys each on the depth
                // the stroke was collected with, so it lands in the pass's
                // pool wherever the blended geometry of every other source
                // sorts. A stroke costs a draw of its own, the same price a
                // z-sorted mesh pane pays; the quads stay instanced and the
                // segment instances stay one buffer.
                for (offset, depth) in self.depths[start as usize..end as usize].iter().enumerate()
                {
                    let base = start + offset as u32;
                    scene.draws.push(
                        DrawEntry::new(
                            pipeline,
                            DrawRange::vertices(0..QUAD_VERTICES).with_instances(base..base + 1),
                        )
                        .with_bind_group(GLOBAL_GROUP, bind_group)
                        .with_vertex_buffer(SEGMENT_SLOT, target)
                        .with_order(DrawOrder::Sorted(*depth)),
                    );
                }
            }
        }
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::LINE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared attributes sit where the record fields do, so a record
    /// whose fields are reordered cannot silently feed the shader another.
    #[test]
    fn the_segment_layout_matches_the_struct() {
        let layout = unlit_wgpu::line_shader::segment_layout();
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

    /// The two named policies a stroke states itself as: a covering one cuts
    /// its edge and keeps the depth, a blending one veils it and leaves the
    /// depth alone. Both test the depth the pass holds either way.
    #[test]
    fn the_named_policies_state_their_depth_and_edge() {
        let opaque = Line::opaque(LineSegment::new([0.0; 3], [1.0; 3]));
        assert!(opaque.options.hard_edge);
        assert!(opaque.options.depth_stencil.depth_write_enabled);
        assert_eq!(opaque.options.blend, None);
        assert_eq!(
            opaque.options.depth_stencil.depth_compare,
            wgpu::CompareFunction::Greater
        );
        // A covering stroke's alpha is honest coverage, so the hint rides
        // along where the samples can use it.
        assert!(opaque.options.a2c_hint);

        let blended = Line::blended(LineSegment::new([0.0; 3], [1.0; 3]));
        assert!(!blended.options.hard_edge);
        assert!(!blended.options.depth_stencil.depth_write_enabled);
        assert_eq!(
            blended.options.blend,
            Some(wgpu::BlendState::ALPHA_BLENDING)
        );
        // A veiled stroke's alpha is light, not coverage: squaring it on a
        // multisampled target would thin it, so the hint stays off.
        assert!(!blended.options.a2c_hint);
    }

    /// The variant a draw resolves carries the caller's policy whole: the
    /// surface is the target's word, and everything else is the options'. A
    /// variant is what a draw specializes on, so two policies that differ
    /// anywhere in their options resolve to two pipelines.
    #[test]
    fn a_variant_carries_the_target_and_the_policy() {
        let surface = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 4,
        };
        let variant = LineVariant {
            surface,
            options: LineOptions::opaque(),
        };
        assert_eq!(variant.surface, surface);
        assert_eq!(variant.options, LineOptions::opaque());

        let mut soft = LineOptions::opaque();
        soft.hard_edge = false;
        soft.blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        assert_ne!(
            variant.options, soft,
            "two policies that differ draw differently"
        );

        // The A2C hint is part of the policy: flipping it alone resolves to
        // another pipeline, because it rewires the multisample state.
        let mut hinted = LineOptions::blended();
        hinted.a2c_hint = true;
        assert_ne!(
            LineOptions::blended(),
            hinted,
            "the hint participates in the variant's identity"
        );
    }
}
