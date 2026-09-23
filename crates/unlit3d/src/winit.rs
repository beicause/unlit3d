//! Presenting a [`Renderer`] into a [winit](https://docs.rs/winit) window.
//!
//! A window presents through a swap chain, which the renderer's own
//! attachment helpers do not cover: every frame hands out a new color image,
//! and the depth and multisample attachments have to match the window's size.
//! [`WindowSurface`] owns the surface, its configuration and those
//! attachments, keeps them registered in the renderer's resource graph, and
//! binds each frame's image as the renderer's render target — so a frame loop
//! is acquire, render, present.
//!
//! The caller creates the `wgpu::Surface` itself, because the adapter has to
//! be requested with it as the compatible surface. That request is async —
//! on the web it resolves on the browser's task queue, so a frame loop must not
//! block on it — and so is the device's, which leaves both to the caller:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use unlit3d::prelude::*;
//! use unlit3d::winit::WindowSurface;
//! use winit::window::Window;
//!
//! # fn setup(
//! #     world: &mut LocalWorld,
//! #     renderer: Entity,
//! #     instance: &wgpu::Instance,
//! #     adapter: &wgpu::Adapter,
//! #     window: Arc<Window>,
//! #     surface: wgpu::Surface<'static>,
//! # ) {
//! // The renderer the surface draws through, spawned once as a resource.
//! world
//!     .with_mut::<Renderer, _>(renderer, |r| {
//!         WindowSurface::new(r, instance, adapter, window, surface, 4)
//!     })
//!     .unwrap();
//! # }
//! ```
//!
//! and the frame loop then only has to render and present:
//!
//! ```no_run
//! # use unlit3d::prelude::*;
//! # use unlit3d::winit::WindowSurface;
//! # fn frame(renderer: &mut Renderer, window_surface: &mut WindowSurface, world: &LocalWorld) {
//! let Some(frame) = window_surface.acquire(renderer) else {
//!     return;
//! };
//! renderer.render(world);
//! frame.present(&renderer.queue);
//! # }
//! ```

use std::sync::Arc;

use wgpu_unlit_render::render_attachments::default_depth_stencil_format;
use wgpu_unlit_render::resources::{Resource, ResourceId};

use crate::renderer::Renderer;

/// A winit window's swap chain, bound to a [`Renderer`].
///
/// The surface is configured for the window's current size and the
/// depth-stencil — and, when multisampling, the multisample — attachments are
/// registered in the renderer's resource graph. [`Self::acquire`] binds the
/// frame's color image as the renderer's render target; [`Self::resize`] keeps
/// everything in step with the window.
///
/// The sample count is fixed when the surface is created. The renderer
/// specializes every pipeline on the sample count the bound attachments
/// report, so the options a key starts from do not have to agree with it — but
/// the sample count does have to be one the device supports.
pub struct WindowSurface {
    instance: wgpu::Instance,
    window: Arc<::winit::window::Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    sample_count: u32,
    /// The format the frame's color attachment is viewed as.
    ///
    /// Usually the swap chain's own format; an sRGB view of it when the
    /// surface offers no sRGB format of its own.
    color_format: wgpu::TextureFormat,
    /// The depth-stencil view, in the renderer's graph.
    depth_view: ResourceId,
    /// The multisample view, in the renderer's graph; `None` when the frames
    /// are not multisampled.
    msaa_view: Option<ResourceId>,
    /// The color view of the most recently acquired frame, in the renderer's
    /// graph. One id is kept for the surface's whole life; `None` until the
    /// first frame is acquired.
    color_view: Option<ResourceId>,
}

