//! The declarative description of a frame.
//!
//! A [`Scene`] is pure data: it names the pipelines to run, the bind groups
//! and vertex buffers each draw binds, and the ranges to draw. Recording it
//! ([`Scene::record`]) is a separate step, so a caller can build the
//! description once and reuse, inspect or modify it — and custom draw
//! commands are expressed with the same shapes as the built-in ones.
//!
//! The recording loop is flat: each draw names its own pipeline and every
//! resource it binds.
//!
//! ```text
//! for draw in scene.draws {
//!     set the pipeline, then the draw's bind groups
//!     set the draw's vertex and index buffers
//!     draw (indexed or not)
//! }
//! ```
//!
//! Consecutive draws that bind the same resource skip the redundant
//! `set_*` call.
//!
//! Scissor rectangles are the exception to "a draw is independent": wgpu has
//! no reset call, so a rectangle set by one draw stays set for every draw
//! after it in the pass. A scene therefore orders its clipped draws last —
//! or sets [`DrawEntry::scissor`] on every draw. The stencil reference is the
//! same kind of pass state, but it is set on every draw (from
//! [`DrawEntry::stencil_reference`]), so it never leaks from one draw to the
//! next.

use arrayvec::ArrayVec;
use core::ops::Range;

/// One bind-group slot: the group index and the group to bind there.
pub type BindGroupBinding<'a> = (u32, &'a wgpu::BindGroup);

/// One vertex-buffer slot: the slot index and the buffer slice to bind there.
pub type VertexBufferBinding<'a> = (u32, wgpu::BufferSlice<'a>);

/// A scissor rectangle, in pixels.
///
/// Fragments outside it are discarded, which is how a clipped draw — a
/// tessellated user-interface primitive, for example — keeps to its clip
/// rectangle without extra geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScissorRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl ScissorRect {
    /// A rectangle covering `width` x `height` pixels from the origin.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// Same rectangle, with its origin moved to (`x`, `y`).
    pub fn at(self, x: u32, y: u32) -> Self {
        Self { x, y, ..self }
    }
}

/// What to draw for one mesh.
#[derive(Clone, Debug)]
pub enum DrawRange {
    /// Draw `vertices` without an index buffer, instanced over `instances`.
    Vertices {
        /// Vertex range to draw.
        vertices: Range<u32>,
        /// Instance range to draw.
        instances: Range<u32>,
    },
    /// Draw indexed, instanced over `instances`.
    Indexed {
        /// Index range to draw.
        indices: Range<u32>,
        /// Value added to every index before fetching a vertex.
        base_vertex: i32,
        /// Instance range to draw.
        instances: Range<u32>,
    },
}

impl DrawRange {
    /// A single non-indexed draw of `vertices`.
    pub fn vertices(vertices: Range<u32>) -> Self {
        Self::Vertices {
            vertices,
            instances: 0..1,
        }
    }

    /// A single indexed draw of `indices`.
    pub fn indexed(indices: Range<u32>) -> Self {
        Self::Indexed {
            indices,
            base_vertex: 0,
            instances: 0..1,
        }
    }

    /// Draw `count` instances instead of one.
    pub fn with_instances(self, instances: Range<u32>) -> Self {
        match self {
            Self::Vertices { vertices, .. } => Self::Vertices {
                vertices,
                instances,
            },
            Self::Indexed {
                indices,
                base_vertex,
                ..
            } => Self::Indexed {
                indices,
                base_vertex,
                instances,
            },
        }
    }

    /// Offset every fetched vertex index by `base_vertex` (indexed draws
    /// only; a no-op for non-indexed draws).
    pub fn with_base_vertex(self, base_vertex: i32) -> Self {
        match self {
            Self::Indexed {
                indices, instances, ..
            } => Self::Indexed {
                indices,
                base_vertex,
                instances,
            },
            other => other,
        }
    }
}

