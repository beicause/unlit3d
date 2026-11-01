//! A frame source that draws screen-space line segments of constant pixel
//! width.
//!
//! A line is not a mesh: the width a caller asks for is in pixels and a mesh
//! vertex is in world space, so the two ends of a segment have to be projected
//! before the quad covering them can be built. A [LineSource](crate::line::LineSource) therefore keeps
//! its segments in a per-frame instance stream and lets the vertex stage derive
//! each quad from the vertex index; nothing is expanded on the CPU and no vertex
//! buffer describes a segment corner.
//!
//! The fragment measures the distance from its own pixel to the segment and
//! turns it into coverage, so an edge is antialiased without a post-processing
//! pass. The two ends carry their own half width and their own end shape, which
//! is what lets a stroke taper and what gives a hand-drawn line round ends.
//!
//! # Framing
//!
//! The source frames its segments through the first active [Camera](crate::components::Camera) its scope
//! sees, the rule [MeshSource](crate::mesh_source::MeshSource) follows too. A
//! source sees its own subtree plus the world shared subtree, so a world that
//! draws meshes and lines together puts the camera in
//! [World::shared](unlit_ecs::World::shared) rather than beside one of them.
//!
//! # Depth
//!
//! Segments test against the depth the pass already holds and write none of
//! their own, so a stroke is hidden by a mesh in front of it and hides nothing
//! behind it. Writing no depth is also why the source records after the meshes:
//! a mesh recorded after a stroke would paint over it wherever the two overlap,
//! since the stroke left no depth to stop it. See
//! [FrameSourceOrder::LINE](crate::source::FrameSourceOrder::LINE).

use unlit_ecs::Scope;
use unlit_wgpu::globals::{Globals, View};
use unlit_wgpu::instance_stream::{InstanceBuffer, InstanceStreamDesc};
use unlit_wgpu::pipeline::{
    CAMERA_BINDING, FRAME_BINDING, GLOBAL_GROUP, GlobalBindings, global_bind_group_layout,
    supports_storage_buffers,
};
use unlit_wgpu::render_attachments::default_depth_stencil_format;
use unlit_wgpu::resources::ResHandle;
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use unlit_wgpu::specialize::{
    FragmentStateDesc, PipelineDescriptor, RenderPipelineDesc, SurfaceKey, SurfaceTarget,
    VertexAttributes, VertexBufferLayoutDesc, VertexStateDesc,
};
use unlit_wgpu::staging::StagingBuffer;
use zerocopy::IntoBytes;

use crate::builtin::uniform_buffer;
use crate::components::frame_camera;
use crate::frame::FrameClock;
use crate::pass::PassInfo;
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

    /// The same segment with both ends in `color`.
    ///
    /// The common case by far: a stroke that does not fade sets one colour for
    /// the whole segment instead of assigning the two ends separately.
    #[must_use]
    pub const fn with_color(mut self, color: [u8; 4]) -> Self {
        self.color_a = color;
        self.color_b = color;
        self
    }

    /// The same segment with both ends `half_width` pixels wide.
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
    core::mem::size_of::<LineSegment>() % wgpu::COPY_BUFFER_ALIGNMENT as usize == 0,
    "a segment record has to be a whole number of copy-alignment units long"
);

/// A frame source drawing [LineSegment]s.
///
/// The segments belong to the caller: they are re-uploaded whole on every frame
/// the source is asked to draw, so a caller fills the vector, draws, and empties
/// it again for the next frame. Colour, width and end shape are per segment, so
/// a frame of strokes that differ in all three is still one draw.
pub struct LineSource {
    /// The segments to draw, in world space.
    pub segments: Vec<LineSegment>,
    /// The per-target GPU state.
    ///
    /// A source is recorded once per pass reaching it, and the passes of a frame
    /// need not share a specialization key — a multisampled offscreen pass and a
    /// single-sampled swap chain differ in sample count — so the pipelines are
    /// kept per key instead of being rebuilt as the passes alternate.
    gpus: Vec<(SurfaceKey, Gpu)>,
}

/// The GPU state one target specialization needs.
struct Gpu {
    /// The pipeline, specialized on the target.
    pipeline: wgpu::RenderPipeline,
    /// The camera uniform, written every frame rather than replaced, so the
    /// group binding it never has to be rebuilt.
    camera: wgpu::Buffer,
    /// The frame globals the shared global group layout declares.
    globals: wgpu::Buffer,
    /// The group binding both uniforms, registered in the frame resource graph.
    bind_group: ResHandle<wgpu::BindGroup>,
    /// The instance stream the segments are uploaded into, allocated on the
    /// first non-empty frame.
    segments: InstanceBuffer,
    /// The staging buffer the two uniforms are rewritten through, so a steady
    /// frame allocates nothing and the copies join the frame's own encoder.
    staging: StagingBuffer,
}

