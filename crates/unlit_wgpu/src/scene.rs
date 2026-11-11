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
//! A draw owns the handles it names — `wgpu` handles are reference-counted, so
//! holding one keeps the resource alive and cloning one is cheap. A scene
//! therefore borrows nothing, which is what lets several scenes be built from
//! one resource graph and then recorded in turn.
//!
//! Scissor rectangles are the exception to "a draw is independent": wgpu has
//! no reset call, so a rectangle set by one draw stays set for every draw
//! after it in the pass. A scene therefore orders its clipped draws last —
//! or sets [`DrawEntry::scissor`] on every draw. In a composite,
//! [`DrawOrder::After`] is that last stretch: nothing outside the trailing
//! tier follows the clipped draws, so their rectangle cannot leak into one.
//! The stencil reference is the
//! same kind of pass state, but it is set on every draw (from
//! [`DrawEntry::stencil_reference`]), so it never leaks from one draw to the
//! next. Both are per-[`Scene`] state: [`Scene::record`] starts from the pass's
//! own defaults, so nothing a scene leaves behind is seen by the next one.
//!
//! The viewport is pass state of the same kind — wgpu has no call that resets
//! it either — but it covers a whole scene rather than one draw, so a scene
//! states it once in [`Scene::viewport`]. A scene that states none draws into
//! whatever region the pass already holds, which is the whole target only if
//! the caller put it there; the frame driver in `unlit3d` does that for every
//! scene that states none, so one scene's letterbox never crops the next one's.
//!
//! [`Scene::record`] lists a single scene's draws in the order they are
//! given. Recording several scenes as one composite —
//! [`Scene::record_composited`] — orders them by [`DrawOrder`] instead:
//! the sequenced draws of every scene first, then the depth-sorted ones of
//! every scene pooled together back-to-front, then the trailing ones. A
//! scene that states no order keeps its place: [`DrawOrder::Before`] is
//! the default, and only draws that join the sorted pool are reordered.

use arrayvec::ArrayVec;
use core::ops::Range;

/// One bind-group slot: the group index and the group to bind there.
pub type BindGroupBinding = (u32, wgpu::BindGroup);

/// One vertex-buffer slot: the slot index, the buffer to bind and the byte
/// range of it the draw reads.
pub type VertexBufferBinding = (u32, wgpu::Buffer, Range<u64>);

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

/// The region of the render target a scene draws into, in pixels.
///
/// Fragments outside it are discarded and the clip space a pipeline writes is
/// stretched across it, which is what letterboxes a scene whose own aspect
/// differs from its target's: the projection keeps the scene's aspect, and the
/// viewport places the result inside the target without distortion.
///
/// Unlike [`ScissorRect`], which clips in whole pixels, a viewport is
/// fractional — a centred letterbox rarely lands on a pixel boundary — so its
/// fields are floats. It is pass state with no reset call, so see
/// [`Scene::viewport`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportRect {
    /// Left edge, in pixels.
    pub x: f32,
    /// Top edge, in pixels.
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
    /// The depth the near plane of the viewport's clip space maps to.
    pub min_depth: f32,
    /// The depth the far plane of the viewport's clip space maps to.
    pub max_depth: f32,
}

impl ViewportRect {
    /// A rectangle covering `width` x `height` pixels from the origin, over the
    /// whole `0.0..=1.0` depth range.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            height,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }

    /// Same rectangle, with its origin moved to (`x`, `y`).
    pub fn at(self, x: f32, y: f32) -> Self {
        Self { x, y, ..self }
    }

    /// Same rectangle, mapped over `min_depth..=max_depth` instead of the whole
    /// depth range.
    pub fn with_depth(self, min_depth: f32, max_depth: f32) -> Self {
        Self {
            min_depth,
            max_depth,
            ..self
        }
    }