/// One draw: the pipeline and every resource it binds, followed by what to
/// draw.
///
/// A draw carries the union of the bind groups the pipeline, its material and
/// the mesh need, at the slots they name. The renderer's built-in pipeline
/// expects three or four vertex buffers — position, optional joints and
/// weights, UV and vertex color, and the per-instance model matrix and base
/// color — but the shape is generic, so custom pipelines can bind whatever
/// they declare.
///
/// All slots are inline: a draw never allocates, so a whole scene can be
/// rebuilt each frame without touching the allocator.
#[derive(Clone, Debug)]
pub struct DrawEntry<'a> {
    /// The render pipeline to bind.
    pub pipeline: &'a wgpu::RenderPipeline,
    /// Bind groups, bound at the slots they name. The built-in pipeline uses
    /// index 0 for the camera, frame globals and mesh metadata, index 1 for
    /// the material, and any remaining slots for mesh-level groups.
    pub bind_groups: ArrayVec<BindGroupBinding<'a>, MAX_BIND_GROUPS>,
    /// Vertex buffers, bound at the slots they name.
    pub vertex_buffers: ArrayVec<VertexBufferBinding<'a>, MAX_VERTEX_BUFFERS>,
    /// Index buffer and its format, when the draw is indexed.
    pub index_buffer: Option<(wgpu::BufferSlice<'a>, wgpu::IndexFormat)>,
    /// Pixels outside this rectangle are discarded.
    ///
    /// A draw that sets it clips in the rasterizer instead of in the geometry.
    /// `None` means "leave whatever the pass already has", **not** "cover the
    /// whole target": wgpu's scissor is pass state with no reset call, so
    /// once a draw sets one it stays set for every later draw in the pass —
    /// ordering a clipped draw last is the caller's job.
    pub scissor: Option<ScissorRect>,
    /// The stencil reference value the draw tests against.
    ///
    /// Set on every draw (defaulting to `0`, the pass's own default), so a
    /// draw that does not name one resets a value an earlier draw left behind.
    pub stencil_reference: u32,
    /// What to draw.
    pub range: DrawRange,
}

impl<'a> DrawEntry<'a> {
    /// A draw of `range` with the given pipeline, no bind groups, no vertex
    /// buffers, no index buffer, no scissor and a zero stencil reference; fill
    /// in the fields the pipeline needs.
    pub fn new(pipeline: &'a wgpu::RenderPipeline, range: DrawRange) -> Self {
        Self {
            pipeline,
            bind_groups: ArrayVec::new(),
            vertex_buffers: ArrayVec::new(),
            index_buffer: None,
            scissor: None,
            stencil_reference: 0,
            range,
        }
    }

    /// Bind `bind_group` at `index`.
    pub fn with_bind_group(mut self, index: u32, bind_group: &'a wgpu::BindGroup) -> Self {
        self.bind_groups.push((index, bind_group));
        self
    }

    /// Bind `buffer` at vertex-buffer `slot`.
    pub fn with_vertex_buffer(mut self, slot: u32, buffer: wgpu::BufferSlice<'a>) -> Self {
        self.vertex_buffers.push((slot, buffer));
        self
    }

    /// Discard fragments outside `scissor`. It stays set for every draw after
    /// this one in the pass, so see [`DrawEntry::scissor`].
    pub fn with_scissor(mut self, scissor: ScissorRect) -> Self {
        self.scissor = Some(scissor);
        self
    }

    /// Test against stencil reference `reference`.
    pub fn with_stencil_reference(mut self, reference: u32) -> Self {
        self.stencil_reference = reference;
        self
    }

    /// Bind `buffer` as the index buffer.
    pub fn with_index_buffer(
        mut self,
        buffer: wgpu::BufferSlice<'a>,
        format: wgpu::IndexFormat,
    ) -> Self {
        self.index_buffer = Some((buffer, format));
        self
    }
}

/// A frame's worth of draws.
///
/// The order of [`DrawEntry`]s is the order they are recorded in. Recording
/// compares every `set_*` against the state the previous draw left behind, so
/// consecutive draws that share a pipeline or bind group skip the redundant
/// call: ordering draws by pipeline, and then so that neighbours share their
/// bind groups, minimizes state changes without changing what is drawn. A
/// caller is also free to order the draws for opaque-then-transparent
/// compositing, which is a correctness requirement rather than a performance
/// one.
#[derive(Clone, Debug, Default)]
pub struct Scene<'a> {
    /// The draws to run, in order.
    pub draws: Vec<DrawEntry<'a>>,
}

impl<'a> Scene<'a> {
    /// An empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `draw` to this scene.
    pub fn push(&mut self, draw: DrawEntry<'a>) {
        self.draws.push(draw);
    }

    /// Append `draw` to this scene and return it.
    pub fn with_draw(mut self, draw: DrawEntry<'a>) -> Self {
        self.draws.push(draw);
        self
    }

