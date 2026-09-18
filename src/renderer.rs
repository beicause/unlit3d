//! Single-pass recording.
//!
//! A [`Renderer`] is a plain collection of the attachments a pass renders
//! into. It owns no allocation: the caller creates every attachment — the
//! color view, the depth view, and the multisample view when one is wanted —
//! and hands the handles over. [`RenderContext`](crate::render_context::RenderContext)
//! is the counterpart that allocates those attachments and rebuilds
//! pipelines when they change; a caller managing attachments by hand uses
//! `Renderer` directly.
//!
//! ```no_run
//! # use wgpu_unlit_render::renderer::Renderer;
//! # use wgpu_unlit_render::scene::Scene;
//! # fn draw(device: &wgpu::Device, color_view: wgpu::TextureView, depth_view: wgpu::TextureView, scene: &Scene<'_>) {
//! let renderer = Renderer {
//!     color_view: Some(color_view),
//!     depth_view: Some(depth_view),
//!     msaa_view: None,
//! };
//! let mut encoder = device.create_command_encoder(&Default::default());
//! renderer.render(&mut encoder, wgpu::Color::BLACK, scene);
//! # }
//! ```

use crate::scene::Scene;

/// The far-plane clear value used for the reverse-z depth attachment.
pub const DEPTH_CLEAR: f32 = 0.0;

/// The attachments a pass renders into.
///
/// All fields are public: build the struct directly, swapping any view
/// between frames is simply an assignment. The color view is optional — a
/// depth preprocessing pass renders into the depth attachment alone — and at
/// least one of the color and depth views must be present, or the pass has
/// nothing to render into.
///
/// The views are owned by clone — wgpu handles are reference-counted — so
/// the caller keeps their own handles and the renderer stays valid for as
/// long as it is needed. Every attachment must agree on format-independent
/// state: the size and (when [`Self::msaa_view`] is set) the sample count of
/// the textures behind them.
#[derive(Clone, Debug)]
pub struct Renderer {
    /// The color attachment, or `None` for a depth-only pass.
    pub color_view: Option<wgpu::TextureView>,
    /// The depth attachment, or `None` for a pass without depth.
    pub depth_view: Option<wgpu::TextureView>,
    /// The multisample attachment the pass draws into, resolved into
    /// [`Self::color_view`]; `None` when multisampling is disabled or the
    /// pass is depth-only.
    pub msaa_view: Option<wgpu::TextureView>,
}

impl Renderer {
    /// The color format pipelines must be built for, or `None` for a
    /// depth-only pass.
    ///
    /// Taken from the attachment itself: the MSAA view when one is set, the
    /// color view otherwise.
    pub fn color_format(&self) -> Option<wgpu::TextureFormat> {
        let view = self.msaa_view.as_ref().or(self.color_view.as_ref())?;
        Some(view.texture().format())
    }

    /// The depth format pipelines must be built for, or `None` for a pass
    /// without depth.
    pub fn depth_format(&self) -> Option<wgpu::TextureFormat> {
        Some(self.depth_view.as_ref()?.texture().format())
    }

    /// The sample count pipelines must be built for: the MSAA attachment's
    /// when one is set, `1` otherwise.
    pub fn sample_count(&self) -> u32 {
        match self.msaa_view.as_ref() {
            Some(view) => view.texture().sample_count(),
            None => 1,
        }
    }

    /// The width of the attachments, in pixels.
    ///
    /// Taken from whichever attachment is present; they must agree.
    pub fn width(&self) -> u32 {
        self.attachment_view()
            .map(|view| view.texture().width())
            .unwrap_or(1)
    }

    /// The height of the attachments, in pixels.
    ///
    /// Taken from whichever attachment is present; they must agree.
    pub fn height(&self) -> u32 {
        self.attachment_view()
            .map(|view| view.texture().height())
            .unwrap_or(1)
    }

    /// The view the attachment dimensions are read from: the MSAA view when
    /// one is set, the color view otherwise, the depth view last.
    fn attachment_view(&self) -> Option<&wgpu::TextureView> {
        self.msaa_view
            .as_ref()
            .or(self.color_view.as_ref())
            .or(self.depth_view.as_ref())
    }

    /// Record `scene` into `encoder` as one render pass.
    ///
    /// The color attachment, when present, is cleared to `clear`; the depth
    /// attachment (when present) to [`DEPTH_CLEAR`]. When a
    /// [`Self::msaa_view`] is set, the pass draws into it and resolves into
    /// [`Self::color_view`] as part of the same pass.
    ///
    /// # Panics
    /// If neither a color nor a depth view is set: a pass needs at least one
    /// attachment.
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        clear: wgpu::Color,
        scene: &Scene<'_>,
    ) {
        assert!(
            self.color_view.is_some() || self.depth_view.is_some(),
            "a render pass needs at least one attachment: set `color_view` \
             or `depth_view`"
        );

        // A depth-only pass has no color attachment to name; with MSAA, the
        // pass draws into the multisample view and resolves into the color
        // view.
        let resolve_target = self
            .msaa_view
            .as_ref()
            .zip(self.color_view.as_ref())
            .map(|(_, view)| view.clone());
        let attachment_view = self
            .msaa_view
            .as_ref()
            .or(self.color_view.as_ref())
            .cloned();

        let color_attachments = attachment_view
            .as_ref()
            .map(|view| {
                [Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve_target.as_ref(),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        // A transient attachment must be discarded on store;
                        // with MSAA enabled the resolved pixels reach the
                        // color view through `resolve_target`, and without
                        // it the color view is the attachment itself.
                        store: wgpu::StoreOp::Discard,
                    },
                })]
            })
            .unwrap_or([None]);

        let depth_stencil_attachment = self.depth_view.as_ref().map(|view| {
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(DEPTH_CLEAR),
                    // Transient convention: the depth buffer never leaves
                    // this pass. A caller reusing a depth attachment across
                    // passes records their own store op instead.
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
}
