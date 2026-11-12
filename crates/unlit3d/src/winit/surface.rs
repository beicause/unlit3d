//! Presenting a [`Renderer`] into a [winit](https://docs.rs/winit) window.
//!
//! A window presents through a swap chain, which the frame's own attachment
//! helpers do not cover: every frame hands out a new color image, and the
//! attachments the frame draws with have to match the window's size.
//! [`WindowSurface`] owns the surface and its configuration, keeps the frame's
//! color image registered in the frame's resource graph, and hands it back from
//! [`WindowSurface::acquire_for`], which parks the pass that states the frame's
//! attachments around the acquire. The depth and multisample attachments belong
//! to the caller — see [`FrameAttachments`]
//! — and a stated attachment spans only the render that consumes it: the frame
//! loop below states and gives up the attachments through the two methods, and
//! presenting the frame destroys the swap-chain view the pass stated.
//!
//! The caller creates the `wgpu::Surface` itself, because the adapter has to
//! be requested with it as the compatible surface. That request is async —
//! on the web it resolves on the browser's task queue, so a frame loop must not
//! block on it — and so is the device's, which leaves both to the caller:
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use unlit3d::attachments::FrameAttachments;
//! use unlit3d::prelude::*;
//! use unlit3d::winit::WindowSurface;
//! use winit::window::Window;
//!
//! # fn setup(
//! #     world: &World,
//! #     renderer: Entity,
//! #     instance: &wgpu::Instance,
//! #     adapter: &wgpu::Adapter,
//! #     window: Arc<Window>,
//! #     surface: wgpu::Surface<'static>,
//! # ) -> (WindowSurface, FrameAttachments) {
//! // The renderer the surface draws through, spawned once as a resource.
//! let window_surface = world
//!     .root_scope()
//!     .query_one::<&mut Renderer>(renderer)
//!     .map(|mut r| {
//!         WindowSurface::new(world, r.get(), instance, adapter, window, surface)
//!     })
//!     .unwrap();
//! // The depth and multisample attachments the frame draws with; `true` asks
//! // for a depth-stencil one, which a frame of depth-tested geometry needs.
//! let context = world
//!     .root_scope()
//!     .query_one::<&Renderer>(renderer)
//!     .unwrap()
//!     .get()
//!     .context();
//! let attachments = FrameAttachments::new(
//!     world,
//!     context,
//!     window_surface.color_format(),
//!     window_surface.size(),
//!     4,
//!     true,
//! );
//! (window_surface, attachments)
//! # }
//! ```
//!
//! and the frame loop then only has to acquire and present through the pass:
//! [`WindowSurface::acquire_for`] parks the pass and attaches the acquired
//! image for its render, and [`Frame::present_and_park`] presents the frame
//! and parks the pass again:
//!
//! ```rust
//! # use unlit3d::attachments::FrameAttachments;
//! # use unlit3d::prelude::*;
//! # use unlit3d::winit::WindowSurface;
//! # fn frame(
//! #     world: &World,
//! #     queue: &wgpu::Queue,
//! #     renderer_entity: Entity,
//! #     pass: Entity,
//! #     window_surface: &mut WindowSurface,
//! #     attachments: &FrameAttachments,
//! # ) {
//! let root = world.root_scope();
//! let Some(frame) = window_surface.acquire_for(
//!     world,
//!     renderer_entity,
//!     pass,
//!     Some(attachments),
//! ) else {
//!     // The pass stays parked: nothing records into a swap-chain image
//!     // that is gone or was never acquired.
//!     return;
//! };
//! // The region the content is drawn into belongs to the caller.
//! root.query_one::<&mut Renderer>(renderer_entity)
//!     .map(|mut renderer| renderer.get().render(world))
//!     .unwrap();
//! frame.present_and_park(world, queue, pass);
//! # }
//! ```

use std::sync::Arc;

use unlit_ecs::{Entity, World};
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};

use crate::attachments::FrameAttachments;
use crate::pass::FramePass;
use crate::renderer::Renderer;