impl LineSource {
    /// A source with no segments.
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            gpus: Vec::new(),
        }
    }

    /// The entry for `surface`, building it on first use.
    fn target(&mut self, scope: &Scope<'_>, ctx: RenderContext, surface: SurfaceKey) -> usize {
        if let Some(index) = self.gpus.iter().position(|(key, _)| *key == surface) {
            return index;
        }
        let mut device_row = scope
            .query_one::<&wgpu::Device>(ctx.device)
            .expect("the context device resource exists");
        let device = device_row.get();
        let layout = global_bind_group_layout(
            device,
            GlobalBindings::default(),
            !supports_storage_buffers(device),
        );
        let pipeline = pipeline(device, &layout, surface);
        let camera = uniform_buffer(
            device,
            "unlit3d::line::camera",
            <View as const_shader_layout::ShaderLayout>::SIZE.get(),
        );
        let globals = uniform_buffer(
            device,
            "unlit3d::line::globals",
            <Globals as const_shader_layout::ShaderLayout>::SIZE.get(),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::line::globals"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: CAMERA_BINDING,
                    resource: camera.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: FRAME_BINDING,
                    resource: globals.as_entire_binding(),
                },
            ],
        });
        let mut graph_holder = ctx.graph(scope);
        let graph = graph_holder.get();
        let camera_id = graph.insert(camera.clone(), None);
        let globals_id = graph.insert(globals.clone(), None);
        let bind_group = graph.insert(group, None);
        graph.add_dependency(&bind_group, &camera_id);
        graph.add_dependency(&bind_group, &globals_id);
        self.gpus.push((
            surface,
            Gpu {
                pipeline,
                camera,
                globals,
                bind_group,
                segments: InstanceBuffer::new(InstanceStreamDesc {
                    slot: SEGMENT_SLOT,
                    array_stride: core::mem::size_of::<LineSegment>() as u32,
                    label: Some("unlit3d::line::segments"),
                }),
                staging: StagingBuffer::new(),
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
        let Some(camera) = frame_camera(scope) else {
            return;
        };
        if self.segments.is_empty() {
            return;
        }

        let index = self.target(scope, ctx, info.surface);
        let (width, height) = info
            .viewport
            .map_or((info.width as f32, info.height as f32), |viewport| {
                (viewport.width, viewport.height)
            });

        let view = View::new(
            camera.view_from_world,
            camera.clip_from_view,
            camera.position(),
        )
        .with_viewport_size(width, height);
        // The shader measures in pixels and reads the camera alone, but the
        // frame globals share the group every pipeline in the frame binds, so
        // the slot is filled from the frame's own clock: a stroke that later
        // wants to animate along time reads the same numbers a mesh does, and
        // the clock belongs to the frame rather than to this source, whose
        // build may run once per pass.
        let mut globals = Globals::default();
        if let Ok(mut clock_row) = scope.query_one::<&FrameClock>(ctx.clock) {
            let clock = clock_row.get();
            globals.time = clock.time;
            globals.delta_time = clock.delta_time;
            globals.frame_count = clock.index;
        }

        {
            let mut device_row = scope
                .query_one::<&wgpu::Device>(ctx.device)
                .expect("the context device resource exists");
            let device = device_row.get();
            let gpu = &mut self.gpus[index].1;
            // Every upload of the frame is recorded into the frame's own
            // encoder and passes through the one staging buffer, so none of
            // them costs a submission of its own.
            gpu.staging
                .write(device, encoder, &gpu.camera, 0, view.as_bytes());
            gpu.staging
                .write(device, encoder, &gpu.globals, 0, globals.as_bytes());
            gpu.segments
                .upload(device, encoder, self.segments.as_bytes());
        }

        // Drop the scope so the graph maintains before the group is read back
        // out: a group a first frame made has to be visible to its own draw.
        drop(maintain);
        let mut graph_holder = ctx.graph(scope);
        let graph = graph_holder.get();
        let gpu = &self.gpus[index].1;
        let bind_group = graph
            .get(&gpu.bind_group)
            .expect("the bind group is registered in the graph");
        let target = gpu
            .segments
            .buffer()
            .expect("the segments were uploaded above");

        // The pass letterboxes by narrowing the viewport, and the segments were
        // placed in pixels of that same rectangle, so the draw is confined to it
        // the way a mesh draw is.
        scene.viewport = info.viewport;
        scene.draws.push(
            DrawEntry::new(
                &gpu.pipeline,
                DrawRange::vertices(0..QUAD_VERTICES).with_instances(0..self.segments.len() as u32),
            )
            .with_bind_group(GLOBAL_GROUP, bind_group)
            .with_vertex_buffer(SEGMENT_SLOT, target),
        );
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

/// The pipeline a line source draws with, specialized on `surface`.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    surface: SurfaceKey,
) -> wgpu::RenderPipeline {
    let module = crate::builtin::shader_module(device, SHADER_MODULE, "unlit3d::line::shader");
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
        // A stroke is coverage over whatever is already there: it tests against
        // the depth the pass holds and leaves it alone, so two strokes crossing
        // blend rather than hide each other. The format and the presence of
        // the state are settled by `set_surface`, which drops it for a target
        // that has no depth at all.
        depth_stencil: Some(wgpu::DepthStencilState {
            format: default_depth_stencil_format(device),
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: Default::default(),
        fragment: Some(FragmentStateDesc {
            module,
            entry_point: Some(FRAGMENT_ENTRY.to_owned()),
            compilation_options: Default::default(),
            targets: vec![Some(wgpu::ColorTargetState {
                format: surface.color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    descriptor.set_surface(surface);
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

    /// A new source draws nothing and holds no target: the state a frame fills
    /// is the caller's, and an empty list is what a frame with no strokes has.
    #[test]
    fn a_new_source_draws_nothing() {
        let source = LineSource::new();
        assert!(source.gpus.is_empty());
        assert!(source.segments.is_empty());
    }
}