impl WindowSurface {
    /// Configure `surface` for `window` and allocate the attachments the
    /// renderer draws with.
    ///
    /// `surface` must have been created from `window`, and `adapter` must be
    /// able to present to it — the adapter the renderer's device was requested
    /// from. `sample_count` is the number of samples every frame is rendered
    /// with; `1` disables multisampling.
    ///
    /// The surface format is the first sRGB format the surface supports. A
    /// surface that supports none — the web's canvas does not — is configured
    /// with a non-sRGB format instead and its frames are viewed as sRGB, which
    /// encodes them on write in exactly the same way. An sRGB attachment is
    /// what lets the built-in unlit shader's colors reach the display
    /// unchanged.
    ///
    /// # Panics
    ///
    /// If `adapter` cannot present to `surface`.
    pub fn new(
        renderer: &mut Renderer,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        window: Arc<::winit::window::Window>,
        surface: wgpu::Surface<'static>,
        sample_count: u32,
    ) -> Self {
        let size = window.inner_size();
        let config = surface_config(&surface, adapter, size.width, size.height);
        let color_format = color_format(&config);
        surface.configure(&renderer.device, &config);

        let (depth, msaa) = create_attachments(
            &renderer.device,
            color_format,
            config.width,
            config.height,
            sample_count,
        );
        let depth_view = renderer
            .graph
            .insert_strong(view_resource(&depth), &[])
            .expect("a texture view has no dependencies");
        let msaa_view = msaa.map(|texture| {
            renderer
                .graph
                .insert_strong(view_resource(&texture), &[])
                .expect("a texture view has no dependencies")
        });

        Self {
            instance: instance.clone(),
            window,
            surface,
            config,
            sample_count,
            color_format,
            depth_view,
            msaa_view,
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

    /// The window the surface presents into.
    ///
    /// The surface owns a handle to it, so a caller that needs the window —
    /// to request a redraw, or to read its size — can borrow it back here.
    pub fn window(&self) -> &Arc<::winit::window::Window> {
        &self.window
    }

    /// Reconfigure the surface and rebuild the attachments for a new size.
    ///
    /// A zero width or height — a minimized window — is ignored: a surface
    /// cannot be configured with a zero dimension, and there is nothing to
    /// draw.
    pub fn resize(&mut self, renderer: &mut Renderer, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&renderer.device, &self.config);

        let (depth, msaa) = create_attachments(
            &renderer.device,
            self.color_format,
            width,
            height,
            self.sample_count,
        );
        renderer
            .graph
            .replace(self.depth_view, view_resource(&depth))
            .expect("the depth view is in the graph");
        if let (Some(id), Some(texture)) = (self.msaa_view, msaa) {
            renderer
                .graph
                .replace(id, view_resource(&texture))
                .expect("the multisample view is in the graph");
        }
    }

    /// Acquire the next frame and bind it as the renderer's render target.
    ///
    /// Returns `None` when the frame should be skipped — the window is
    /// occluded or minimized, or the swap chain had to be reconfigured. The
    /// renderer must not be asked to render in that case.
    ///
    /// The frame's image replaces the previous one in the resource graph, so
    /// at most the frame just presented is still referenced — never a longer
    /// history of swap-chain images.
    pub fn acquire(&mut self, renderer: &mut Renderer) -> Option<Frame> {
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&renderer.device, &self.config);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.recreate_surface(renderer);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return None,
        };

        // The color view keeps one id for the surface's whole life: replacing
        // the resource swaps in the new frame's view and drops the previous
        // one, and a skipped frame leaves the id — and the target bound to it —
        // untouched.
        let view = color_view_resource(&surface_texture.texture, self.color_format);
        let color = match self.color_view {
            Some(id) => {
                renderer
                    .graph
                    .replace(id, view)
                    .expect("the color view is in the graph");
                id
            }
            None => renderer
                .graph
                .insert_strong(view, &[])
                .expect("a texture view has no dependencies"),
        };
        self.color_view = Some(color);
        renderer.set_render_target(Some(color), Some(self.depth_view), self.msaa_view);
        Some(Frame { surface_texture })
    }

    /// Rebuild the surface from the window after the swap chain was lost.
    ///
    /// A surface that cannot be rebuilt — the window is gone — leaves the
    /// current one in place, and the next acquire retries.
    fn recreate_surface(&mut self, renderer: &Renderer) {
        if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
            self.surface = surface;
            self.surface.configure(&renderer.device, &self.config);
        }
    }
}

/// A swap-chain image acquired for one frame.
pub struct Frame {
    surface_texture: wgpu::SurfaceTexture,
}