/// How a window's swap chain presents its frames.
///
/// The present mode decides how a presented frame waits for the display:
/// [`wgpu::PresentMode::Fifo`] ties every frame to a refresh and is what a
/// plain window wants, while `Immediate` and `Mailbox` cut the wait at the
/// cost of tearing or of a frame of latency. A surface does not offer every
/// mode — WebGL2 and many mobile drivers offer only `Fifo` — so a requested
/// mode the surface lacks degrades to `Fifo`, and a `Fifo` a surface lacks
/// falls back to what it does offer; either way the surface logs which mode
/// it settled on, in a warning.
///
/// `AutoVsync` and `AutoNoVsync` are passed through to wgpu, which resolves
/// them against the same capabilities by its own rules.
///
/// The frame latency is the number of refreshes between acquiring a frame and
/// presenting it — frames in flight. `1` minimizes latency and gives the CPU
/// and the GPU no room to overlap; `2`, the default, lets each use a whole
/// refresh; higher values favor throughput. The backend clamps it to the range
/// it supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceOptions {
    /// The present mode to ask for.
    pub present_mode: wgpu::PresentMode,
    /// How many refreshes may pass between acquiring a frame and presenting
    /// it.
    pub desired_maximum_frame_latency: u32,
}

impl Default for SurfaceOptions {
    /// A surface tied to the display: `Fifo` with two frames in flight.
    fn default() -> Self {
        Self {
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
        }
    }
}

/// A winit window's swap chain, presented through a [`Renderer`].
///
/// The surface is configured for the window's current size and keeps the
/// frame's color image registered in the frame's resource graph.
/// [`Self::acquire_for`] hands a frame to a pass — parking the pass around
/// the acquire and stating the frame's color view with the pass's own depth
/// and multisample attachments, see
/// [`FrameAttachments`] — and
/// [`Frame::present_and_park`] presents the frame and parks the pass again.
/// [`Self::resize`] keeps the surface in step with the window.
///
/// A platform can invalidate the render surface while the app is not in the
/// foreground, and Android does for as long as it is suspended. [`Self::release`]
/// is for that: it drops the swap chain and its color view without touching the
/// device, the pipelines or anything else the frame draws from, so a scene keeps
/// its state and presents again through a surface built from the same window.
pub struct WindowSurface {
    instance: wgpu::Instance,
    window: Arc<::winit::window::Window>,
    /// The swap chain; `None` after [`Self::release`], until a host builds a
    /// surface for the same window again.
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    /// The format the frame's color attachment is viewed as.
    ///
    /// Usually the swap chain's own format; an sRGB view of it when the
    /// surface offers no sRGB format of its own.
    color_format: wgpu::TextureFormat,
    /// The color view of the most recently acquired frame, in the frame's
    /// graph. One id is kept for the surface's whole life; `None` until the
    /// first frame is acquired.
    color_view: Option<ResHandle<TextureView>>,
}

impl WindowSurface {
    /// Configure `surface` for `window`.
    ///
    /// `surface` must have been created from `window`, and `adapter` must be
    /// able to present to it — the adapter the renderer's device was requested
    /// from.
    ///
    /// The surface format is the first sRGB format the surface supports. A
    /// surface that supports none — the web's canvas does not — is configured
    /// with a non-sRGB format instead and its frames are viewed as sRGB, which
    /// encodes them on write in exactly the same way. An sRGB attachment is
    /// what lets the built-in lit shader's colors reach the display
    /// unchanged.
    ///
    /// # Panics
    ///
    /// If `adapter` cannot present to `surface`.
    pub fn new(
        world: &World,
        renderer: &mut Renderer,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        window: Arc<::winit::window::Window>,
        surface: wgpu::Surface<'static>,
        options: SurfaceOptions,
    ) -> Self {
        let size = window.inner_size();
        let config = surface_config(&surface, adapter, size.width, size.height, options);
        let color_format = color_format(&config);
        let device = renderer
            .context()
            .device(&world.root_scope())
            .expect("the context's device resource exists");
        surface.configure(&device, &config);

        Self {
            instance: instance.clone(),
            window,
            surface: Some(surface),
            config,
            color_format,
            color_view: None,
        }
    }

    /// The size the surface is configured for, in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// The format the swap chain presents in.
    ///
    /// A surface that offers no sRGB format of its own still presents as one:
    /// the frames are viewed as [`Self::color_format`], which differs from
    /// this.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// The format the frame's color attachment is viewed as — the format the
    /// render target's pipelines are specialized for.
    pub fn color_format(&self) -> wgpu::TextureFormat {
        self.color_format
    }

    /// Reconfigure the surface for a new size.
    ///
    /// A zero width or height — a minimized window — is ignored: a surface
    /// cannot be configured with a zero dimension, and there is nothing to
    /// draw.
    pub fn resize(&mut self, world: &World, renderer: &mut Renderer, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        let Some(surface) = self.surface.as_ref() else {
            // A released surface has no swap chain to configure; the size is
            // kept for the one a host builds from the window later.
            return;
        };
        let device = renderer
            .context()
            .device(&world.root_scope())
            .expect("the context's device resource exists");
        surface.configure(&device, &self.config);
    }

