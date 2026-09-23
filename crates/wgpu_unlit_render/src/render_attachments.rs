//! Attachment management for the single-pass frame.
//!
//! A [`RenderAttachments`] holds the views a frame renders into — the color
//! view, the depth-stencil view and an optional multisample view — all of them
//! caller-owned. The attachment set allocates nothing: textures live wherever
//! the caller keeps them (often a [`crate::resources::ResourceGraph`]), and the
//! set only borrows the views for the duration of a pass.
//!
//! Recording is the caller's: [`RenderAttachments::begin_pass`] opens a pass
//! over the attachments with the caller's load and store ops, so a depth pass
//! can keep its results (a shadow map, a preprocessed depth buffer) instead of
//! discarding them.
//!
//! Sharing resources more broadly — textures, buffers, bind groups across
//! scenes — is the job of [`crate::resources::ResourceGraph`], which the
//! caller keeps separately and registers their own resources in. A
//! [`crate::scene::Scene`] stays the caller's, assembled per frame from the
//! handles they hand out.
//!
//! ```
//! # use wgpu_unlit_render::render_attachments::{
//! #     RenderAttachments, color_clear, depth_clear, stencil_clear,
//! # };
//! # use wgpu_unlit_render::scene::Scene;
//! # fn frame(device: &wgpu::Device, color: wgpu::TextureView, depth: wgpu::TextureView, scene: &Scene<'_>) {
//! let attachments = RenderAttachments::from_views(Some(color), Some(depth), None);
//!
//! let mut encoder = device.create_command_encoder(&Default::default());
//! let mut pass = attachments.begin_pass(
//!     &mut encoder,
//!     color_clear(),
//!     depth_clear(), // the reverse-z far plane
//!     stencil_clear(),
//! );
//! scene.record(&mut pass);
//! # }
//! ```

/// The color load op a frame starts from by default: a clear to black.
#[must_use]
pub const fn color_clear() -> wgpu::LoadOp<wgpu::Color> {
    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
}

/// The depth load op of the renderer's reverse-z convention: a clear to the
/// far plane.
///
/// Pipelines built by [`UnlitOptions::standard`] compare with
/// `CompareFunction::Greater`, so depth starts at the far plane and nearer
/// geometry carries the greater value. Clearing a depth attachment with any
/// other value would put the convention out of joint.
///
/// [`UnlitOptions::standard`]: crate::pipeline::UnlitOptions::standard
#[must_use]
pub const fn depth_clear() -> wgpu::LoadOp<f32> {
    wgpu::LoadOp::Clear(0.0)
}

/// The stencil load op of a frame whose pipelines write no stencil: a clear
/// to zero.
///
/// The built-in pipelines never write stencil, so the pass discards it either
/// way; this only says what a pass that reads stencil beforehand starts from.
#[must_use]
pub const fn stencil_clear() -> wgpu::LoadOp<u32> {
    wgpu::LoadOp::Clear(0)
}

/// The depth-stencil format a render target should use on `device`:
/// `Depth32FloatStencil8` when the device supports it, otherwise the
/// universally-available `Depth24PlusStencil8`.
///
/// Both formats carry a stencil aspect, so the render pass and pipelines must
/// account for it (see [`RenderAttachments::begin_pass`]).
pub fn default_depth_stencil_format(device: &wgpu::Device) -> wgpu::TextureFormat {
    if device
        .features()
        .contains(wgpu::Features::DEPTH32FLOAT_STENCIL8)
    {
        wgpu::TextureFormat::Depth32FloatStencil8
    } else {
        wgpu::TextureFormat::Depth24PlusStencil8
    }
}

/// The attachments a frame renders into.
///
/// Every view is caller-owned; the set allocates nothing. Build one with
/// [`Self::from_views`].
pub struct RenderAttachments {
    /// The color attachment, or `None` for a depth-only pass.
    color_view: Option<wgpu::TextureView>,
    /// The depth attachment, or `None` for a pass without depth.
    depth_stencil_view: Option<wgpu::TextureView>,
    /// The multisample attachment the pass draws into, resolved into
    /// [`Self::color_view`]; `None` when multisampling is disabled or the
    /// pass is depth-only.
    msaa_view: Option<wgpu::TextureView>,
    /// The format the color attachment is viewed as, when it differs from the
    /// format of the texture it views.
    color_format: Option<wgpu::TextureFormat>,
}

