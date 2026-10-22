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
//! exactly. `with_depth` decides whether one is allocated at all, and
//! [`FrameAttachments::depth_view`] reports it.

use core::ops::DerefMut;

use unlit_ecs::World;
use unlit_wgpu::render_attachments::{create_depth_target, create_msaa_target};
use unlit_wgpu::resources::{ResHandle, ResourceGraph, TextureExt, TextureView};

use crate::source::RenderContext;

/// The device the frame's context was created with, cloned out of `world`.
///
/// # Panics
///
/// If the context's device resource is gone.
fn context_device(world: &World, ctx: RenderContext) -> wgpu::Device {
    world
        .root_scope()
        .query_one::<&wgpu::Device>(ctx.device)
        .expect("the context's device resource exists")
        .clone()
}

/// The frame's resource graph.
///
/// # Panics
///
/// If the context's graph resource is gone.
fn context_graph<'w>(
    world: &'w World,
    ctx: RenderContext,
) -> impl DerefMut<Target = ResourceGraph> + 'w {
    world
        .root_scope()
        .query_one::<&mut ResourceGraph>(ctx.graph)
        .expect("the context's resource graph exists")
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
    /// The size the attachments are allocated at.
    size: (u32, u32),
    /// The sample count the attachments are allocated with.
    sample_count: u32,
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
    /// A depth-stencil texture is allocated only when `with_depth`, and a
    /// multisample one only when `sample_count > 1`. `format` is the color
    /// attachment's, which the multisample attachment resolves into.
    pub fn new(
        world: &World,
        ctx: RenderContext,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        sample_count: u32,
        with_depth: bool,
    ) -> Self {
        let device = context_device(world, ctx);
        let mut graph = context_graph(world, ctx);
        let depth_view = with_depth.then(|| {
            let texture = create_depth_target(&device, size.0, size.1, sample_count);
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
            size,
            sample_count,
            depth_view,
            msaa_view,
        }
    }

    /// Rebuild the attachments for a new target size.
    ///
    /// A no-op when the size is unchanged. The views keep their ids, so
    /// anything holding one — the renderer's bound target, most of all —
    /// follows the replacement. The graph is maintained before returning, since
    /// a resize is not followed by a scene rebuild that would maintain it.
    pub fn resize(&mut self, world: &World, size: (u32, u32)) {
        if size == self.size {
            return;
        }
        self.size = size;
        let device = context_device(world, self.ctx);
        let mut graph = context_graph(world, self.ctx);
        if let Some(id) = self.depth_view.as_ref() {
            let texture = create_depth_target(&device, size.0, size.1, self.sample_count);
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
