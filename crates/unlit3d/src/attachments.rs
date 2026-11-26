//! The depth and multisample attachments a frame draws with.
//!
//! A frame's color attachment is not always the caller's to keep: a swap chain
//! hands out a new image every frame. The depth and multisample attachments do
//! not change that way — they follow the target's size and sample count — so
//! [`FrameAttachments`] owns them, keeps them registered in the frame's
//! resource graph, and rebuilds them when the target changes size.
//!
//! The caller pairs them with the color view it holds — a swap chain image, or
//! a persistent offscreen texture — by stating a
//! [`FramePass`](crate::pass::FramePass) with
//! [`FramePass::set_attachments`](crate::pass::FramePass::set_attachments),
//! which it calls again whenever the target changes. A
//! caller that owns its own attachment set reads [`FrameAttachments::depth_view`]
//! and [`FrameAttachments::msaa_view`] and states the pass's attachments itself.
//! The pass is also what owns the frame's letterbox, so a caller that scales
//! content states it there rather than on the target.
//!
//! A frame whose sources declare no depth state must have no depth attachment:
//! wgpu requires a pipeline's depth state to match the pass's attachments
//! exactly. [`AttachmentDesc::depth`] decides whether one is allocated at all
//! and how it keeps its contents, and [`FrameAttachments::depth_view`] reports
//! it.

use unlit_ecs::World;
use unlit_wgpu::render_attachments::{create_depth_target, create_msaa_target};
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};

use crate::source::RenderContext;

/// How a depth attachment keeps its contents between passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthAttachment {
    /// The contents are discarded after each pass, so the texture is allocated
    /// transient and costs no memory once the target is drawn into.
    Transient,
    /// The contents stay readable, so a pass on this target may open with
    /// [`LoadOp::Load`](wgpu::LoadOp). The texture is stored rather than
    /// discarded, which costs the memory of a full depth buffer.
    Persistent,
}

/// What a frame's attachments are made of, besides the color one the caller
/// holds.
///
/// This is what [`FrameAttachments::new`] takes: one value describing the
/// target, rather than a run of arguments whose order a caller has to
/// remember and whose neighbouring booleans read the same at a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentDesc {
    /// The color attachment's format, which a multisample attachment resolves
    /// into.
    pub format: wgpu::TextureFormat,
    /// The target's size, in physical pixels.
    pub size: (u32, u32),
    /// How many samples every attachment carries. `1` allocates no
    /// multisample attachment, because a single-sampled pass resolves into
    /// nothing.
    pub sample_count: u32,
    /// The depth attachment, or `None` for a frame whose sources declare no
    /// depth state.
    pub depth: Option<DepthAttachment>,
}

/// The depth and multisample attachments a frame draws with, allocated and
/// kept registered in the frame's resource graph.
///
/// They are created for a target of a given size, format and sample count and
/// rebuilt by [`Self::resize`] when the target changes size. The color
/// attachment is the caller's; a pass pairs the two with
/// [`FramePass::set_attachments`](crate::pass::FramePass::set_attachments).
pub struct FrameAttachments {
    /// The frame's context, whose device allocates the textures and whose graph
    /// holds their views.
    ctx: RenderContext,
    /// The format the multisample attachment carries: the color attachment's.
    format: wgpu::TextureFormat,
    /// The sample count the attachments are allocated with.
    sample_count: u32,
    /// Whether the depth attachment keeps its previous contents readable, so
    /// a pass may load them. A resized target rebuilds its depth texture the
    /// same way.
    persistent_depth: bool,
    /// The depth-stencil view, in the frame's graph; `None` when the frame
    /// carries no depth attachment.
    pub depth_view: Option<ResHandle<TextureView>>,
    /// The multisample view, in the frame's graph; `None` when the frame is
    /// not multisampled.
    pub msaa_view: Option<ResHandle<TextureView>>,
}

impl FrameAttachments {
    /// Allocate the depth and multisample attachments for a `size`-pixel
    /// target, and register their views in the frame's graph.
    ///
    /// A depth-stencil texture is allocated only when the description names
    /// one, and a multisample one only when `sample_count > 1`. See
    /// [`AttachmentDesc`] for what each part of the description decides.
    pub fn new(world: &World, ctx: RenderContext, desc: AttachmentDesc) -> Self {
        let AttachmentDesc {
            format,
            size,
            sample_count,
            depth,
        } = desc;
        let persistent_depth = depth == Some(DepthAttachment::Persistent);
        let device = ctx
            .device(&world.root_scope())
            .expect("the context's device resource exists");
        let mut graph_holder = ctx.graph(&world.root_scope());
        let graph = graph_holder.get();
        let depth_view = depth.is_some().then(|| {
            let texture =
                create_depth_target(&device, size.0, size.1, sample_count, !persistent_depth);
            graph.insert(
                TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
                None,
            )
        });
        let msaa_view = (sample_count > 1).then(|| {
            let texture = create_msaa_target(&device, format, size.0, size.1, sample_count);
            graph.insert(
                TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
                None,
            )
        });
        Self {
            ctx,
            format,
            sample_count,
            persistent_depth,
            depth_view,
            msaa_view,
        }
    }

    /// The size the attachments are allocated at, in physical pixels.
    ///
    /// It is the target's own size, which the depth and multisample views
    /// span. A pass that letterboxes states the region its content occupies
    /// on the pass, not here.
    ///
    /// The size is read from a view rather than stored, so there is one answer
    /// to how big the target is: the one the view the pass draws into carries.
    /// `None` when this set has no depth or multisample view to read it from,
    /// or when their size cannot be resolved out of the frame's graph.
    #[must_use]
    pub fn size(&self, world: &World) -> Option<(u32, u32)> {
        let id = self.msaa_view.as_ref().or(self.depth_view.as_ref())?;
        let mut graph_row = self.ctx.try_graph(&world.root_scope()).ok()?;
        let view = graph_row.get().get(id)?;
        let texture = view.texture();
        Some((texture.width(), texture.height()))
    }

    /// Rebuild the attachments for a new target size.
    ///
    /// A no-op when the size is unchanged. The views keep their ids, so
    /// anything holding one — the renderer's bound target, most of all —
    /// follows the replacement. The graph is maintained before returning, since
    /// a resize is not followed by a scene rebuild that would maintain it.
    pub fn resize(&mut self, world: &World, size: (u32, u32)) {
        if self.depth_view.is_none() && self.msaa_view.is_none() {
            // Nothing to allocate: the colour attachment is the target's own
            // and follows its own resize.
            return;
        }
        if self.size(world) == Some(size) {
            return;
        }
        let device = self
            .ctx
            .device(&world.root_scope())
            .expect("the context's device resource exists");
        let mut graph_holder = self.ctx.graph(&world.root_scope());
        let graph = graph_holder.get();
        if let Some(id) = self.depth_view.as_ref() {
            let texture = create_depth_target(
                &device,
                size.0,
                size.1,
                self.sample_count,
                !self.persistent_depth,
            );
            graph
                .replace(
                    id,
                    TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
                )
                .expect("the depth view is in the graph");
        }
        if let Some(id) = self.msaa_view.as_ref() {
            let texture =
                create_msaa_target(&device, self.format, size.0, size.1, self.sample_count);
            graph
                .replace(
                    id,
                    TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
                )
                .expect("the multisample view is in the graph");
        }
        graph.maintain();
    }
}