    /// Whether the scene draws nothing.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// Record the scene into `pass`, skipping any `set_*` call whose target is
    /// already bound from the previous draw.
    pub fn record(&self, pass: &mut wgpu::RenderPass<'_>) {
        let mut state = PassState::default();
        for draw in &self.draws {
            if state.pipeline != Some(draw.pipeline) {
                pass.set_pipeline(draw.pipeline);
                state.pipeline = Some(draw.pipeline);
            }
            for &(index, bind_group) in &draw.bind_groups {
                state.set_bind_group(pass, index, bind_group);
            }
            for (slot, buffer) in &draw.vertex_buffers {
                state.set_vertex_buffer(pass, *slot, *buffer);
            }
            if let Some((buffer, format)) = &draw.index_buffer {
                state.set_index_buffer(pass, *buffer, *format);
            }
            if let Some(scissor) = &draw.scissor {
                state.set_scissor(pass, *scissor);
            }
            state.set_stencil_reference(pass, draw.stencil_reference);

            match &draw.range {
                DrawRange::Vertices {
                    vertices,
                    instances,
                } => pass.draw(vertices.clone(), instances.clone()),
                DrawRange::Indexed {
                    indices,
                    base_vertex,
                    instances,
                } => pass.draw_indexed(indices.clone(), *base_vertex, instances.clone()),
            }
        }
    }
    /// Empty this scene and hand back its allocation with an unconstrained
    /// lifetime, ready to be reused by a later frame.
    ///
    /// Reusing the allocation is what keeps a steady scene from allocating;
    /// see [`Scene::reborrow`] for the other half.
    pub fn recycle(mut self) -> Scene<'static> {
        self.draws.clear();
        Scene {
            draws: launder(self.draws),
        }
    }
}

impl Scene<'static> {
    /// Reuse this empty scene's allocation for a scene that borrows
    /// shorter-lived resources, such as one frame's pipelines and buffers.
    ///
    /// Paired with [`Scene::recycle`], this lets a caller keep a
    /// `Scene<'static>` between frames and lend it the frame's lifetime while
    /// the frame is recorded.
    pub fn reborrow<'a>(self) -> Scene<'a> {
        Scene {
            draws: launder(self.draws),
        }
    }
}

/// Move a `Vec`'s allocation to a different element lifetime.
///
/// The vector must be empty: no element is moved, so no value of the old
/// lifetime is ever observed as the new one. It is how a [`Scene`] survives
/// between frames while each frame's draws borrow resources that do not.
fn launder<A, B>(vec: Vec<A>) -> Vec<B> {
    debug_assert!(vec.is_empty(), "only an empty Vec can change lifetime");
    vec.into_iter().map(|_| unreachable!()).collect()
}

/// Maximum number of bind-group slots a pass is tracked for.
///
/// The largest `max_bind_groups` a WebGPU device can advertise for this
/// renderer; exceeding it is a programming error caught while recording.
pub const MAX_BIND_GROUPS: usize = 8;

/// Maximum number of vertex-buffer slots a pass is tracked for.
pub const MAX_VERTEX_BUFFERS: usize = 16;