    /// Release the swap chain.
    ///
    /// Drops the surface and the color view it registered in the frame's
    /// graph, so nothing is left naming a swap chain that is gone. The device,
    /// the pipelines and everything else the scene draws from are untouched,
    /// and neither is the window: [`Self::build`] opens a surface for the same
    /// window again, at the size this one last knew.
    ///
    /// This is what a suspension needs. Android invalidates the native surface
    /// for as long as the app is not in the foreground, which outlives the swap
    /// chain but not the window, the scene or the GPU context.
    pub fn release(&mut self, world: &World, renderer: &mut Renderer) {
        // Giving up the ids is the whole removal. Dropping them — the surface
        // with the view whose images it holds — leaves the graph's pass below
        // to collect the view, before anything can be left naming a swap chain
        // that is gone.
        let surface = self.surface.take();
        drop(surface);
        self.color_view = None;
        renderer
            .context()
            .graph(&world.root_scope())
            .get()
            .maintain();
    }

    /// Whether [`Self::release`] dropped the swap chain and nothing has opened one
    /// since.
    #[must_use]
    pub fn is_released(&self) -> bool {
        self.surface.is_none()
    }

    /// Open a surface for the window and configure it at the size the released
    /// one had.
    ///
    /// The counterpart of [`Self::release`]: a host that suspended keeps its
    /// window and its scene, and this gives it a swap chain to present through
    /// again. A surface that is already open is left alone.
    ///
    /// # Errors
    ///
    /// Returns the error of the platform surface the window could not create.
    pub fn build(
        &mut self,
        world: &World,
        renderer: &Renderer,
    ) -> Result<(), wgpu::CreateSurfaceError> {
        if self.surface.is_some() {
            return Ok(());
        }
        let surface = self.instance.create_surface(self.window.clone())?;
        let device = renderer
            .context()
            .device(&world.root_scope())
            .expect("the context's device resource exists");
        surface.configure(&device, &self.config);
        self.surface = Some(surface);
        Ok(())
    }

    /// Acquire the next frame.
    ///
    /// Returns `None` when the frame should be skipped — the window is
    /// occluded or minimized, or the swap chain had to be reconfigured. The
    /// caller must not render in that case.
    ///
    /// The frame's image replaces the previous one in the resource graph, so
    /// at most the frame just presented is still referenced — never a longer
    /// history of swap-chain images. The caller binds the returned frame's
    /// color view as its render target, together with whatever depth and
    /// multisample attachments it draws with.
    ///
    /// A frame loop usually wants [`Self::acquire_for`], which parks the
    /// stating pass around this call and states the frame's attachments on it.
    pub fn acquire(&mut self, world: &World, renderer: &mut Renderer) -> Option<Frame> {
        let device = renderer
            .context()
            .device(&world.root_scope())
            .expect("the context's device resource exists");
        let Some(surface) = self.surface.as_ref() else {
            // A released surface has nothing to acquire from: the caller
            // presents nothing until a host builds one again.
            return None;
        };
        let surface_texture = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated => {
                surface.configure(&device, &self.config);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.recreate_surface(&device);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return None,
        };

        // The color view keeps one id for the surface's whole life: replacing
        // the resource swaps in the new frame's view and drops the previous
        // one, and a skipped frame leaves the id untouched.
        let view = color_view_resource(&surface_texture.texture, self.color_format);
        let color = match self.color_view.as_ref() {
            Some(id) => {
                renderer
                    .context()
                    .graph(&world.root_scope())
                    .get()
                    .replace(id, view)
                    .expect("the color view is in the graph");
                id.clone()
            }
            None => renderer
                .context()
                .graph(&world.root_scope())
                .get()
                .insert(view, None),
        };
        self.color_view = Some(color.clone());
        // Replacing the color view marked every reader dirty; drop the old
        // frame's view now rather than holding it until the next scene build.
        renderer
            .context()
            .graph(&world.root_scope())
            .get()
            .maintain();
        Some(Frame {
            surface_texture,
            color_view: color,
        })
    }

