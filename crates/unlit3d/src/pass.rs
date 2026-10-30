//! A pass of a frame: where one group of sources draws, and what it draws
//! into.
//!
//! A frame is made of passes, recorded in [`FramePassOrder`]. Each pass is an
//! ECS entity carrying a [`FramePass`], and the sources under it — the
//! entities in its own scope — record into it, in
//! [`FrameSourceOrder`](crate::source::FrameSourceOrder). A pass therefore
//! states both halves of a frame's shape: how the frame is split up, and where
//! each part of it draws.
//!
//! A pass names its attachments as texture-view resources in the frame's
//! resource graph. It owns none of them: the color attachment is the caller's
//! — a swap chain image, or a persistent offscreen texture — and the depth and
//! multisample ones come from a [`FrameAttachments`], which a caller states
//! with [`FramePass::set_attachments`]. A pass with no attachment at all draws
//! nothing, which is how a caller parks one while its target is gone.
//!
//! What a pass loads and clears is a [`RenderLoadOps`] on the same entity
//! rather than a field here: it is the one part of a pass a tool may want to
//! set from outside, so it stays a component of its own, reflected like the
//! rest. [`Pass`] spawns the two as one object. A pass entity that carries no
//! load ops of its own is opened with [`RenderLoadOps::default`], which is why
//! a bare [`FramePass`] still records.
//!
//! [`PassInfo`] is what a pass tells its sources: the target's specialization
//! key, the target's size, and the region of it the pass letterboxes to.

use unlit_ecs::{Bundle, DynamicBundle, World, bundle};
use unlit_wgpu::render_attachments::RenderAttachments;
use unlit_wgpu::resources::{ResHandle, ResourceGraph, TextureView};
use unlit_wgpu::scene::ViewportRect;
use unlit_wgpu::specialize::SurfaceKey;

use crate::attachments::FrameAttachments;
use crate::components::RenderLoadOps;
use crate::order::OrderKey;
use crate::source::RenderContext;

/// When a pass records, relative to the others.
///
/// Lower records first. Deliberately has no `Default`, and the library defines
/// no values of its own: a caller states the shape of its own frame, and a
/// silent default or a built-in constant would be a shape it never asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FramePassOrder(pub i32);

impl OrderKey for FramePassOrder {
    const ORDER: &'static str = "FramePassOrder";
    const SUBJECT: &'static str = "passes";

    fn value(self) -> i32 {
        self.0
    }
}

/// One pass of a frame, as an ECS component.
///
/// Spawn it on an entity and hang sources below it: the pass records the
/// scenes of every [`Source`](crate::source::Source) in its own scope, so a
/// source's place in the entity tree is what chooses its pass. The passes
/// themselves record in [`FramePassOrder`], then by entity.
///
/// The depth and multisample attachments are taken from a
/// [`FrameAttachments`] with [`Self::set_attachments`]; the color attachment
/// is the caller's.
///
/// The load ops are a
/// [`RenderLoadOps`] on the same entity, so
/// spawning a pass states both halves at once — which is what [`Pass`] does:
///
/// ```rust
/// # use unlit3d::prelude::*;
/// let mut world = World::new();
/// let pass = world.root_scope_mut().spawn(Pass::new(FramePassOrder(0)));
/// # let _ = pass;
/// ```
#[derive(Debug)]
pub struct FramePass {
    /// The pass's order among the frame's passes; see [`FramePassOrder`].
    pub order: FramePassOrder,
    /// The region the pass's sources are recorded into, or the whole target
    /// when `None`.
    ///
    /// This is how a frame letterboxes content whose own aspect differs from
    /// its target's: the pass states the rectangle, and every source that
    /// records into it is told about it through [`PassInfo::viewport`] — so
    /// the content is scaled uniformly and the same picture appears on any
    /// target shape. A source that composes over the content rather than being
    /// part of it, a UI overlay for instance, ignores it and covers the whole
    /// target.
    pub viewport: Option<ViewportRect>,
    color: Option<ResHandle<TextureView>>,
    depth: Option<ResHandle<TextureView>>,
    msaa: Option<ResHandle<TextureView>>,
}

impl FramePass {
    /// A pass recording at `order`, whose attachments are not stated yet.
    ///
    /// The pass draws nowhere until it is given a color attachment, which is
    /// also when its load ops start to matter.
    pub fn new(order: FramePassOrder) -> Self {
        Self {
            order,
            viewport: None,
            color: None,
            depth: None,
            msaa: None,
        }
    }

