//! Attachment management for the single-pass frame.
//!
//! A [`RenderAttachments`] is the long-lived hub of a frame loop. It holds
//! the attachments — the color view the caller hands over, plus the
//! multisample and depth textures the attachment set allocates as transient
//! attachments, so mobile GPUs can keep them in tile memory.
//!
//! Recording is the caller's: [`RenderAttachments::begin_pass`] opens a pass
//! over the attachments with the caller's load and store ops, so a depth pass
//! can keep its results (a shadow map, a preprocessed depth buffer) instead
//! of discarding them.
//!
//! Sharing resources more broadly — textures, buffers, bind groups across
//! scenes — is the job of [`crate::resources::ResourceGraph`], which the
//! caller keeps separately and registers their own resources in. A
//! [`crate::scene::Scene`] stays the caller's, assembled per frame from the
//! handles they hand out.
//!
//! ```no_run
//! # use wgpu_unlit_render::render_attachments::{RenderAttachments, AttachmentsInfo};
//! # use wgpu_unlit_render::scene::Scene;
//! # fn frame(device: &wgpu::Device, scene: &Scene<'_>) {
//! let attachments = RenderAttachments::new(device, AttachmentsInfo::new(1280, 720));
//!
//! let mut encoder = device.create_command_encoder(&Default::default());
//! let mut pass = attachments.begin_pass(
//!     &mut encoder,
//!     Some(wgpu::Color::BLACK),
//!     Some(attachments.depth_clear()),
//! );
//! scene.record(&mut pass);
//! # }
//! ```

/// Configuration for a [`RenderAttachments`]: the attachments' formats, the
/// target size and the sample count.
///
/// Changing any field recreates the transient attachments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentsInfo {
    /// Color attachment format, matching the color view when that is `Some`;
    /// `None` exactly when the pass is depth-only.
    pub color: Option<wgpu::TextureFormat>,
    /// Depth attachment format, or `None` for a pass without depth.
    ///
    /// At least one of [`Self::color`] and this must be `Some`: a pass with
    /// neither attachment cannot exist.
    pub depth: Option<wgpu::TextureFormat>,
    /// Width of the attachments, in pixels.
    pub width: u32,
    /// Height of the attachments, in pixels.
    pub height: u32,
    /// MSAA sample count. `1` disables multisampling.
    pub sample_count: u32,
    /// Whether the depth attachment is the attachment set's own transient
    /// texture (`true`) or a caller-supplied persistent one (`false`). A
    /// transient depth attachment is cleared and discarded inside a single
    /// pass; a persistent one — a shadow map, a preprocessed depth buffer —
    /// stores its results for a later pass to sample.
    pub transient_depth: bool,
}

impl AttachmentsInfo {
    /// The color, depth, size and sample count for an `Rgba8UnormSrgb` +
    /// `Depth32Float` target of `width` x `height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            color: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
            depth: Some(wgpu::TextureFormat::Depth32Float),
            width,
            height,
            sample_count: 4,
            transient_depth: true,
        }
    }
}

/// The attachments a frame renders into.
pub struct RenderAttachments {
    /// The configuration the attachments were built from.
    options: AttachmentsInfo,
    /// The color attachment, or `None` for a depth-only pass.
    color_view: Option<wgpu::TextureView>,
    /// The depth attachment, or `None` for a pass without depth.
    depth_view: Option<wgpu::TextureView>,
    /// The multisample attachment the pass draws into, resolved into
    /// [`Self::color_view`]; `None` when multisampling is disabled or the
    /// pass is depth-only.
    msaa_view: Option<wgpu::TextureView>,
}