    /// Acquire the next frame for a pass and state the frame's attachments on
    /// it.
    ///
    /// This is a frame loop's whole acquire step: the pass is parked, the
    /// next frame is acquired through its renderer, and the pass takes the
    /// frame's color view along with `attachments` when they are given.
    /// Without attachments the pass stays parked and the frame comes back
    /// with nothing stated on it — a loop that lost its attachments presents
    /// it empty rather than stale. A `None` — the window is occluded or
    /// minimized, or the swap chain had to be reconfigured — leaves the pass
    /// parked, so a frame rendered while the loop is between presents skips
    /// the pass rather than recording into a swap-chain image that is gone.
    ///
    /// Pair the frame with [`Frame::present_and_park`], which presents it
    /// and parks the pass again.
    ///
    /// # Panics
    ///
    /// If `renderer` does not name a [`Renderer`], or — with
    /// `attachments` — if `pass` does not name a [`FramePass`].
    pub fn acquire_for(
        &mut self,
        world: &World,
        renderer: Entity,
        pass: Entity,
        attachments: Option<&FrameAttachments>,
    ) -> Option<Frame> {
        let _ = world
            .root_scope()
            .query_one::<&mut FramePass>(pass)
            .map(|mut pass| pass.get().park());
        let mut renderer_row = world
            .root_scope()
            .query_one::<&mut Renderer>(renderer)
            .expect("the renderer exists");
        let Some(frame) = self.acquire(world, &mut *renderer_row.get()) else {
            // The acquire failed, so the pass stays parked: nothing records
            // into a swap-chain image that is gone or was never acquired.
            return None;
        };
        if let Some(attachments) = attachments {
            world
                .root_scope()
                .query_one::<&mut FramePass>(pass)
                .map(|mut pass| {
                    pass.get()
                        .set_attachments(attachments, frame.color_view().clone());
                })
                .expect("the presenting pass exists");
        }
        Some(frame)
    }

    /// Rebuild the surface from the window after the swap chain was lost.
    ///
    /// A surface that cannot be rebuilt — the window is gone — leaves the
    /// current one in place, and the next acquire retries.
    fn recreate_surface(&mut self, device: &wgpu::Device) {
        if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
            surface.configure(device, &self.config);
            self.surface = Some(surface);
        }
    }
}

/// A swap-chain image acquired for one frame.
pub struct Frame {
    surface_texture: wgpu::SurfaceTexture,
    color_view: ResHandle<TextureView>,
}

impl Frame {
    /// The image this frame draws into.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.surface_texture.texture
    }

    /// The frame's color view, in the frame's resource graph.
    ///
    /// This is what a caller binds as its render target — together with the
    /// depth and multisample views its own attachments hold.
    pub fn color_view(&self) -> &ResHandle<TextureView> {
        &self.color_view
    }

    /// Present the frame.
    pub fn present(self, queue: &wgpu::Queue) {
        queue.present(self.surface_texture);
    }

    /// Present the frame and park the pass that drew into it.
    ///
    /// Presenting destroys the frame's swap-chain image, so the pass parks
    /// here rather than holding a dead view until its next own park: a frame
    /// rendered in between — a screenshot tool's render, say — skips the
    /// pass rather than recording into the dead image.
    pub fn present_and_park(self, world: &World, queue: &wgpu::Queue, pass: Entity) {
        self.present(queue);
        let _ = world
            .root_scope()
            .query_one::<&mut FramePass>(pass)
            .map(|mut pass| pass.get().park());
    }
}

/// The configuration to present `surface` with on `adapter`.
///
/// # Panics
///
/// If `adapter` cannot present to `surface`.
fn surface_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    width: u32,
    height: u32,
    options: SurfaceOptions,
) -> wgpu::SurfaceConfiguration {
    let caps = surface.get_capabilities(adapter);
    let view_formats_supported = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::SURFACE_VIEW_FORMATS);
    let (format, view_formats) = surface_format(&caps.formats, view_formats_supported);
    let present_mode = resolve_present_mode(&caps.present_modes, options.present_mode);
    if present_mode != options.present_mode
        && !matches!(
            options.present_mode,
            wgpu::PresentMode::AutoVsync | wgpu::PresentMode::AutoNoVsync
        )
    {
        log::warn!(
            "the surface does not offer the requested present mode {:?}; presenting with \
             {present_mode:?} instead (the surface offers {:?})",
            options.present_mode,
            caps.present_modes
        );
    }
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        // A surface cannot be configured with a zero dimension.
        width: width.max(1),
        height: height.max(1),
        present_mode,
        alpha_mode: caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto),
        view_formats,
        desired_maximum_frame_latency: options.desired_maximum_frame_latency,
    }
}