    /// Take the depth and multisample attachments from `attachments` and draw
    /// into `color_view` from the next frame on.
    ///
    /// This is how a frame loop follows a swap chain: the image it draws into
    /// changes every frame, while the pass it states it with stays the same
    /// entity. `color_view` must be in the frame's resource graph, like the
    /// attachments' own views.
    pub fn set_attachments(
        &mut self,
        attachments: &FrameAttachments,
        color_view: ResHandle<TextureView>,
    ) {
        self.color = Some(color_view);
        self.depth = attachments.depth_view.clone();
        self.msaa = attachments.msaa_view.clone();
    }

    /// Draw nowhere until an attachment is stated again.
    ///
    /// A parked pass records nothing, which is what a frame loop states while
    /// its target is gone — a minimized window, say — without disturbing the
    /// sources under it.
    pub fn park(&mut self) {
        self.color = None;
        self.depth = None;
        self.msaa = None;
    }

    /// The attachments this pass records into, read out of the frame's graph.
    ///
    /// `None` when the pass has none to draw into. The views are cloned out of
    /// the graph, so the returned set borrows nothing from the world.
    ///
    /// # Panics
    ///
    /// If one of the pass's views is gone from the graph, or if it is not a
    /// texture view.
    pub(crate) fn attachments(
        &self,
        world: &World,
        ctx: RenderContext,
    ) -> Option<RenderAttachments> {
        let mut resource_graph_row = world
            .root_scope()
            .query_one::<&mut ResourceGraph>(ctx.graph)
            .expect("the context's resource graph exists");

        let graph = resource_graph_row.get();
        let view = |handle: &Option<ResHandle<TextureView>>| {
            handle
                .as_ref()
                .map(|id| graph.get(id).expect("pass attachment view").clone())
        };
        let (color, depth, msaa) = (view(&self.color), view(&self.depth), view(&self.msaa));
        // Any stated view means the pass is bound: a multisample view without a
        // color one is a mistake, and `from_views` says so rather than the pass
        // silently parking itself.
        (color.is_some() || depth.is_some() || msaa.is_some())
            .then(|| RenderAttachments::from_views(color, depth, msaa))
    }
}

/// A pass as one entity: the [`FramePass`] itself and the
/// [`RenderLoadOps`] it opens the frame with.
///
/// Spawning it is how a caller states a pass; the two components land on the
/// same entity, which is what makes the load ops reachable on their own — a
/// tool can patch them without touching the pass.
///
/// State the load ops of your own with a struct update:
///
/// ```rust
/// # use unlit3d::prelude::*;
/// let load = RenderLoadOps { depth: wgpu::LoadOp::Load, ..RenderLoadOps::default() };
/// let pass = Pass { load, ..Pass::new(FramePassOrder(0)) };
/// # let _ = pass;
/// ```
pub struct Pass {
    /// The pass itself, named after its type so the two are not read as one.
    pub frame_pass: FramePass,
    /// What the pass loads and clears when it opens the frame.
    pub load: RenderLoadOps,
}

impl Pass {
    /// A pass recording at `order`, clearing every attachment it draws into.
    #[must_use]
    pub fn new(order: FramePassOrder) -> Self {
        Self {
            frame_pass: FramePass::new(order),
            load: RenderLoadOps::default(),
        }
    }
}

impl Bundle for Pass {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.frame_pass, self.load]
    }
}

/// What a pass tells each of its sources about the frame.
///
/// A source has no way to reach the pass it is recorded by — the pass is an
/// ancestor entity, and the source is handed its own scope — so this is how it
/// learns what it is drawing into and where in that target it belongs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PassInfo {
    /// The target's pipeline specialization key.
    pub surface: SurfaceKey,
    /// The target's width, in physical pixels.
    pub width: u32,
    /// The target's height, in physical pixels.
    pub height: u32,
    /// The region of the target the pass letterboxes to, if it states one.
    pub viewport: Option<ViewportRect>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_spawns_its_load_ops_on_the_same_entity() {
        let mut world = World::new();
        let load = RenderLoadOps {
            depth: wgpu::LoadOp::Load,
            ..RenderLoadOps::default()
        };
        let entity = world.root_scope_mut().spawn(Pass {
            load,
            ..Pass::new(FramePassOrder(7))
        });
        let root = world.root_scope();
        let mut frame_pass_row = root
            .query_one::<&FramePass>(entity)
            .expect("the pass is on the entity");

        let pass = frame_pass_row.get();
        assert_eq!(pass.order, FramePassOrder(7));
        let mut render_load_ops_row = root
            .query_one::<&RenderLoadOps>(entity)
            .expect("the load ops are on the same entity");

        let spawned = render_load_ops_row.get();
        assert_eq!(spawned.depth, wgpu::LoadOp::Load);
    }
}
