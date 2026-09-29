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
//! or sets [`DrawEntry::scissor`] on every draw. The stencil reference is the
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

/// The largest viewport a target can hold: the whole of it.
///
/// A scene that states none is left with whatever the pass holds, and the
/// frame driver in `unlit3d` sets this before such a scene records, so the
/// default a scene comes out to is the whole target.
#[must_use]
pub fn full_viewport(width: u32, height: u32) -> ViewportRect {
    ViewportRect::new(width as f32, height as f32)
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

    /// Test against stencil reference `reference`.
    pub fn with_stencil_reference(mut self, reference: u32) -> Self {
        self.stencil_reference = reference;
        self
    }

    /// Bind `buffer` as the index buffer, reading all of it.
    pub fn with_index_buffer(mut self, buffer: &wgpu::Buffer, format: wgpu::IndexFormat) -> Self {
        self.index_buffer = Some((buffer.clone(), 0..buffer.size(), format));
        self
    }

    /// Bind `range` of `buffer` as the index buffer.
    pub fn with_index_buffer_range(
        mut self,
        buffer: &wgpu::Buffer,
        range: Range<u64>,
        format: wgpu::IndexFormat,
    ) -> Self {
        self.index_buffer = Some((buffer.clone(), range, format));
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
pub struct Scene {
    /// The draws to run, in order.
    pub draws: Vec<DrawEntry>,
    /// The region of the target every draw is recorded into, or `None` to
    /// leave the pass's own viewport alone.
    ///
    /// Whole-scene state rather than per-draw: wgpu's viewport has no reset
    /// call, so a scene that letterboxes itself states one here and a scene
    /// that draws full-target states none — see [`Scene::viewport`].
    viewport: Option<ViewportRect>,
}

impl Scene {
    /// An empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `draw` to this scene.
    pub fn push(&mut self, draw: DrawEntry) {
        self.draws.push(draw);
    }

    /// Append `draw` to this scene and return it.
    pub fn with_draw(mut self, draw: DrawEntry) -> Self {
        self.draws.push(draw);
        self
    }

    /// The region of the target this scene is recorded into, or `None` to leave
    /// the pass's own viewport alone.
    ///
    /// Recording a scene with no viewport of its own leaves the pass's viewport
    /// exactly as it was, which is *not* the same as "cover the whole target":
    /// wgpu has no call that resets a viewport, so a region an earlier scene
    /// set stays set for every scene after it. [`Renderer`] therefore sets the
    /// whole target before recording a scene that states none, so one scene's
    /// letterbox never crops the next one's draws.
    ///
    /// [`Renderer`]: https://docs.rs/unlit3d
    pub fn viewport(&self) -> Option<ViewportRect> {
        self.viewport
    }

    /// Record every draw into `viewport` instead of the pass's own region.
    pub fn set_viewport(&mut self, viewport: Option<ViewportRect>) {
        self.viewport = viewport;
    }

    /// Same scene, recorded into `viewport`.
    #[must_use]
    pub fn with_viewport(mut self, viewport: ViewportRect) -> Self {
        self.viewport = Some(viewport);
        self
    }

    /// Drop every draw, keeping the allocation for the next frame.
    pub fn clear(&mut self) {
        self.draws.clear();
    }

    /// Move every draw of `other` onto the end of this scene.
    ///
    /// `other` is left empty but keeps its own allocation.
    pub fn extend(&mut self, other: &mut Scene) {
        self.draws.append(&mut other.draws);
    }

    /// Whether the scene draws nothing.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// Record the scene into `pass`, skipping any `set_*` call whose target is
    /// already bound from the previous draw.
    ///
    /// The pass state the recording tracks — the scissor rectangle, the
    /// stencil reference and the scene's own viewport — starts from the pass's
    /// defaults and ends with this scene, so recording a second scene into the
    /// same pass does not inherit the first one's clips. A scene that states no
    /// viewport leaves the pass's region exactly as it is; a caller that needs
    /// a region for it uses [`Scene::record_with_viewport`].
    pub fn record(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.record_in(pass, None);
    }

    /// Record the scene into `pass` as [`Scene::record`] does, drawing into
    /// `fallback` when the scene states no viewport of its own.
    ///
    /// A caller recording several scenes into one pass needs this: wgpu has no
    /// call that resets a viewport, so a scene that states none would otherwise
    /// inherit the region the previous scene set. Passing the whole target as
    /// the fallback gives each scene that does not letterbox the full target
    /// back — which is what `unlit3d`'s frame driver does.
    pub fn record_with_viewport(&self, pass: &mut wgpu::RenderPass<'_>, fallback: ViewportRect) {
        self.record_in(pass, Some(fallback));
    }

    /// The region the scene records into when `fallback` may stand in for the
    /// viewport the scene itself states.
    ///
    /// Split out from [`Scene::record_in`] so the choice is testable without a
    /// render pass: the recording needs a real one, and which region a scene
    /// ends up with is the rule that matters.
    fn resolved_viewport(&self, fallback: Option<ViewportRect>) -> Option<ViewportRect> {
        self.viewport.or(fallback)
    }

    /// The shared body of [`Scene::record`] and [`Scene::record_with_viewport`].
    fn record_in(&self, pass: &mut wgpu::RenderPass<'_>, fallback: Option<ViewportRect>) {
        let mut state = PassState::default();
        if let Some(viewport) = self.resolved_viewport(fallback) {
            state.set_viewport(pass, viewport);
        }
        for draw in &self.draws {
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
            label: Some("test::shader"),
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

    #[test]
    fn viewport_rect_builders_compose() {
        let rect = ViewportRect::new(64.0, 32.0).at(8.5, 16.25);
        assert_eq!(
            rect,
            ViewportRect {
                x: 8.5,
                y: 16.25,
                width: 64.0,
                height: 32.0,
                min_depth: 0.0,
                max_depth: 1.0,
            }
        );
        // The whole target, which is what the driver sets before a scene that
        // states no viewport of its own.
        assert_eq!(full_viewport(320, 240), ViewportRect::new(320.0, 240.0));
        assert_eq!(
            rect.with_depth(0.5, 1.0).min_depth,
            0.5,
            "the depth range is the caller's to narrow"
        );
    }

    /// A letterbox keeps the requested aspect and stays inside the target,
    /// centred, whichever way the target and the aspect differ.
    #[test]
    fn fitting_an_aspect_letterboxes_within_the_target() {
        // A 4:3 target is the aspect itself: no bars, no offset.
        assert_eq!(
            ViewportRect::fit_aspect(4.0 / 3.0, 800, 600),
            ViewportRect::new(800.0, 600.0)
        );

        // A wider target gets bars at the sides: the height is the constraint,
        // and the rectangle is centred horizontally.
        let wide = ViewportRect::fit_aspect(4.0 / 3.0, 1600, 600);
        assert_eq!(wide, ViewportRect::new(800.0, 600.0).at(400.0, 0.0));
        assert!((wide.width / wide.height - 4.0 / 3.0).abs() < 1e-5);
        assert_eq!(wide.x + wide.width + wide.x, 1600.0, "centred horizontally");
        assert_eq!(wide.y, 0.0);

        // A narrower target gets bars above and below, centred vertically.
        let tall = ViewportRect::fit_aspect(4.0 / 3.0, 600, 900);
        assert_eq!(tall, ViewportRect::new(600.0, 450.0).at(0.0, 225.0));
        assert!((tall.width / tall.height - 4.0 / 3.0).abs() < 1e-5);
        assert_eq!(tall.y + tall.height + tall.y, 900.0, "centred vertically");
        assert_eq!(tall.x, 0.0);

        // The depth range is left at the whole of clip space.
        assert_eq!((tall.min_depth, tall.max_depth), (0.0, 1.0));
    }

    /// A target that cannot hold a letterbox — a degenerate one, or an aspect
    /// that is not a positive number — comes back whole rather than empty.
    #[test]
    fn an_impossible_letterbox_is_the_whole_target() {
        for aspect in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                ViewportRect::fit_aspect(aspect, 320, 240),
                ViewportRect::new(320.0, 240.0),
                "aspect {aspect} has no rectangle to fit"
            );
        }
        for target in [(0, 0), (0, 240), (320, 0)] {
            let fitted = ViewportRect::fit_aspect(4.0 / 3.0, target.0, target.1);
            assert!(fitted.width >= 0.0 && fitted.height >= 0.0);
            assert!(fitted.width.is_finite() && fitted.height.is_finite());
        }
    }

    /// A scene's own viewport is recorded, and a scene that states none leaves
    /// the pass's region to whoever set it.
    #[test]
    fn a_scene_records_only_the_viewport_it_states() {
        let own = ViewportRect::new(320.0, 180.0).at(0.0, 20.0);
        let scene = Scene::new().with_viewport(own);
        assert_eq!(scene.viewport(), Some(own));

        // A scene that never states one issues no viewport call of its own: the
        // pass keeps whatever region it holds.
        let plain = Scene::new();
        assert_eq!(plain.viewport(), None);
        assert_eq!(plain.resolved_viewport(None), None);
    }

    /// A fallback region stands in only for a scene that states no viewport of
    /// its own, which is what keeps one scene's letterbox from cropping the
    /// next one's.
    #[test]
    fn a_fallback_viewport_fills_in_for_a_scene_that_states_none() {
        let whole = ViewportRect::new(800.0, 600.0);
        let own = ViewportRect::new(320.0, 180.0).at(0.0, 20.0);

        let plain = Scene::new();
        assert_eq!(plain.resolved_viewport(Some(whole)), Some(whole));

        // The scene's own region wins over the fallback rather than the two
        // being combined.
        let letterboxed = Scene::new().with_viewport(own);
        assert_eq!(letterboxed.resolved_viewport(Some(whole)), Some(own));

        // With no fallback either, there is nothing to set.
        assert_eq!(letterboxed.resolved_viewport(None), Some(own));
    }

    /// Setting the same viewport twice records one call, and a later scene
    /// that states a different one records again rather than inheriting it.
    #[test]
    fn viewport_calls_are_deduplicated() {
        let mut pass = MockPass::default();
        let mut state = PassState::default();
        let first = ViewportRect::new(320.0, 180.0);
        let second = ViewportRect::new(160.0, 90.0);

        state.set_viewport(&mut pass, first);
        // The same rectangle again is a no-op: the pass already has it.
        state.set_viewport(&mut pass, first);
        // A fractional difference is a real one: a centred letterbox rarely
        // lands on a pixel boundary.
        let nudged = ViewportRect::new(320.0, 180.0).at(0.5, 0.5);
        state.set_viewport(&mut pass, nudged);
        state.set_viewport(&mut pass, second);

        assert_eq!(pass.viewports, vec![first, nudged, second]);
        assert_eq!(state.viewport, Some(second));
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
        viewports: Vec<ViewportRect>,
        stencil_references: Vec<u32>,
    }

    impl<'a> RenderPassInterface<'a> for MockPass {
        fn set_bind_group(&mut self, index: u32, _bind_group: &'a wgpu::BindGroup) {
            self.bind_groups.push(index);
        }

        fn set_vertex_buffer(&mut self, slot: u32, _buffer: &'a wgpu::Buffer, _range: Range<u64>) {
            self.vertex_buffers.push(slot);
        }

        fn set_index_buffer(
            &mut self,
            _buffer: &'a wgpu::Buffer,
            _range: Range<u64>,
            format: wgpu::IndexFormat,
        ) {
            self.index_buffers.push(format);
        }

        fn set_scissor_rect(&mut self, scissor: ScissorRect) {
            self.scissors.push(scissor);
        }

        fn set_viewport_rect(&mut self, viewport: ViewportRect) {
            self.viewports.push(viewport);
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
                    min_binding_size: wgpu::BufferSize::new(buffer.size()),
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
        let whole = 0..buffer.size();
        let head = 0..32;
        let tail = 32..64;

        state.set_bind_group(&mut pass, 0, &bind_group);
        state.set_bind_group(&mut pass, 0, &bind_group);
        state.set_bind_group(&mut pass, 3, &bind_group);

        state.set_vertex_buffer(&mut pass, 0, &buffer, whole.clone());
        state.set_vertex_buffer(&mut pass, 0, &buffer, whole.clone());
        // The same buffer under a different range is a different binding, so
        // the call is not skipped.
        state.set_vertex_buffer(&mut pass, 0, &buffer, head.clone());
        state.set_vertex_buffer(&mut pass, 2, &buffer, whole.clone());

        state.set_index_buffer(&mut pass, &buffer, head.clone(), wgpu::IndexFormat::Uint16);
        state.set_index_buffer(&mut pass, &buffer, head.clone(), wgpu::IndexFormat::Uint16);
        // A different range and a different format each re-bind.
        state.set_index_buffer(&mut pass, &buffer, tail.clone(), wgpu::IndexFormat::Uint16);
        state.set_index_buffer(&mut pass, &buffer, tail.clone(), wgpu::IndexFormat::Uint32);

        assert_eq!(pass.bind_groups, vec![0, 3]);
        // Slot 0 is bound whole, skipped as a repeat, re-bound for the
        // narrower range, then slot 2 is bound for the first time.
        assert_eq!(pass.vertex_buffers, vec![0, 0, 2]);
        assert_eq!(
            pass.index_buffers,
            vec![
                wgpu::IndexFormat::Uint16,
                wgpu::IndexFormat::Uint16,
                wgpu::IndexFormat::Uint32
            ]
        );
    }

    #[test]
    fn empty_scene_reports_empty() {
        let scene = Scene::new();
        assert!(scene.is_empty());
    }

    /// Clearing a scene keeps its allocation, so a steady scene rebuilt each
    /// frame does not allocate.
    #[test]
    fn clearing_a_scene_keeps_the_allocation() {
        let (device, _queue) = crate::util::test::noop_device();
        let pipeline = noop_pipeline(&device);

        let mut scene = Scene::new();
        scene.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..3)));
        scene.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..6)));
        let ptr = scene.draws.as_ptr();
        let cap = scene.draws.capacity();

        scene.clear();
        assert!(scene.is_empty());
        assert_eq!(scene.draws.as_ptr(), ptr);
        assert_eq!(scene.draws.capacity(), cap);

        scene.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..9)));
        assert_eq!(scene.draws.as_ptr(), ptr, "the allocation was reused");
    }

    /// Extending one scene with another moves the draws across and leaves the
    /// source empty but still holding its own allocation.
    ///
    /// The target reserves room up front so appending does not have to grow it;
    /// what the test pins is that the draws move and that the source's
    /// allocation survives to be refilled next frame.
    #[test]
    fn extending_moves_the_draws_and_keeps_both_allocations() {
        let (device, _queue) = crate::util::test::noop_device();
        let pipeline = noop_pipeline(&device);

        let mut into = Scene::new();
        into.draws.reserve_exact(3);
        into.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..3)));
        let into_ptr = into.draws.as_ptr();

        let mut from = Scene::new();
        from.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..6)));
        from.push(DrawEntry::new(&pipeline, DrawRange::vertices(0..9)));
        let from_cap = from.draws.capacity();

        into.extend(&mut from);

        assert_eq!(into.draws.len(), 3, "the source's draws moved across");
        assert_eq!(into.draws.as_ptr(), into_ptr, "the target kept its own");
        assert!(from.is_empty(), "the source was emptied");
        assert_eq!(
            from.draws.capacity(),
            from_cap,
            "the source kept its allocation for the next frame"
        );
    }

    #[test]
    fn pass_state_capacity_covers_the_wgpu_defaults() {
        // The fixed arrays must have room for everything a default device
        // accepts, otherwise the recording asserts would fire on valid scenes.
        assert!(MAX_BIND_GROUPS >= wgpu::Limits::default().max_bind_groups as usize);
        assert!(MAX_VERTEX_BUFFERS >= wgpu::Limits::default().max_vertex_buffers as usize);
    }
}
