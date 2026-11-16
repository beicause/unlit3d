//! A frame source that copies a texture over the pass's target.
//!
//! [`BlitSource`] draws one screen-covering triangle, sampling a texture view
//! it is handed. It is how a frame presents what an earlier pass drew: a first
//! pass renders into an offscreen texture, and a blit source in a later pass
//! copies that texture into the target.
//!
//! The view is the source's own input, so a world decides what to hand it and
//! may swap it between frames through its [`view`][BlitSource::view] field. It
//! lives in the frame's [`ResourceGraph`](unlit_wgpu::resources::ResourceGraph), and the bind group is a node there
//! that a replace of the view rebuilds; the pipeline is specialized on the
//! pass's target through the generic variant cache, like any other source's, so
//! the same source presents into a swap chain or into another offscreen target.

use unlit_ecs::Scope;
use unlit_wgpu::resources::{ResHandle, TextureView};
use unlit_wgpu::scene::{DrawOrder, Scene};

use crate::pass::PassInfo;
use crate::screen::ScreenBlit;
use crate::source::{FrameSource, RenderContext};

/// A frame source that blits one texture view over its pass's target.
///
/// The view the source samples is the caller's, registered in the frame's
/// [`ResourceGraph`](unlit_wgpu::resources::ResourceGraph), so a world may hand over a different one through the
/// [`view`][BlitSource::view] field. The pipeline is built on the first frame
/// that knows its target, the way
/// [`EguiSource`](crate::egui::EguiSource) specializes its own.
///
/// The blit covers the whole target, so spawn it at
/// [`FrameSourceOrder::MESH`](crate::source::FrameSourceOrder::MESH).
pub struct BlitSource {
    /// The view the source copies, in the frame's resource graph.
    ///
    /// Assigning a different handle makes the next frame rebuild the bind
    /// group; the pipeline and the sampler stay.
    pub view: ResHandle<TextureView>,
    /// How the copy combines with what the pass already holds.
    ///
    /// `None` overwrites it, which is what presenting an earlier pass's picture
    /// wants. A blended copy layers one texture over the frame instead:
    /// `wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING`, for example, leaves the
    /// meshes showing through the parts of the texture that are transparent.
    pub blend: Option<wgpu::BlendState>,
    /// The composite, built on the first frame.
    screen: ScreenBlit,
}

impl BlitSource {
    /// Blit `view` over the pass's target.
    ///
    /// `view` must already be registered in the frame's resource graph — the
    /// one the frame's [`RenderContext`] addresses — like the view it is
    /// blitted into.
    pub fn new(view: ResHandle<TextureView>) -> Self {
        Self {
            view,
            blend: None,
            screen: ScreenBlit::new(DrawOrder::Before),
        }
    }
}

impl FrameSource for BlitSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        self.screen
            .record(scope, ctx, info, scene, self.view.clone(), self.blend);
    }
}