impl Frame {
    /// The image this frame draws into.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.surface_texture.texture
    }

    /// Present the frame.
    pub fn present(self, queue: &wgpu::Queue) {
        queue.present(self.surface_texture);
    }
}

/// The configuration to present `surface` with on `adapter`.
///
/// The format is the surface's first sRGB one; when it supports none, the
/// configuration advertises an sRGB view of a non-sRGB format instead (see
/// [`color_format`]).
///
/// # Panics
///
/// If `adapter` cannot present to `surface`.
fn surface_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    width: u32,
    height: u32,
) -> wgpu::SurfaceConfiguration {
    let caps = surface.get_capabilities(adapter);
    let format = srgb_format(&caps.formats).or_else(|| {
        // A format that has an sRGB counterpart, so the frames can still be
        // encoded by hardware; the surface's own preference comes first.
        caps.formats
            .iter()
            .copied()
            .find(|format| format.add_srgb_suffix() != *format)
    });
    let format = format.unwrap_or_else(|| {
        // Neither sRGB nor viewable as one, as an fp16 surface might be. The
        // frames are then rendered in this format itself.
        *caps
            .formats
            .first()
            .expect("the adapter can present to the surface")
    });
    let srgb = format.add_srgb_suffix();
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        // A surface cannot be configured with a zero dimension.
        width: width.max(1),
        height: height.max(1),
        present_mode: caps
            .present_modes
            .first()
            .copied()
            .unwrap_or(wgpu::PresentMode::Fifo),
        alpha_mode: caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto),
        // Only the sRGB counterpart of the configured format is ever viewed
        // through; anything else would need a view descriptor of its own.
        view_formats: (srgb != format).then_some(srgb).into_iter().collect(),
        desired_maximum_frame_latency: 2,
    }
}

/// The surface's first sRGB format.
fn srgb_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    formats.iter().copied().find(wgpu::TextureFormat::is_srgb)
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
/// # Panics
///
/// If `config.format` is neither sRGB nor listed among its own
/// `view_formats`.
fn color_format(config: &wgpu::SurfaceConfiguration) -> wgpu::TextureFormat {
    if config.format.is_srgb() {
        return config.format;
    }
    config
        .view_formats
        .iter()
        .copied()
        .find(|format| *format != config.format)
        .unwrap_or(config.format)
}

/// The depth-stencil and, when `sample_count > 1`, multisample textures a
/// frame draws with.
///
/// Both are sized `width` x `height` and transient: they are cleared and
/// discarded inside the frame's single pass, so nothing is ever read back from
/// them.
///
/// `color_format` is the format the frame's color attachment is viewed as (see
/// [`WindowSurface::color_format`]), which the multisample texture carries too:
/// its pixels are resolved into the color view, and wgpu requires the two to be
/// in the same format.
fn create_attachments(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    sample_count: u32,
) -> (wgpu::Texture, Option<wgpu::Texture>) {
    let extent = wgpu::Extent3d {
        width: width.max(1),
        height: height.max(1),
        depth_or_array_layers: 1,
    };
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unlit3d::winit::depth"),
        size: extent,
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: default_depth_stencil_format(device),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
        view_formats: &[],
    });
    let msaa = (sample_count > 1).then(|| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("unlit3d::winit::msaa"),
            size: extent,
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: color_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
            view_formats: &[],
        })
    });
    (depth, msaa)
}

/// A graph resource holding a default view of `texture`.
///
/// Used for the depth and multisample attachments, whose views are always in
/// their texture's own format.
fn view_resource(texture: &wgpu::Texture) -> Resource {
    Resource::from(texture.create_view(&wgpu::TextureViewDescriptor::default()))
}

/// A graph resource holding a view of `texture` reinterpreted as `format`.
///
/// Used for the swap chain's color image, which the surface configures with a
/// non-sRGB format on the web: the view is what makes the frame sRGB-encoded.
fn color_view_resource(texture: &wgpu::Texture, format: wgpu::TextureFormat) -> Resource {
    Resource::TextureView {
        view: texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format),
            ..Default::default()
        }),
        format,
    }
}
