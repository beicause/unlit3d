//! Offscreen render targets a frame draws into and a tool reads back.
//!
//! A pass's color attachment is the caller's: a swap chain image the frame
//! loop rebinds every frame, or a persistent texture that outlives the frame
//! — the target a screenshot reads, an intermediate texture a post-process
//! chain renders through, or both at once. This module holds the persistent
//! kind, as [`PersistentTarget`].
//!
//! The primitives here are what a presenter composes; a caller after the
//! ready-made windowed loop wants
//! [`winit::presenter`](crate::winit::presenter) instead.

use unlit_ecs::{Entity, World};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};

use crate::attachments::FrameAttachments;
use crate::pass::FramePass;
use crate::source::RenderContext;

/// The format the built-in presenters state for an offscreen target.
///
/// sRGB so the picture matches what the swap chain shows.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The sample count an offscreen target of the built-in presenters uses.
///
/// The scene's pipelines specialize on the sample count like any other
/// attachment; matching the windowed path's keeps one variant across both.
pub const OFFSCREEN_SAMPLES: u32 = 4;

/// What kind of target to build: the format and the attachments around it.
#[derive(Clone, Copy, Debug)]
pub struct TargetConfig {
    /// The color format the frames land in.
    pub format: wgpu::TextureFormat,
    /// The sample count of the color and depth targets.
    pub samples: u32,
    /// Whether the target carries a depth-stencil attachment.
    pub with_depth: bool,
    /// How an intermediate target presented to a window follows its size.
    /// Purely offscreen rendering states an explicit pixel size instead.
    pub size: IntermediateSize,
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            format: OFFSCREEN_FORMAT,
            samples: OFFSCREEN_SAMPLES,
            with_depth: true,
            size: IntermediateSize::Scale(1.0),
        }
    }
}

/// Where one persistent target's frames land, and what reads them back.
///
/// The texture is the one a screenshot copies out of; the view is the handle
/// a pass draws into and a blit samples. Both are resource-graph nodes that
/// a [`resize`](Self::resize) replaces in place, so a pass or a blit bound
/// to the view follows the new texture without being told — the view node
/// keeps its id, and what reads it is rebuilt through its own dependency.
pub struct PersistentTarget {
    /// The color texture the frames land in, as a graph node.
    ///
    /// Holding the handle keeps the texture alive; a screenshot resolves the
    /// same node and reads it back.
    pub texture: ResHandle<wgpu::Texture>,
    /// The view of that texture, registered in the frame's resource graph.
    pub view: ResHandle<TextureView>,
    /// The depth and multisample attachments the frames draw with.
    attachments: FrameAttachments,
    /// The size the target is built at, so a same-size resize is a no-op.
    size: (u32, u32),
    /// What kind of target this is, so a resize rebuilds the same kind.
    config: TargetConfig,
}

impl PersistentTarget {
    /// Create a color target of `size` pixels, register it in the frame's
    /// resource graph, and bind it as what `pass` draws into.
    ///
    /// This is the offscreen twin of a window surface: the attachments go
    /// into the graph exactly as a surface's do, so the renderer specializes
    /// its pipelines on them the same way — and the color texture is the one
    /// a screenshot copies out of.
    ///
    /// # Errors
    ///
    /// Returns an error when the pass or the frame's resource graph is gone.
    pub fn bind(
        world: &World,
        context: RenderContext,
        pass: Entity,
        device: &wgpu::Device,
        size: (u32, u32),
        config: TargetConfig,
    ) -> Result<Self, unlit_ecs::EntityAccessError> {
        let color = create_color_target(device, config.format, size.0, size.1);
        // The texture is registered as a resource in its own right, and the
        // view depends on it, so whoever holds the view keeps the texture
        // alive — and a screenshot can resolve the texture by handle and read
        // it back.
        let (texture, view) = {
            let mut graph = context.try_graph(&world.root_scope())?;
            let texture = graph.get().insert(color.clone(), None);
            let view = graph.get().insert(
                TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
                None,
            );
            graph.get().add_dependency(&view, &texture);
            (texture, view)
        };
        let attachments = FrameAttachments::new(
            world,
            context,
            config.format,
            size,
            config.samples,
            config.with_depth,
        );
        let mut pass_row = world.root_scope().query_one::<&mut FramePass>(pass)?;
        pass_row.get().set_attachments(&attachments, view.clone());
        Ok(Self {
            texture,
            view,
            attachments,
            size,
            config,
        })
    }

    /// Rebind the target against a new window `size`.
    ///
    /// The target's [`IntermediateSize`] policy resolves the actual texture
    /// size. The texture and view are replaced in place, so their resource
    /// nodes keep their ids: the pass keeps drawing into the view, and a blit
    /// reading it rebuilds its bind group through its own dependency. A
    /// same-size resize is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame's resource graph is gone.
    pub fn resize(
        &mut self,
        world: &World,
        context: RenderContext,
        device: &wgpu::Device,
        size: (u32, u32),
    ) -> Result<(), unlit_ecs::EntityAccessError> {
        let size = self.config.size.resolve(size);
        if size == self.size {
            return Ok(());
        }
        self.size = size;
        let color = create_color_target(device, self.config.format, size.0, size.1);
        {
            let mut graph = context.try_graph(&world.root_scope())?;
            graph.get().replace(&self.texture, color.clone());
            graph.get().replace(
                &self.view,
                TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            );
        }
        // Replaces the depth and multisample views in place and maintains the
        // graph, which rebuilds what reads the replaced ones.
        self.attachments.resize(world, size);
        Ok(())
    }
}

/// How big an intermediate target is.
///
/// The window's size is in physical pixels, the unit every target is built
/// with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IntermediateSize {
    /// A multiple of the window's size, rounded down to at least one pixel.
    ///
    /// `1.0` follows the window exactly, so a screenshot is the picture on the
    /// glass; `0.5` renders at half the width and height.
    Scale(f32),
    /// A fixed pixel size that ignores the window.
    Fixed(u32, u32),
}

impl IntermediateSize {
    /// Resolve the size against the window's `window_size`.
    pub fn resolve(&self, window_size: (u32, u32)) -> (u32, u32) {
        match *self {
            Self::Scale(scale) => {
                let width = (window_size.0 as f32 * scale) as u32;
                let height = (window_size.1 as f32 * scale) as u32;
                (width.max(1), height.max(1))
            }
            Self::Fixed(width, height) => (width, height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IntermediateSize;

    #[test]
    fn intermediate_sizes_resolve_against_the_window() {
        let window = (1920, 1080);
        assert_eq!(IntermediateSize::Scale(1.0).resolve(window), window);
        assert_eq!(IntermediateSize::Scale(0.5).resolve(window), (960, 540));
        assert_eq!(
            IntermediateSize::Fixed(640, 360).resolve(window),
            (640, 360)
        );
        assert_eq!(IntermediateSize::Scale(0.0).resolve(window), (1, 1));
    }
}