impl RenderAttachments {
    /// Assemble an attachment set entirely from the caller's views: the color
    /// view, the depth-stencil view and an optional multisample view.
    ///
    /// The formats, size and sample count are read from the views themselves,
    /// so they must agree across views (the caller's responsibility) — except
    /// for the color format, which only a view's texture reports; use
    /// [`Self::with_color_format`] when the color view was created in another
    /// format. The
    /// depth attachment's transience is read from its texture's usage: a depth
    /// texture created with
    /// [`TextureUsages::TRANSIENT_ATTACHMENT`](wgpu::TextureUsages::TRANSIENT_ATTACHMENT)
    /// is treated as transient (cleared and discarded within a single pass),
    /// any other as persistent (a pass stores into it so a later pass may
    /// sample the results).
    ///
    /// `msaa_view` must be `None` for a non-multisampled pass; when `Some`, its
    /// format and size must match the color view and its sample count must be
    /// greater than one.
    ///
    /// # Panics
    ///
    /// If neither the color nor the depth view is present, or if the
    /// multisample view is present without a color view.
    pub fn from_views(
        color_view: Option<wgpu::TextureView>,
        depth_stencil_view: Option<wgpu::TextureView>,
        msaa_view: Option<wgpu::TextureView>,
    ) -> Self {
        assert!(
            color_view.is_some() || depth_stencil_view.is_some(),
            "a render pass needs at least one attachment"
        );
        assert!(
            msaa_view.is_none() || color_view.is_some(),
            "a multisample view needs a color view to resolve into"
        );
        Self {
            color_view,
            depth_stencil_view,
            msaa_view,
            color_format: None,
        }
    }