    /// The largest rectangle of `aspect` that fits inside a
    /// `target_width` x `target_height` target, centred.
    ///
    /// This is the letterbox: a target wider than `aspect` gets a rectangle
    /// spanning the full height with bars at the sides, a narrower one spans
    /// the full width with bars above and below, and the rectangle's own aspect
    /// is `aspect` either way — so content drawn through it is scaled
    /// uniformly, showing the same picture on any target shape rather than
    /// being stretched to the target's.
    ///
    /// A degenerate target, or an `aspect` that is not a positive finite
    /// number, has no rectangle to fit, so the whole target comes back rather
    /// than a zero-sized region.
    pub fn fit_aspect(aspect: f32, target_width: u32, target_height: u32) -> Self {
        let (width, height) = (target_width as f32, target_height as f32);
        // `is_finite` is what rejects a NaN aspect, which no comparison would.
        if !aspect.is_finite() || aspect <= 0.0 || width <= 0.0 || height <= 0.0 {
            return Self::new(width.max(0.0), height.max(0.0));
        }
        if width / height > aspect {
            let fitted = height * aspect;
            Self::new(fitted, height).at((width - fitted) * 0.5, 0.0)
        } else {
            let fitted = width / aspect;
            Self::new(width, fitted).at(0.0, (height - fitted) * 0.5)
        }
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

/// Where a draw sits in the composite of a pass's scenes.
///
/// [`Scene::record`] runs a scene's draws in the order they are listed and
/// reads none of this. [`Scene::record_composited`] groups the draws of all
/// of its scenes: the [sequenced](DrawOrder::Before) ones first in scene
/// order, then the [sorted](DrawOrder::Sorted) ones of every scene together,
/// ordered back-to-front by their depth, then the
/// [trailing](DrawOrder::After) ones in scene order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawOrder {
    /// Draw in sequence, ahead of the sorted pool. The default: opaque
    /// geometry and anything whose listed order is its own business stays
    /// exactly where its scene put it.
    Before,
    /// Join the pass's depth-sorted pool at `depth`, measured along the
    /// camera's view axis — `Camera::view_depth` in `unlit3d` reports the
    /// same quantity: positive into the screen, so the farthest draw sorts
    /// first. The pool holds the sorted draws of every scene, which is what
    /// lets blended geometry composite by depth instead of by which scene
    /// recorded first. A `NaN` depth is ordered by [`f32::total_cmp`],
    /// which places it relative to the finite depths by the sign bit its
    /// payload carries; the order is total, so it cannot poison the sort.
    Sorted(f32),
    /// Draw in sequence, behind the sorted pool: UI and overlays that sit
    /// above the sorted draws whatever their depth. Clipped draws belong
    /// here too, where nothing unclipped follows them — see
    /// [`DrawEntry::scissor`].
    After,
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
pub struct DrawEntry {
    /// The render pipeline to bind.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind groups, bound at the slots they name. The built-in pipeline uses
    /// index 0 for the camera, frame globals and mesh metadata, index 1 for
    /// the material, and any remaining slots for mesh-level groups.
    pub bind_groups: ArrayVec<BindGroupBinding, MAX_BIND_GROUPS>,
    /// Vertex buffers, bound at the slots they name.
    pub vertex_buffers: ArrayVec<VertexBufferBinding, MAX_VERTEX_BUFFERS>,
    /// Index buffer, the byte range read from it and its format, when the draw
    /// is indexed.
    pub index_buffer: Option<(wgpu::Buffer, Range<u64>, wgpu::IndexFormat)>,
    /// Pixels outside this rectangle are discarded.
    ///
    /// A draw that sets it clips in the rasterizer instead of in the geometry.
    /// `None` means "leave whatever the pass already has", **not** "cover the
    /// whole target": wgpu's scissor is pass state with no reset call, so
    /// once a draw sets one it stays set for every later draw in the pass —
    /// ordering a clipped draw last is the caller's job, which in a composite
    /// is what [`DrawOrder::After`] is for.
    pub scissor: Option<ScissorRect>,
    /// The stencil reference value the draw tests against.
    ///
    /// Set on every draw (defaulting to `0`, the pass's own default), so a
    /// draw that does not name one resets a value an earlier draw left behind.
    pub stencil_reference: u32,
    /// Where the draw sits in the composite of a pass's scenes.
    ///
    /// [`Scene::record`] ignores it. [`Scene::record_composited`] groups
    /// its scenes' draws by it; the default, [`DrawOrder::Before`], keeps
    /// the draw where its scene listed it.
    pub order: DrawOrder,
    /// What to draw.
    pub range: DrawRange,
}

impl DrawEntry {
    /// A draw of `range` with the given pipeline, no bind groups, no vertex
    /// buffers, no index buffer, no scissor and a zero stencil reference; fill
    /// in the fields the pipeline needs.
    pub fn new(pipeline: &wgpu::RenderPipeline, range: DrawRange) -> Self {
        Self {
            pipeline: pipeline.clone(),
            bind_groups: ArrayVec::new(),
            vertex_buffers: ArrayVec::new(),
            index_buffer: None,
            scissor: None,
            stencil_reference: 0,
            order: DrawOrder::Before,
            range,
        }
    }

    /// Bind `bind_group` at `index`.
    pub fn with_bind_group(mut self, index: u32, bind_group: &wgpu::BindGroup) -> Self {
        self.bind_groups.push((index, bind_group.clone()));
        self
    }

    /// Bind `buffer` whole at vertex-buffer `slot`.
    pub fn with_vertex_buffer(mut self, slot: u32, buffer: &wgpu::Buffer) -> Self {
        self.vertex_buffers
            .push((slot, buffer.clone(), 0..buffer.size()));
        self
    }

    /// Bind `range` of `buffer` at vertex-buffer `slot`.
    pub fn with_vertex_buffer_range(
        mut self,
        slot: u32,
        buffer: &wgpu::Buffer,
        range: Range<u64>,
    ) -> Self {
        self.vertex_buffers.push((slot, buffer.clone(), range));
        self
    }

    /// Discard fragments outside `scissor`. It stays set for every draw after
    /// this one in the pass, so see [`DrawEntry::scissor`].
    pub fn with_scissor(mut self, scissor: ScissorRect) -> Self {
        self.scissor = Some(scissor);
        self
    }

    /// Bind `buffer` as the index buffer, reading all of it.
    pub fn with_index_buffer(mut self, buffer: &wgpu::Buffer, format: wgpu::IndexFormat) -> Self {
        self.index_buffer = Some((buffer.clone(), 0..buffer.size(), format));
        self
    }

    /// Place the draw in the composite of a pass's scenes; see [`DrawOrder`].
    pub fn with_order(mut self, order: DrawOrder) -> Self {
        self.order = order;
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
/// one; across scenes, [`DrawOrder`] and [`Scene::record_composited`]
/// take that job over.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// The draws to run, in order.
    pub draws: Vec<DrawEntry>,
    /// The region of the target every draw is recorded into, or `None` to
    /// leave the pass's own viewport alone.
    ///
    /// Whole-scene state rather than per-draw: wgpu's viewport has no reset
    /// call, so a scene that letterboxes itself states one here and a scene
    /// that draws full-target states none.
    pub viewport: Option<ViewportRect>,
}

impl Scene {
    /// An empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// The region the scene records into when `fallback` may stand in for the
    /// viewport the scene itself states.
    ///
    /// Split out from [`Scene::record`] so the choice is testable without a
    /// render pass: the recording needs a real one, and which region a scene
    /// ends up with is the rule that matters.
    fn resolved_viewport(&self, fallback: Option<ViewportRect>) -> Option<ViewportRect> {
        self.viewport.or(fallback)
    }

    /// Record the scene into `pass`, drawing into `fallback` when the scene
    /// states no viewport of its own.
    ///
    /// wgpu has no call that resets a viewport, so a caller recording several
    /// scenes into one pass has to pass the whole target as `fallback`: a scene
    /// that states none would otherwise inherit the region the previous scene
    /// set. `None` leaves the pass's region exactly as it is, for a caller that
    /// set one itself.
    pub fn record(&self, pass: &mut wgpu::RenderPass<'_>, fallback: Option<ViewportRect>) {
        let mut state = PassState::default();
        if let Some(viewport) = self.resolved_viewport(fallback) {
            state.set_viewport(pass, viewport);
        }
        for draw in &self.draws {
            record_draw(pass, &mut state, draw);
        }
    }

    /// Record the draws of several scenes into `pass` as one composite,
    /// drawing into `fallback` where a scene states no viewport of its own.
    ///
    /// The scenes' draws are grouped by their [`DrawOrder`] rather than kept
    /// in scene order: the sequenced ones of every scene first, then the
    /// depth-sorted ones of every scene pooled together back-to-front, then
    /// the trailing ones. The pool spanning scenes is what lets blended
    /// geometry from one source composite by depth against another's, instead
    /// of by which scene recorded first. A scene whose draws state no order
    /// keeps its place, so a caller that never tags a draw sees the frame
    /// [`Scene::record`] would have given.
    ///
    /// Every draw draws into the region its own scene resolves to — the
    /// scene's [`Scene::viewport`] or `fallback` — so the pool interleaving
    /// scenes never moves a draw out of its scene's letterbox; the
    /// deduplicated `set_*` recording keeps the repeated regions from
    /// repeating the calls.
    pub fn record_composited(
        pass: &mut wgpu::RenderPass<'_>,
        scenes: &[&Scene],
        fallback: Option<ViewportRect>,
    ) {
        let mut state = PassState::default();
        record_composited_into(pass, &mut state, scenes, fallback);
    }
}

/// Records one draw: the state it changes, then the draw call itself.
///
/// The body of [`Scene::record`]'s loop, split out so the composite recorder
/// runs the same state-reuse rules over an interleaving of several scenes'
/// draws.
fn record_draw<'a, P: RenderPassInterface<'a>>(
    pass: &mut P,
    state: &mut PassState<'a>,
    draw: &'a DrawEntry,
) {
    if state.pipeline != Some(&draw.pipeline) {
        pass.set_pipeline(&draw.pipeline);
        state.pipeline = Some(&draw.pipeline);
    }
    for (index, bind_group) in &draw.bind_groups {
        state.set_bind_group(pass, *index, bind_group);
    }
    for (slot, buffer, range) in &draw.vertex_buffers {
        state.set_vertex_buffer(pass, *slot, buffer, range.clone());
    }
    if let Some((buffer, range, format)) = &draw.index_buffer {
        state.set_index_buffer(pass, buffer, range.clone(), *format);
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

/// A depth-sorted draw waiting for the pool to be complete.
///
/// The pool interleaves scenes, so the draw carries the region its own scene
/// resolves to rather than relying on the pass holding it.
struct SortedDraw<'a> {
    draw: &'a DrawEntry,
    depth: f32,
    viewport: Option<ViewportRect>,
}

/// The composite recorder behind [`Scene::record_composited`], generic over
/// the pass so tests can drive it with a mock.
///
/// Three linear passes over the scenes, and a sort over only the pool: the
/// sequenced and trailing draws are emitted in scene order without being
/// compared, so an untagged scene's batching survives the composite, and a
/// frame whose draws all state no order never sorts at all.
fn record_composited_into<'a, P: RenderPassInterface<'a>>(
    pass: &mut P,
    state: &mut PassState<'a>,
    scenes: &[&'a Scene],
    fallback: Option<ViewportRect>,
) {
    // The sequenced draws first, in scene order: opaque geometry keeps the
    // order its scene listed, which is the order its batching is arranged
    // for.
    for scene in scenes {
        let viewport = scene.resolved_viewport(fallback);
        for draw in &scene.draws {
            match draw.order {
                DrawOrder::Before => {
                    if let Some(viewport) = viewport {
                        state.set_viewport(pass, viewport);
                    }
                    record_draw(pass, state, draw);
                }
                DrawOrder::Sorted(_) | DrawOrder::After => {}
            }
        }
    }

    // The depth-sorted pool: the sorted draws of every scene, ordered
    // back-to-front. Farthest first is the order translucent colours layer
    // in; the sort is stable, so draws that tie on depth keep the order
    // their scenes listed them in.
    let mut pool: Vec<SortedDraw<'_>> = Vec::new();
    for scene in scenes {
        for draw in &scene.draws {
            if let DrawOrder::Sorted(depth) = draw.order {
                pool.push(SortedDraw {
                    draw,
                    depth,
                    viewport: scene.resolved_viewport(fallback),
                });
            }
        }
    }
    pool.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    for sorted in &pool {
        if let Some(viewport) = sorted.viewport {
            state.set_viewport(pass, viewport);
        }
        record_draw(pass, state, sorted.draw);
    }

    // The trailing draws last, in scene order: UI and overlays sit above the
    // sorted pool whatever their depth, and a clipped draw recorded here is
    // followed by nothing its rectangle could leak into.
    for scene in scenes {
        let viewport = scene.resolved_viewport(fallback);
        for draw in &scene.draws {
            match draw.order {
                DrawOrder::After => {
                    if let Some(viewport) = viewport {
                        state.set_viewport(pass, viewport);
                    }
                    record_draw(pass, state, draw);
                }
                DrawOrder::Before | DrawOrder::Sorted(_) => {}
            }
        }
    }
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
    /// Bind `range` of `buffer` at vertex-buffer `slot`.
    fn set_vertex_buffer(&mut self, slot: u32, buffer: &'a wgpu::Buffer, range: Range<u64>);
    /// Bind `range` of `buffer` as the index buffer.
    fn set_index_buffer(
        &mut self,
        buffer: &'a wgpu::Buffer,
        range: Range<u64>,
        format: wgpu::IndexFormat,
    );
    /// Set the scissor rectangle.
    fn set_scissor_rect(&mut self, scissor: ScissorRect);
    /// Set the viewport.
    fn set_viewport_rect(&mut self, viewport: ViewportRect);
    /// Set the stencil reference.
    fn set_stencil_reference(&mut self, reference: u32);
    /// Bind the pipeline the draws rasterize with.
    fn set_pipeline(&mut self, pipeline: &'a wgpu::RenderPipeline);
    /// Draw a non-indexed range of vertices.
    fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>);
    /// Draw an indexed range of vertices.
    fn draw_indexed(&mut self, indices: Range<u32>, base_vertex: i32, instances: Range<u32>);
}

impl<'a, 'p> RenderPassInterface<'a> for wgpu::RenderPass<'p> {
    fn set_bind_group(&mut self, index: u32, bind_group: &'a wgpu::BindGroup) {
        wgpu::RenderPass::set_bind_group(self, index, bind_group, &[]);
    }

    fn set_vertex_buffer(&mut self, slot: u32, buffer: &'a wgpu::Buffer, range: Range<u64>) {
        wgpu::RenderPass::set_vertex_buffer(self, slot, buffer.slice(range));
    }

    fn set_index_buffer(
        &mut self,
        buffer: &'a wgpu::Buffer,
        range: Range<u64>,
        format: wgpu::IndexFormat,
    ) {
        wgpu::RenderPass::set_index_buffer(self, buffer.slice(range), format);
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

    fn set_viewport_rect(&mut self, viewport: ViewportRect) {
        wgpu::RenderPass::set_viewport(
            self,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            viewport.min_depth,
            viewport.max_depth,
        );
    }

    fn set_stencil_reference(&mut self, reference: u32) {
        wgpu::RenderPass::set_stencil_reference(self, reference);
    }

    fn set_pipeline(&mut self, pipeline: &'a wgpu::RenderPipeline) {
        wgpu::RenderPass::set_pipeline(self, pipeline);
    }

    fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>) {
        wgpu::RenderPass::draw(self, vertices, instances);
    }

    fn draw_indexed(&mut self, indices: Range<u32>, base_vertex: i32, instances: Range<u32>) {
        wgpu::RenderPass::draw_indexed(self, indices, base_vertex, instances);
    }
}

/// Which resources the previous draw left bound.
///
/// The entries borrow from the draws being recorded, which outlive the
/// recording, so tracking them costs no clones. `wgpu` resources compare by
/// identity, so a plain `==` is the fast "is this the same resource as last
/// time" test the recording loop needs. The slots are fixed-capacity
/// ([`MAX_BIND_GROUPS`] / [`MAX_VERTEX_BUFFERS`]) so recording a frame never
/// allocates.
#[derive(Default)]
struct PassState<'a> {
    pipeline: Option<&'a wgpu::RenderPipeline>,
    bind_groups: ArrayVec<(u32, &'a wgpu::BindGroup), MAX_BIND_GROUPS>,
    vertex_buffers: ArrayVec<(u32, &'a wgpu::Buffer, Range<u64>), MAX_VERTEX_BUFFERS>,
    index_buffer: Option<(&'a wgpu::Buffer, Range<u64>, wgpu::IndexFormat)>,
    scissor: Option<ScissorRect>,
    viewport: Option<ViewportRect>,
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

    /// Vertex-buffer slots are keyed by slot, and their value is the buffer
    /// and the byte range bound to it.
    fn set_vertex_buffer(
        &mut self,
        pass: &mut impl RenderPassInterface<'a>,
        slot: u32,
        buffer: &'a wgpu::Buffer,
        range: Range<u64>,
    ) {
        if let Some(bound) = self
            .vertex_buffers
            .iter_mut()
            .find(|(bound, _, _)| *bound == slot)
        {
            if core::ptr::eq(bound.1, buffer) && bound.2 == range {
                return;
            }
            *bound = (slot, buffer, range.clone());
        } else {
            assert!(
                slot < MAX_VERTEX_BUFFERS as u32,
                "vertex buffer slot {slot} exceeds MAX_VERTEX_BUFFERS ({MAX_VERTEX_BUFFERS})"
            );
            self.vertex_buffers.push((slot, buffer, range.clone()));
        }
        pass.set_vertex_buffer(slot, buffer, range);
    }

    fn set_index_buffer(
        &mut self,
        pass: &mut impl RenderPassInterface<'a>,
        buffer: &'a wgpu::Buffer,
        range: Range<u64>,
        format: wgpu::IndexFormat,
    ) {
        if let Some((bound, bound_range, bound_format)) = &self.index_buffer
            && core::ptr::eq(*bound, buffer)
            && *bound_range == range
            && *bound_format == format
        {
            return;
        }
        pass.set_index_buffer(buffer, range.clone(), format);
        self.index_buffer = Some((buffer, range, format));
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

    /// Set the viewport, skipping the call when the pass already holds that
    /// rectangle.
    fn set_viewport(&mut self, pass: &mut impl RenderPassInterface<'a>, viewport: ViewportRect) {
        if self.viewport == Some(viewport) {
            return;
        }
        self.viewport = Some(viewport);
        pass.set_viewport_rect(viewport);
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
mod tests;