impl RenderAttachments {
    /// Create an attachment set for `options`, allocating every attachment it
    /// asks for: a persistent color texture (see [`Self::color_texture`]), and
    /// the transient depth and multisample attachments.
    ///
    /// A depth-only pass — depth preprocessing, shadow maps — is
    /// `AttachmentsInfo::color = None`.
    ///
    /// A color target the caller owns — the swapchain's texture, say — goes
    /// through [`Self::new_with_targets`].
    ///
    /// # Panics
    /// If both `options.color` and `options.depth` are `None`: a pass needs
    /// at least one attachment.
    pub fn new(device: &wgpu::Device, mut options: AttachmentsInfo) -> Self {
        // This constructor allocates the transient depth texture itself, so
        // the depth attachment is transient by definition.
        options.transient_depth = true;
        let color = options.color.map(|format| {
            // The color target is persistent by construction: a caller copies
            // or reads back the frame from it, and `begin_pass` stores into
            // it through its own view.
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wgpu_unlit_render::color"),
                size: wgpu::Extent3d {
                    width: options.width.max(1),
                    height: options.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let color_view = color
            .as_ref()
            .map(|texture| texture.create_view(&Default::default()));
        // The depth texture is this constructor's own allocation, so it is
        // transient; `new_with_targets` passes its caller's choice.
        let depth_view = options.depth.map(|format| {
            transient_texture(device, &options, "wgpu_unlit_render::depth", format)
                .create_view(&Default::default())
        });
        Self::from_parts(device, options, color_view, depth_view)
    }

    /// Create an attachment set drawing into `color_view` (and `depth_view`),
    /// the caller's own targets — the swapchain's texture, a shadow map, a
    /// preprocessed depth buffer. The transient attachments `options` asks for
    /// are still allocated here.
    ///
    /// The color view's format must be `options.color`'s, the depth view's
    /// `options.depth`'s, and the sizes must match. The depth view is
    /// persistent from the caller's side, so a depth pass stores its results
    /// into it.
    ///
    /// # Panics
    /// If the views do not match the options: a color view needs a color
    /// format and a matching size, a depth view needs a depth format and a
    /// matching size, and at least one of the two must be present.
    pub fn new_with_targets(
        device: &wgpu::Device,
        mut options: AttachmentsInfo,
        color_view: Option<wgpu::TextureView>,
        depth_view: Option<wgpu::TextureView>,
    ) -> Self {
        assert_eq!(
            color_view.is_some(),
            options.color.is_some(),
            "a color view and its format must be present together"
        );
        assert_eq!(
            depth_view.is_some(),
            options.depth.is_some(),
            "a depth view and its format must be present together"
        );
        assert!(
            color_view.is_some() || depth_view.is_some(),
            "a render pass needs at least one attachment"
        );
        if let Some(view) = color_view.as_ref() {
            assert_eq!(
                view.texture().format(),
                options.color.expect("checked above"),
                "the color view's format must match `options.color`"
            );
            assert_eq!(
                view.texture().width(),
                options.width.max(1),
                "the color view's width must match `options.width`"
            );
            assert_eq!(
                view.texture().height(),
                options.height.max(1),
                "the color view's height must match `options.height`"
            );
        }
        if let Some(view) = depth_view.as_ref() {
            assert_eq!(
                view.texture().format(),
                options.depth.expect("checked above"),
                "the depth view's format must match `options.depth`"
            );
            assert_eq!(
                view.texture().width(),
                options.width.max(1),
                "the depth view's width must match `options.width`"
            );
            assert_eq!(
                view.texture().height(),
                options.height.max(1),
                "the depth view's height must match `options.height`"
            );
        }
        // The depth view the caller supplies is their persistent texture: a
        // depth pass stores into it.
        options.transient_depth = false;
        Self::from_parts(device, options, color_view, depth_view)
    }

    /// Assemble an attachment set from the caller's views: the color view
    /// (the caller's own, or allocated by [`Self::new`]), the depth view (the
    /// caller's own, or allocated as transient by [`Self::new`]), and the
    /// transient multisample attachment.
    fn from_parts(
        device: &wgpu::Device,
        options: AttachmentsInfo,
        color_view: Option<wgpu::TextureView>,
        depth_view: Option<wgpu::TextureView>,
    ) -> Self {
        let msaa_view = options.color.and_then(|format| {
            (options.sample_count > 1).then(|| {
                transient_texture(device, &options, "wgpu_unlit_render::msaa", format)
                    .create_view(&Default::default())
            })
        });
        Self {
            options,
            color_view,
            depth_view,
            msaa_view,
        }
    }

    /// Replace the depth attachment with a caller-supplied view of the same
    /// format — the persistent texture a depth pass stores into, such as a
    /// shadow map or a preprocessed depth buffer.
    pub fn set_depth_view(&mut self, depth_view: wgpu::TextureView) {
        self.depth_view = Some(depth_view);
        self.options.transient_depth = false;
    }

    /// The color texture the frame is rendered into, for copying or reading
    /// the result back. `None` for a depth-only pass.
    pub fn color_texture(&self) -> Option<&wgpu::Texture> {
        self.color_view.as_ref().map(|view| view.texture())
    }

    /// The color attachment, or `None` for a depth-only pass.
    pub fn color_view(&self) -> Option<&wgpu::TextureView> {
        self.color_view.as_ref()
    }

    /// The depth attachment, if any.
    pub fn depth_view(&self) -> Option<&wgpu::TextureView> {
        self.depth_view.as_ref()
    }

    /// The multisample attachment, if any.
    pub fn msaa_view(&self) -> Option<&wgpu::TextureView> {
        self.msaa_view.as_ref()
    }

    /// The color format the pass renders into, or `None` for a depth-only
    /// pass.
    ///
    /// Taken from the attachment itself: the MSAA view when one is set, the
    /// color view otherwise.
    pub fn color_format(&self) -> Option<wgpu::TextureFormat> {
        let view = self.msaa_view.as_ref().or(self.color_view.as_ref())?;
        Some(view.texture().format())
    }

    /// The depth format the pass renders into, or `None` for a pass without
    /// depth.
    pub fn depth_format(&self) -> Option<wgpu::TextureFormat> {
        Some(self.depth_view.as_ref()?.texture().format())
    }

    /// The sample count the pass renders with: the MSAA attachment's when one
    /// is set, `1` otherwise.
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

    /// The depth value this renderer's reverse-z convention clears to: the
    /// far plane.
    pub fn depth_clear(&self) -> f32 {
        0.0
    }

    /// Begin a render pass over the attachments.
    ///
    /// `color_clear`/`depth_clear` of `Some(value)` clear the matching
    /// attachment to `value`; `None` loads its previous contents. The color
    /// attachment discards on store with MSAA (the resolve carries the pixels
    /// into the color view) and stores without it (the color view *is* the
    /// attachment). The depth attachment discards after a clear and stores
    /// after a load: the attachment set's own depth texture is transient and
    /// only accepts `Clear + Discard`, while a caller's persistent depth
    /// texture — a shadow map, a preprocessed depth buffer — keeps the
    /// results for a later pass to sample.
    ///
    /// # Panics
    /// If the attachment set has no attachments, if a clear value is given
    /// without the matching attachment, or if an existing attachment is given
    /// no op.
    pub fn begin_pass<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
        color_clear: Option<wgpu::Color>,
        depth_clear: Option<f32>,
    ) -> wgpu::RenderPass<'a> {
        assert!(
            self.color_view.is_some() || self.depth_view.is_some(),
            "a render pass needs at least one attachment"
        );
        if depth_clear.is_none() {
            assert!(
                !self.options.transient_depth,
                "the attachment set's depth texture is transient: load its \
                 previous contents with `set_depth_view` on a persistent \
                 texture"
            );
        }

        // With MSAA, the pass draws into the multisample view and resolves
        // into the color view.
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
                        // `Some` clears, `None` loads the previous contents.
                        load: color_clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                        // With MSAA the resolved pixels reach the color
                        // view through the resolve, so the multisample
                        // attachment itself can be discarded. Without MSAA
                        // the color view *is* the attachment: the frame is
                        // only there if it is stored.
                        store: if self.msaa_view.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })]
            })
            .unwrap_or([None]);

        let depth_stencil_attachment = self.depth_view.as_ref().map(|view| {
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    // `Some` clears, `None` loads the previous contents.
                    load: depth_clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    // The attachment set's own depth texture is transient and
                    // only accepts `Clear + Discard`; a caller's persistent
                    // depth texture stores, so a later pass can sample the
                    // results.
                    store: if self.options.transient_depth {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                }),
                stencil_ops: None,
            }
        });

        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wgpu_unlit_render::pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }
}

/// Create a transient attachment texture: cleared and consumed inside a
/// single pass, so it is never sampled or copied afterwards.
///
/// The usage must be exactly `RENDER_ATTACHMENT | TRANSIENT_ATTACHMENT`;
/// anything more makes the texture non-transient.
fn transient_texture(
    device: &wgpu::Device,
    options: &AttachmentsInfo,
    label: &str,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: options.width.max(1),
            height: options.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: options.sample_count,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
        view_formats: &[],
    })
}