    /// State the format the color attachment's view was created with.
    ///
    /// [`Self::from_views`] reads the color format from the attachment's
    /// texture, which is what a default view uses. A view may be created in
    /// another format instead — an sRGB view over a non-sRGB swap-chain image,
    /// the only way to get correct gamma on the web — and a pipeline's color
    /// target must match the *view*, so the caller that created it states it
    /// here.
    #[must_use]
    pub fn with_color_format(mut self, format: wgpu::TextureFormat) -> Self {
        self.color_format = Some(format);
        self
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
    pub fn depth_stencil_view(&self) -> Option<&wgpu::TextureView> {
        self.depth_stencil_view.as_ref()
    }

    /// The multisample attachment, if any.
    pub fn msaa_view(&self) -> Option<&wgpu::TextureView> {
        self.msaa_view.as_ref()
    }

    /// The color format the pass renders into, or `None` for a depth-only
    /// pass.
    ///
    /// [`Self::with_color_format`]'s when one was given; the attachment's
    /// texture format otherwise — the MSAA view's when one is set, the color
    /// view's otherwise.
    pub fn color_format(&self) -> Option<wgpu::TextureFormat> {
        if let Some(format) = self.color_format {
            return Some(format);
        }
        let view = self.msaa_view.as_ref().or(self.color_view.as_ref())?;
        Some(view.texture().format())
    }

    /// The depth format the pass renders into, or `None` for a pass without
    /// depth.
    pub fn depth_stencil_format(&self) -> Option<wgpu::TextureFormat> {
        Some(self.depth_stencil_view.as_ref()?.texture().format())
    }

    /// The sample count the pass renders with: the MSAA attachment's when one
    /// is set, `1` otherwise.
    pub fn sample_count(&self) -> u32 {
        match self.msaa_view.as_ref() {
            Some(view) => view.texture().sample_count(),
            None => 1,
        }
    }

    /// The [SurfaceKey](crate::specialize::SurfaceKey) of the attachments this
    /// set owns: their color format, depth format and sample count.
    ///
    /// The color format and sample count come from
    /// [`Self::color_format`] and [`Self::sample_count`], so they always
    /// describe the same attachment.
    pub fn surface_key(&self) -> crate::specialize::SurfaceKey {
        crate::specialize::SurfaceKey::from_attachments(self)
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
            .or(self.depth_stencil_view.as_ref())
    }

    /// Whether the depth attachment is transient — created with
    /// [`TextureUsages::TRANSIENT_ATTACHMENT`](wgpu::TextureUsages::TRANSIENT_ATTACHMENT)
    /// — and so only accepts `Clear + Discard` within a single pass. `false`
    /// when there is no depth attachment.
    fn depth_is_transient(&self) -> bool {
        self.depth_stencil_view.as_ref().is_some_and(|view| {
            view.texture()
                .usage()
                .contains(wgpu::TextureUsages::TRANSIENT_ATTACHMENT)
        })
    }

    /// Begin a render pass over the attachments.
    ///
    /// Each `*_load` value is the load op of its attachment:
    /// [`wgpu::LoadOp::Clear(value)`](wgpu::LoadOp) clears it to `value`,
    /// [`wgpu::LoadOp::Load`] keeps its previous contents. The color
    /// attachment discards on store with MSAA (the resolve carries the pixels
    /// into the color view) and stores without it (the color view *is* the
    /// attachment). The depth attachment discards after a clear and stores
    /// after a load: a transient depth texture (see
    /// [`Self::depth_is_transient`]) only accepts `Clear + Discard`, while a
    /// persistent depth texture — a shadow map, a preprocessed depth buffer —
    /// keeps the results for a later pass to sample. The stencil aspect
    /// follows the same rules (the renderer never writes it, so `Clear`
    /// clears it and `Load` leaves a persistent buffer's contents alone).
    ///
    /// # Panics
    /// If the attachment set has no attachments, if a clear value is given
    /// without the matching attachment, or if an existing attachment is given
    /// no op.
    pub fn begin_pass<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
        color_load: wgpu::LoadOp<wgpu::Color>,
        depth_load: wgpu::LoadOp<f32>,
        stencil_load: wgpu::LoadOp<u32>,
    ) -> wgpu::RenderPass<'a> {
        assert!(
            self.color_view.is_some() || self.depth_stencil_view.is_some(),
            "a render pass needs at least one attachment"
        );
        if matches!(depth_load, wgpu::LoadOp::Load) {
            assert!(
                !self.depth_is_transient(),
                "the depth texture is transient: load its previous contents \
                 only on a persistent texture"
            );
        }
        if matches!(stencil_load, wgpu::LoadOp::Load)
            && self
                .depth_stencil_view
                .as_ref()
                .is_some_and(|view| view.texture().format().has_stencil_aspect())
        {
            assert!(
                !self.depth_is_transient(),
                "the stencil is transient: load its previous contents only on \
                 a persistent texture"
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
                        load: color_load,
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

        let depth_stencil_attachment = self.depth_stencil_view.as_ref().map(|view| {
            // A stencil-capable format must be given stencil load/store ops.
            // The renderer never writes stencil, so it is discarded either
            // way.
            let stencil_ops =
                view.texture()
                    .format()
                    .has_stencil_aspect()
                    .then_some(wgpu::Operations {
                        load: stencil_load,
                        store: wgpu::StoreOp::Discard,
                    });
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: depth_load,
                    // A transient depth texture only accepts `Clear +
                    // Discard`; a persistent depth texture stores, so a
                    // later pass can sample the results.
                    store: if self.depth_is_transient() {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                }),
                stencil_ops,
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

/// Textures a frame loop allocates for offscreen rendering.
///
/// Created by [`create_render_target`]: a persistent color texture (read back
/// or presented by the caller), plus transient depth and multisample textures
/// that are discarded inside a single pass.
pub struct FrameTextures {
    /// The persistent color texture the frame renders into.
    pub color: wgpu::Texture,
    /// The transient depth texture.
    pub depth: wgpu::Texture,
    /// The transient multisample texture, `None` for a non‑multisampled pass.
    pub msaa: Option<wgpu::Texture>,
    /// The attachment set built from the three textures above.
    pub attachments: RenderAttachments,
}

/// Allocate a persistent color texture, a transient depth texture, and — when
/// `sample_count > 1` — a transient multisample texture, each with a matching
/// view, and assemble them into a [`RenderAttachments`].
///
/// The color texture is created with
/// [`TextureUsages::RENDER_ATTACHMENT`](wgpu::TextureUsages::RENDER_ATTACHMENT)
/// and [`TextureUsages::COPY_SRC`](wgpu::TextureUsages::COPY_SRC) so the
/// caller can read the frame back. The depth and multisample textures use
/// [`TextureUsages::RENDER_ATTACHMENT`] |
/// [`TRANSIENT_ATTACHMENT`](wgpu::TextureUsages::TRANSIENT_ATTACHMENT). The
/// depth format is [`default_depth_stencil_format`]'s choice for `device`.
///
/// The returned [`FrameTextures`] gives the caller ownership of every texture,
/// so they can register views in a [`crate::resources::ResourceGraph`] or
/// otherwise manage lifetimes beyond the attachment set's borrow.
pub fn create_render_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    sample_count: u32,
) -> FrameTextures {
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgpu_unlit_render::color"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth_format = default_depth_stencil_format(device);
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgpu_unlit_render::depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: depth_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    let msaa = (sample_count > 1).then(|| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgpu_unlit_render::msaa"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
            view_formats: &[],
        })
    });
    let msaa_view = msaa
        .as_ref()
        .map(|tex| tex.create_view(&wgpu::TextureViewDescriptor::default()));
    let attachments = RenderAttachments::from_views(Some(color_view), Some(depth_view), msaa_view);
    FrameTextures {
        color,
        depth,
        msaa,
        attachments,
    }
}
