//! Render target configuration and single-pass recording.
//!
//! A [`Renderer`] owns the attachments a frame renders into: the caller's
//! color view, plus the multisample and depth textures the renderer manages.
//! MSAA is on by default, and both the multisample and the depth textures are
//! transient attachments — they live for exactly one pass, so mobile GPUs can
//! keep them in tile memory.
//!
//! ```no_run
//! # use wgpu_unlit_render::renderer::{RenderTarget, Renderer, RendererOptions};
//! # fn draw(device: &wgpu::Device, view: &wgpu::TextureView, scene: &wgpu_unlit_render::scene::Scene<'_>) {
//! let target = RenderTarget::new(view, wgpu::TextureFormat::Rgba8UnormSrgb, 1280, 720);
//! let renderer = Renderer::new(device, target, RendererOptions::default());
//! let mut encoder = device.create_command_encoder(&Default::default());
//! renderer.render(&mut encoder, wgpu::Color::BLACK, scene);
//! # }
//! ```

use crate::scene::Scene;

/// The color attachment a frame renders into.
///
/// The view is borrowed for the lifetime of the target, so the caller keeps
/// ownership of the swapchain (or offscreen) texture.
#[derive(Clone, Copy, Debug)]
pub struct RenderTarget<'a> {
    /// The view that receives the resolved frame.
    pub view: &'a wgpu::TextureView,
    /// Format of the view's texture; every pipeline must match it.
    pub format: wgpu::TextureFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl<'a> RenderTarget<'a> {
    /// Describe a render target.
    pub fn new(
        view: &'a wgpu::TextureView,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            view,
            format,
            width,
            height,
        }
    }
}

/// Attachment configuration for a [`Renderer`].
#[derive(Clone, Copy, Debug)]
pub struct RendererOptions {
    /// Depth attachment format, or `None` for a pass without depth.
    pub depth: Option<wgpu::TextureFormat>,
    /// MSAA sample count. `1` disables multisampling; the default is 4.
    pub sample_count: u32,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            depth: Some(wgpu::TextureFormat::Depth32Float),
            sample_count: 4,
        }
    }
}

/// The far-plane clear value used for the reverse-z depth attachment.
pub const DEPTH_CLEAR: f32 = 0.0;

/// Renders scenes into one color view in a single pass.
///
/// The multisample and depth textures are owned here and recreated whenever
/// the target size or the options change, so a resizing surface does not leak
/// attachments.
pub struct Renderer<'a> {
    target: RenderTarget<'a>,
    options: RendererOptions,
    /// Resolved (single-sample) color target of the MSAA pass; `None` when
    /// multisampling is disabled.
    msaa_view: Option<wgpu::TextureView>,
    depth_view: Option<wgpu::TextureView>,
}

impl<'a> Renderer<'a> {
    /// Create a renderer for `target`, allocating its attachments.
    pub fn new(device: &wgpu::Device, target: RenderTarget<'a>, options: RendererOptions) -> Self {
        let mut renderer = Self {
            target,
            options,
            msaa_view: None,
            depth_view: None,
        };
        renderer.allocate(device);
        renderer
    }