/// The present mode to configure a surface that offers `available` with, when
/// the caller asked for `requested`.
///
/// A surface that does not offer the requested mode is configured with
/// `Fifo` instead: the alternative is whatever the driver happens to list
/// first, which may be a mode that uncaps the framerate or tears, and neither
/// is what a caller asking for a particular mode wants as a surprise. Only a
/// surface that offers neither falls back to its first mode, because some mode
/// has to be chosen. The automatic modes pass through — wgpu resolves them
/// against the same capabilities.
fn resolve_present_mode(
    available: &[wgpu::PresentMode],
    requested: wgpu::PresentMode,
) -> wgpu::PresentMode {
    use wgpu::PresentMode;
    if matches!(requested, PresentMode::AutoVsync | PresentMode::AutoNoVsync) {
        return requested;
    }
    if available.contains(&requested) {
        return requested;
    }
    if available.contains(&PresentMode::Fifo) {
        return PresentMode::Fifo;
    }
    available.first().copied().unwrap_or(PresentMode::Fifo)
}

/// The format to configure a surface whose capabilities offer `formats` with,
/// together with the view formats to advertise alongside it.
///
/// The format is the first sRGB one. When the surface offers none, a format
/// with an sRGB counterpart is configured and that counterpart advertised as a
/// view — so the hardware still encodes the shader's output on write and the
/// display sees what an sRGB surface would have shown it.
///
/// Advertising that view needs
/// [`wgpu::DownlevelFlags::SURFACE_VIEW_FORMATS`], which the GLES and WebGL2
/// backends lack, so `view_formats_supported` gates it. Without it the frame is
/// rendered into the presented format itself: the built-in shaders write
/// sRGB-encoded colors, and a non-sRGB target passes them through unconverted,
/// which is the same result by a different route. A surface that is neither
/// sRGB nor has an sRGB counterpart — an fp16 one, say — is presented as it is.
///
/// # Panics
///
/// If `formats` is empty, which means the adapter cannot present to the
/// surface at all.
fn surface_format(
    formats: &[wgpu::TextureFormat],
    view_formats_supported: bool,
) -> (wgpu::TextureFormat, Vec<wgpu::TextureFormat>) {
    let srgb = formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::has_srgb_suffix);
    let format = srgb
        .or_else(|| {
            // A format that has an sRGB counterpart, so the frames can still be
            // encoded by hardware; the surface's own preference comes first.
            formats
                .iter()
                .copied()
                .find(|format| format.add_srgb_suffix() != *format)
        })
        .unwrap_or_else(|| {
            // Neither sRGB nor viewable as one. The frames are then rendered in
            // this format itself.
            *formats
                .first()
                .expect("the adapter can present to the surface")
        });
    // A format that already encodes sRGB needs no view, nor does one whose
    // backend cannot be given a view at all. `add_srgb_suffix` returns the
    // format itself when it has no counterpart, which the filter drops.
    let view_formats = (view_formats_supported && !format.has_srgb_suffix())
        .then(|| format.add_srgb_suffix())
        .filter(|srgb| *srgb != format)
        .into_iter()
        .collect();
    (format, view_formats)
}

/// The format a frame's color attachment is viewed as, for a surface
/// configured with `config`.
///
/// The swap chain's own format when it encodes sRGB. When it does not — the
/// web's canvas offers only non-sRGB formats, and writes to those are taken as
/// already encoded — the swap chain is configured with an sRGB view of the
/// format, so the hardware encodes the shader's linear output on write and the
/// display sees exactly what an sRGB swap chain would have shown it.
///
/// A backend that cannot be given that view has none to pick, so the frame is
/// rendered in the presented format and the shader's own values reach the
/// display unconverted — which is the right thing for the sRGB-encoded colors
/// the built-in shaders write, and why this falls back to `config.format`
/// rather than refusing to pick one.
fn color_format(config: &wgpu::SurfaceConfiguration) -> wgpu::TextureFormat {
    if config.format.has_srgb_suffix() {
        return config.format;
    }
    config
        .view_formats
        .iter()
        .copied()
        .find(|format| *format != config.format)
        .unwrap_or(config.format)
}