/// The render-pass operations [`PassState`] records through.
///
/// [`wgpu::RenderPass`] is the production implementation. Tests implement it
/// with a mock that counts calls, so the state-reuse rules — a `set_*` whose
/// target is already bound is skipped — can be checked without asking wgpu to
/// expose its command stream.
trait RenderPassInterface<'a> {
    /// Bind `bind_group` at `index`.
    fn set_bind_group(&mut self, index: u32, bind_group: &'a wgpu::BindGroup);
    /// Bind `buffer` at vertex-buffer `slot`.
    fn set_vertex_buffer(&mut self, slot: u32, buffer: wgpu::BufferSlice<'a>);
    /// Bind the index buffer.
    fn set_index_buffer(&mut self, buffer: wgpu::BufferSlice<'a>, format: wgpu::IndexFormat);
    /// Set the scissor rectangle.
    fn set_scissor_rect(&mut self, scissor: ScissorRect);
    /// Set the stencil reference.
    fn set_stencil_reference(&mut self, reference: u32);
}

impl<'a, 'p> RenderPassInterface<'a> for wgpu::RenderPass<'p> {
    fn set_bind_group(&mut self, index: u32, bind_group: &'a wgpu::BindGroup) {
        wgpu::RenderPass::set_bind_group(self, index, bind_group, &[]);
    }

    fn set_vertex_buffer(&mut self, slot: u32, buffer: wgpu::BufferSlice<'a>) {
        wgpu::RenderPass::set_vertex_buffer(self, slot, buffer);
    }

    fn set_index_buffer(&mut self, buffer: wgpu::BufferSlice<'a>, format: wgpu::IndexFormat) {
        wgpu::RenderPass::set_index_buffer(self, buffer, format);
    }

    fn set_scissor_rect(&mut self, scissor: ScissorRect) {
        wgpu::RenderPass::set_scissor_rect(
            self,
            scissor.x,
            scissor.y,
            scissor.width,
            scissor.height,
        );
    }

    fn set_stencil_reference(&mut self, reference: u32) {
        wgpu::RenderPass::set_stencil_reference(self, reference);
    }
}

/// Which resources the previous draw left bound.
///
/// `wgpu` resources compare by identity, so a plain `==` is the fast
/// "is this the same resource as last time" test the recording loop needs.
/// The slots are fixed-capacity ([`MAX_BIND_GROUPS`] / [`MAX_VERTEX_BUFFERS`])
/// so recording a frame never allocates.
#[derive(Default)]
struct PassState<'a> {
    pipeline: Option<&'a wgpu::RenderPipeline>,
    bind_groups: ArrayVec<(u32, &'a wgpu::BindGroup), MAX_BIND_GROUPS>,
    vertex_buffers: ArrayVec<(u32, wgpu::BufferSlice<'a>), MAX_VERTEX_BUFFERS>,
    index_buffer: Option<(wgpu::BufferSlice<'a>, wgpu::IndexFormat)>,
    scissor: Option<ScissorRect>,
    stencil_reference: u32,
}

impl<'a> PassState<'a> {
    fn set_bind_group(
        &mut self,
        pass: &mut impl RenderPassInterface<'a>,
        index: u32,
        bind_group: &'a wgpu::BindGroup,
    ) {
        if let Some(slot) = self
            .bind_groups
            .iter_mut()
            .find(|(bound, _)| *bound == index)
        {
            if slot.1 == bind_group {
                return;
            }
            slot.1 = bind_group;
        } else {
            assert!(
                index < MAX_BIND_GROUPS as u32,
                "bind group index {index} exceeds MAX_BIND_GROUPS ({MAX_BIND_GROUPS})"
            );
            self.bind_groups.push((index, bind_group));
        }
        pass.set_bind_group(index, bind_group);
    }

    fn set_vertex_buffer(
        &mut self,
        pass: &mut impl RenderPassInterface<'a>,
        slot: u32,
        buffer: wgpu::BufferSlice<'a>,
    ) {
        if let Some(bound) = self
            .vertex_buffers
            .iter_mut()
            .find(|(bound, _)| *bound == slot)
        {
            if bound.1 == buffer {
                return;
            }
            bound.1 = buffer;
        } else {
            assert!(
                slot < MAX_VERTEX_BUFFERS as u32,
                "vertex buffer slot {slot} exceeds MAX_VERTEX_BUFFERS ({MAX_VERTEX_BUFFERS})"
            );
            self.vertex_buffers.push((slot, buffer));
        }
        pass.set_vertex_buffer(slot, buffer);
    }

    fn set_index_buffer(
        &mut self,
        pass: &mut impl RenderPassInterface<'a>,
        buffer: wgpu::BufferSlice<'a>,
        format: wgpu::IndexFormat,
    ) {
        if let Some((bound, bound_format)) = &self.index_buffer
            && *bound == buffer
            && *bound_format == format
        {
            return;
        }
        self.index_buffer = Some((buffer, format));
        pass.set_index_buffer(buffer, format);
    }

    /// Narrow rasterization to `scissor`, skipping the call when the pass
    /// already has that rectangle — a clip rectangle is usually shared by
    /// many consecutive draws.
    fn set_scissor(&mut self, pass: &mut impl RenderPassInterface<'a>, scissor: ScissorRect) {
        if self.scissor == Some(scissor) {
            return;
        }
        self.scissor = Some(scissor);
        pass.set_scissor_rect(scissor);
    }

    /// Set the stencil reference, skipping the call when the pass already
    /// holds it.
    fn set_stencil_reference(&mut self, pass: &mut impl RenderPassInterface<'a>, reference: u32) {
        if self.stencil_reference == reference {
            return;
        }
        self.stencil_reference = reference;
        pass.set_stencil_reference(reference);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest shader that compiles: a vertex stage returning the origin
    /// and a fragment stage returning opaque white.
    const MINIMAL_WGSL: &str = r#"
@vertex
fn vs_main() -> @builtin(position) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
"#;

    /// A render pipeline on the noop device, for builder tests that only need
    /// a handle to name.
    fn noop_pipeline(device: &wgpu::Device) -> wgpu::RenderPipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(MINIMAL_WGSL.into()),
        });
        let targets = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("test::pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    #[test]
    fn draw_range_builders_compose() {
        let range = DrawRange::vertices(0..36).with_instances(2..5);
        match range {
            DrawRange::Vertices {
                vertices,
                instances,
            } => {
                assert_eq!(vertices, 0..36);
                assert_eq!(instances, 2..5);
            }
            other => panic!("expected a non-indexed draw, got {other:?}"),
        }

        let range = DrawRange::indexed(0..12)
            .with_base_vertex(7)
            .with_instances(0..3);
        match range {
            DrawRange::Indexed {
                indices,
                base_vertex,
                instances,
            } => {
                assert_eq!(indices, 0..12);
                assert_eq!(base_vertex, 7);
                assert_eq!(instances, 0..3);
            }
            other => panic!("expected an indexed draw, got {other:?}"),
        }
    }

    #[test]
    fn base_vertex_is_ignored_for_non_indexed_draws() {
        let range = DrawRange::vertices(0..3).with_base_vertex(9);
        assert!(matches!(range, DrawRange::Vertices { .. }));
    }

    #[test]
    fn scissor_rect_builders_compose() {
        let rect = ScissorRect::new(64, 32).at(8, 16);
        assert_eq!(
            rect,
            ScissorRect {
                x: 8,
                y: 16,
                width: 64,
                height: 32
            }
        );
        assert_eq!(ScissorRect::new(64, 32), rect.at(0, 0));
    }

    /// The builders thread through a draw unchanged, so a draw keeps every
    /// bound resource while adding the scissor.
    #[test]
    fn draw_builders_keep_the_scissor() {
        let (device, _queue) = crate::util::test::noop_device();
        let pipeline = noop_pipeline(&device);
        let draw = DrawEntry::new(&pipeline, DrawRange::indexed(0..6))
            .with_scissor(ScissorRect::new(8, 8));
        assert_eq!(draw.scissor, Some(ScissorRect::new(8, 8)));
        assert!(matches!(
            draw.range,
            DrawRange::Indexed { ref indices, .. } if *indices == (0..6)
        ));

        // A draw that never sets one leaves the pass's rectangle alone.
        let plain = DrawEntry::new(&pipeline, DrawRange::vertices(0..3));
        assert_eq!(plain.scissor, None);
    }

    /// The stencil reference defaults to zero and the builder sets it without
    /// disturbing the rest of the draw.
    #[test]
    fn stencil_reference_builder_composes() {
        let (device, _queue) = crate::util::test::noop_device();
        let pipeline = noop_pipeline(&device);
        let draw = DrawEntry::new(&pipeline, DrawRange::indexed(0..6))
            .with_stencil_reference(3)
            .with_scissor(ScissorRect::new(8, 8));
        assert_eq!(draw.stencil_reference, 3);
        assert_eq!(draw.scissor, Some(ScissorRect::new(8, 8)));

        let plain = DrawEntry::new(&pipeline, DrawRange::vertices(0..3));
        assert_eq!(plain.stencil_reference, 0);
    }

    /// A mock [`RenderPassInterface`] that records the calls it receives, so a
    /// test can assert exactly which `set_*` calls `PassState` skipped.
    /// Recording needs no device and no render pass of its own.
    #[derive(Default)]
    struct MockPass {
        bind_groups: Vec<u32>,
        vertex_buffers: Vec<u32>,
        index_buffers: Vec<wgpu::IndexFormat>,
        scissors: Vec<ScissorRect>,
        stencil_references: Vec<u32>,
    }

    impl<'a> RenderPassInterface<'a> for MockPass {
        fn set_bind_group(&mut self, index: u32, _bind_group: &'a wgpu::BindGroup) {
            self.bind_groups.push(index);
        }

        fn set_vertex_buffer(&mut self, slot: u32, _buffer: wgpu::BufferSlice<'a>) {
            self.vertex_buffers.push(slot);
        }

        fn set_index_buffer(&mut self, _buffer: wgpu::BufferSlice<'a>, format: wgpu::IndexFormat) {
            self.index_buffers.push(format);
        }

        fn set_scissor_rect(&mut self, scissor: ScissorRect) {
            self.scissors.push(scissor);
        }

        fn set_stencil_reference(&mut self, reference: u32) {
            self.stencil_references.push(reference);
        }
    }

    /// A repeated scissor rectangle is recorded once; a different one after it
    /// again — so a draw never silently inherits a stale clip.
    #[test]
    fn scissor_calls_are_deduplicated() {
        let mut pass = MockPass::default();
        let mut state = PassState::default();
        let first = ScissorRect::new(16, 8).at(4, 2);
        let second = ScissorRect::new(16, 8).at(0, 0);

        state.set_scissor(&mut pass, first);
        // The same rectangle again is a no-op: the pass already has it.
        state.set_scissor(&mut pass, first);
        state.set_scissor(&mut pass, second);

        assert_eq!(pass.scissors, vec![first, second]);
        assert_eq!(state.scissor, Some(second));
    }

    /// The stencil reference is recorded once per distinct value, so a value
    /// an earlier draw left behind never leaks into one that names none.
    #[test]
    fn stencil_reference_calls_are_deduplicated() {
        let mut pass = MockPass::default();
        let mut state = PassState::default();

        state.set_stencil_reference(&mut pass, 3);
        // The same value again is a no-op: the pass already holds it.
        state.set_stencil_reference(&mut pass, 3);
        // A later draw that names none resets it to zero.
        state.set_stencil_reference(&mut pass, 0);

        assert_eq!(pass.stencil_references, vec![3, 0]);
        assert_eq!(state.stencil_reference, 0);
    }

    /// Bind groups, vertex buffers and index buffers are recorded once per
    /// distinct target too. The handles are real — identity is what the
    /// dedup compares — but the pass is the mock, so the calls are visible.
    #[test]
    fn bound_resources_are_deduplicated() {
        let (device, _queue) = crate::util::test::noop_device();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test::buffer"),
            size: 64,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test::layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test::bind_group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        let mut pass = MockPass::default();
        let mut state = PassState::default();
        let slice = buffer.slice(..);

        state.set_bind_group(&mut pass, 0, &bind_group);
        state.set_bind_group(&mut pass, 0, &bind_group);
        state.set_bind_group(&mut pass, 3, &bind_group);

        state.set_vertex_buffer(&mut pass, 0, slice);
        state.set_vertex_buffer(&mut pass, 0, slice);
        state.set_vertex_buffer(&mut pass, 2, slice);

        state.set_index_buffer(&mut pass, slice, wgpu::IndexFormat::Uint16);
        state.set_index_buffer(&mut pass, slice, wgpu::IndexFormat::Uint16);
        state.set_index_buffer(&mut pass, slice, wgpu::IndexFormat::Uint32);

        assert_eq!(pass.bind_groups, vec![0, 3]);
        assert_eq!(pass.vertex_buffers, vec![0, 2]);
        assert_eq!(
            pass.index_buffers,
            vec![wgpu::IndexFormat::Uint16, wgpu::IndexFormat::Uint32]
        );
    }

    #[test]
    fn empty_scene_reports_empty() {
        let scene = Scene::new();
        assert!(scene.is_empty());
    }

    /// Recycling an empty scene and borrowing it back must hand the same
    /// allocation to the next frame, so a steady scene never allocates.
    #[test]
    fn recycle_and_reborrow_keep_the_allocation() {
        let (device, _queue) = crate::util::test::noop_device();
        let pipeline = noop_pipeline(&device);

        let mut scene = Scene::new();
        scene.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..3)));
        scene.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..6)));
        let ptr = scene.draws.as_ptr();
        let cap = scene.draws.capacity();

        let scene = scene.recycle();
        assert!(scene.is_empty());
        assert_eq!(scene.draws.as_ptr(), ptr);
        assert_eq!(scene.draws.capacity(), cap);

        let mut next = scene.reborrow();
        assert!(next.is_empty());
        assert_eq!(next.draws.as_ptr(), ptr);
        assert_eq!(next.draws.capacity(), cap);

        next.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..9)));
        let recycled = next.recycle();
        assert_eq!(recycled.draws.as_ptr(), ptr);
        assert_eq!(recycled.draws.capacity(), cap);
    }

    #[test]
    fn pass_state_capacity_covers_the_wgpu_defaults() {
        // The fixed arrays must have room for everything a default device
        // accepts, otherwise the recording asserts would fire on valid scenes.
        assert!(MAX_BIND_GROUPS >= wgpu::Limits::default().max_bind_groups as usize);
        assert!(MAX_VERTEX_BUFFERS >= wgpu::Limits::default().max_vertex_buffers as usize);
    }
}