    /// The target this renderer draws into.
    pub fn target(&self) -> RenderTarget<'a> {
        self.target
    }

    /// The options this renderer was created with.
    pub fn options(&self) -> RendererOptions {
        self.options
    }

    /// Point the renderer at a new target, reallocating attachments when the
    /// size or format changed.
    pub fn set_target(&mut self, device: &wgpu::Device, target: RenderTarget<'a>) {
        let needs_reallocation = target.width != self.target.width
            || target.height != self.target.height
            || target.format != self.target.format;
        self.target = target;
        if needs_reallocation {
            self.allocate(device);
        }
    }

    /// Change the attachment options, reallocating attachments when they
    /// changed.
    pub fn set_options(&mut self, device: &wgpu::Device, options: RendererOptions) {
        if options.depth != self.options.depth || options.sample_count != self.options.sample_count
        {
            self.options = options;
            self.allocate(device);
        }
    }

    /// The depth format in use, if any.
    pub fn depth_format(&self) -> Option<wgpu::TextureFormat> {
        self.options.depth
    }

    /// The sample count in use.
    pub fn sample_count(&self) -> u32 {
        self.options.sample_count
    }

    /// Record `scene` into `encoder` as one render pass.
    ///
    /// The color attachment is cleared to `clear`, and the depth attachment
    /// (when enabled) to [`DEPTH_CLEAR`]. MSAA resolves into the target view
    /// as part of the same pass.
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        clear: wgpu::Color,
        scene: &Scene<'_>,
    ) {
        let resolve_target = self.msaa_view.as_ref().map(|_| self.target.view);
        let color_view = self.msaa_view.as_ref().unwrap_or(self.target.view);

        let color_attachments = [Some(wgpu::RenderPassColorAttachment {
            view: color_view,
            depth_slice: None,
            resolve_target,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear),
                // A transient attachment must be discarded on store; with
                // MSAA enabled the resolved pixels reach the target through
                // `resolve_target`, and without it the target view is the
                // attachment itself.
                store: wgpu::StoreOp::Discard,
            },
        })];

        let depth_stencil_attachment = self.depth_view.as_ref().map(|view| {
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(DEPTH_CLEAR),
                    // Transient: the depth buffer never leaves this pass.
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wgpu_unlit_render::pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        scene.record(&mut pass);
    }

    fn allocate(&mut self, device: &wgpu::Device) {
        self.msaa_view = (self.options.sample_count > 1).then(|| {
            transient_view(
                device,
                "wgpu_unlit_render::msaa",
                self.target.format,
                self.target.width,
                self.target.height,
                self.options.sample_count,
            )
        });
        // Every attachment of a pass must share one sample count, so the
        // depth texture follows the color attachment rather than the target.
        self.depth_view = self.depth_format().map(|format| {
            transient_view(
                device,
                "wgpu_unlit_render::depth",
                format,
                self.target.width,
                self.target.height,
                self.options.sample_count,
            )
        });
    }
}

/// Create a transient attachment view: cleared and consumed inside a single
/// pass, so it is never sampled or copied afterwards.
///
/// The usage must be exactly `RENDER_ATTACHMENT | TRANSIENT_ATTACHMENT`;
/// anything more makes the texture non-transient.
fn transient_view(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    sample_count: u32,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A render target an offscreen test can read back from.
    fn target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test::target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    #[test]
    fn defaults_enable_msaa_and_depth() {
        let options = RendererOptions::default();
        assert_eq!(options.sample_count, 4);
        assert_eq!(options.depth, Some(wgpu::TextureFormat::Depth32Float));
    }

    #[test]
    fn an_empty_scene_records_and_submits() {
        let (device, queue) = crate::util::test_device::device();
        let view = target(&device, 64, 64);
        let renderer = Renderer::new(
            &device,
            RenderTarget::new(&view, wgpu::TextureFormat::Rgba8UnormSrgb, 64, 64),
            RendererOptions::default(),
        );
        assert_eq!(renderer.sample_count(), 4);
        assert_eq!(
            renderer.depth_format(),
            Some(wgpu::TextureFormat::Depth32Float)
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
        renderer.render(&mut encoder, wgpu::Color::BLACK, &Scene::new());
        queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
    }

    #[test]
    fn depth_can_be_disabled() {
        let (device, queue) = crate::util::test_device::device();
        let view = target(&device, 32, 32);
        let renderer = Renderer::new(
            &device,
            RenderTarget::new(&view, wgpu::TextureFormat::Rgba8UnormSrgb, 32, 32),
            RendererOptions {
                depth: None,
                sample_count: 1,
            },
        );
        assert_eq!(renderer.depth_format(), None);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
        renderer.render(&mut encoder, wgpu::Color::BLACK, &Scene::new());
        queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
    }

    #[test]
    fn resizing_reallocates_attachments() {
        let (device, queue) = crate::util::test_device::device();
        let first = target(&device, 32, 32);
        let second = target(&device, 64, 64);
        let mut renderer = Renderer::new(
            &device,
            RenderTarget::new(&first, wgpu::TextureFormat::Rgba8UnormSrgb, 32, 32),
            RendererOptions::default(),
        );
        renderer.set_target(
            &device,
            RenderTarget::new(&second, wgpu::TextureFormat::Rgba8UnormSrgb, 64, 64),
        );
        assert_eq!(renderer.target().width, 64);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
        renderer.render(&mut encoder, wgpu::Color::BLACK, &Scene::new());
        queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
    }
}