/// A graph view of `texture` reinterpreted as `format`.
///
/// Used for the swap chain's color image, which the surface configures with a
/// non-sRGB format on the web: the view is what makes the frame sRGB-encoded.
/// The format goes into the descriptor, so the view and the format the graph
/// records cannot disagree.
fn color_view_resource(texture: &wgpu::Texture, format: wgpu::TextureFormat) -> TextureView {
    TextureExt::create_view(
        texture,
        &wgpu::TextureViewDescriptor {
            format: Some(format),
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formats a GLES or WebGL2 surface offers: the non-sRGB pair, with
    /// the sRGB one added only when the platform's frame buffer can encode.
    const GLES_FORMATS: &[wgpu::TextureFormat] = &[
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
    ];

    /// A configuration with every field but the format at a harmless value, so
    /// a test can vary only what it is about.
    fn surface_configuration(format: wgpu::TextureFormat) -> wgpu::SurfaceConfiguration {
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 1,
            height: 1,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 2,
        }
    }

    #[test]
    fn an_srgb_surface_format_is_used_as_it_is() {
        let formats = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ];

        // Already sRGB, so there is nothing to view it as, on either backend.
        for supported in [true, false] {
            let (format, views) = surface_format(&formats, supported);
            assert_eq!(format, wgpu::TextureFormat::Bgra8UnormSrgb);
            assert!(views.is_empty(), "an sRGB format needs no view");
        }
    }

    #[test]
    fn a_surface_without_srgb_is_viewed_as_srgb_where_views_are_supported() {
        let (format, views) = surface_format(GLES_FORMATS, true);

        assert_eq!(format, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(views, vec![wgpu::TextureFormat::Rgba8UnormSrgb]);
    }

    #[test]
    fn a_surface_without_srgb_advertises_no_view_where_they_are_unsupported() {
        // GLES and WebGL2 have no `SURFACE_VIEW_FORMATS`, so configuring one
        // with a view is rejected outright. The frame is then rendered into the
        // presented format, which `color_format` follows.
        let (format, views) = surface_format(GLES_FORMATS, false);

        assert_eq!(format, wgpu::TextureFormat::Rgba8Unorm);
        assert!(views.is_empty());
        let config = wgpu::SurfaceConfiguration {
            format,
            view_formats: views,
            ..surface_configuration(format)
        };
        assert_eq!(color_format(&config), wgpu::TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn a_surface_with_no_srgb_counterpart_is_presented_as_it_is() {
        // An fp16 surface: neither sRGB nor viewable as one.
        let formats = [wgpu::TextureFormat::Rgba16Float];

        for supported in [true, false] {
            let (format, views) = surface_format(&formats, supported);
            assert_eq!(format, wgpu::TextureFormat::Rgba16Float);
            assert!(views.is_empty(), "there is no sRGB view of an fp16 format");
        }
    }

    #[test]
    fn a_requested_present_mode_the_surface_offers_is_kept() {
        use wgpu::PresentMode;

        let offers_mailbox = [PresentMode::Fifo, PresentMode::Mailbox];
        assert_eq!(
            resolve_present_mode(&offers_mailbox, PresentMode::Mailbox),
            PresentMode::Mailbox
        );
        // Fifo is kept even when the surface lists a mode with less waiting
        // first: a caller that did not ask for it keeps the display's pace.
        let mailbox_first = [PresentMode::Mailbox, PresentMode::Fifo];
        assert_eq!(
            resolve_present_mode(&mailbox_first, PresentMode::Fifo),
            PresentMode::Fifo
        );
    }

    #[test]
    fn a_present_mode_the_surface_lacks_degrades_to_fifo() {
        use wgpu::PresentMode;

        // WebGL2 and many mobile drivers offer Fifo alone.
        let fifo_only = [PresentMode::Fifo];
        for requested in [PresentMode::Immediate, PresentMode::Mailbox] {
            assert_eq!(
                resolve_present_mode(&fifo_only, requested),
                PresentMode::Fifo,
                "{requested:?} is not offered, so the display's pace wins"
            );
        }
    }

    #[test]
    fn a_surface_that_lacks_fifo_too_gets_the_first_mode_it_offers() {
        use wgpu::PresentMode;

        let no_fifo = [PresentMode::Immediate];
        assert_eq!(
            resolve_present_mode(&no_fifo, PresentMode::Mailbox),
            PresentMode::Immediate
        );
    }

    #[test]
    fn the_automatic_present_modes_are_passed_to_wgpu() {
        use wgpu::PresentMode;

        // wgpu resolves these against the same capabilities, by its own rules.
        let fifo_only = [PresentMode::Fifo];
        for requested in [PresentMode::AutoVsync, PresentMode::AutoNoVsync] {
            assert_eq!(resolve_present_mode(&fifo_only, requested), requested);
        }
    }
}
