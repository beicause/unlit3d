//! Presenting a [`Renderer`] into a [winit](https://docs.rs/winit) window.
//!
//! A window presents through a swap chain, which the frame's own attachment
//! helpers do not cover: every frame hands out a new color image, and the
//! depth and multisample attachments have to match the window's size.
//! [`WindowSurface`] owns the surface, its configuration and those
//! attachments, keeps them registered in the frame's resource graph, and binds
//! each frame's image as the renderer's render target — so a frame loop is
//! acquire, render, present.
//!
//! The caller creates the `wgpu::Surface` itself, because the adapter has to
//! be requested with it as the compatible surface. That request is async —
//! on the web it resolves on the browser's task queue, so a frame loop must not
//! block on it — and so is the device's, which leaves both to the caller:
//!
//! ```
//! use std::sync::Arc;
//!
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
//! # ) {
//! // The renderer the surface draws through, spawned once as a resource.
//! world
//!     .with_mut::<Renderer, _>(renderer, |r| {
//!         WindowSurface::new(world, r, instance, adapter, window, surface, 4)
//!     })
//!     .unwrap();
//! # }
//! ```
//!
//! and the frame loop then only has to render and present:
//!
//! ```
//! # use unlit3d::prelude::*;
//! # use unlit3d::winit::WindowSurface;
//! # fn frame(
//! #     world: &World,
//! #     queue: &wgpu::Queue,
//! #     renderer_entity: Entity,
//! #     window_surface: &mut WindowSurface,
//! # ) {
//! let Some(frame) = world
//!     .with_mut::<Renderer, _>(renderer_entity, |r| window_surface.acquire(world, r))
//!     .unwrap()
//! else {
//!     return;
//! };
//! world
//!     .with_mut::<Renderer, _>(renderer_entity, |r| r.render(world))
//!     .unwrap();
//! frame.present(queue);
//! # }
//! ```

use std::sync::Arc;

use unlit_ecs::World;
use unlit_wgpu::render_attachments::default_depth_stencil_format;
use unlit_wgpu::resources::{ResourceId, TextureExt, TextureView};

use crate::renderer::Renderer;

/// The device the frame's context was created with, cloned out of `world`.
///
/// # Panics
///
/// If the context's device resource is gone.
fn context_device(world: &World, renderer: &Renderer) -> wgpu::Device {
    world
        .get::<wgpu::Device>(renderer.context().device)
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
    renderer: &Renderer,
) -> impl core::ops::DerefMut<Target = unlit_wgpu::resources::ResourceGraph> + 'w {
    world
        .get_mut::<unlit_wgpu::resources::ResourceGraph>(renderer.context().graph)
        .expect("the context's resource graph exists")
}

/// A winit window's swap chain, presented through a [`Renderer`].
///
/// The surface is configured for the window's current size and the
/// depth-stencil — and, when multisampling, the multisample — attachments are
/// registered in the frame's resource graph. [`Self::acquire`] binds the
/// frame's color image as the renderer's render target; [`Self::resize`] keeps
/// everything in step with the window.
///
/// A platform can invalidate the render surface while the app is not in the
/// foreground, and Android does for as long as it is suspended. [`Self::release`]
/// is for that: it drops the swap chain and its attachments without touching the
/// device, the pipelines or anything else the frame draws from, so a scene keeps
/// its state and presents again through a surface built from the same window.
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
    /// The depth-stencil view, in the frame's graph.
    depth_view: ResourceId<TextureView>,
    /// The multisample view, in the frame's graph; `None` when the frames
    /// are not multisampled.
    msaa_view: Option<ResourceId<TextureView>>,
    /// The color view of the most recently acquired frame, in the frame's
    /// graph. One id is kept for the surface's whole life; `None` until the
    /// first frame is acquired.
    color_view: Option<ResourceId<TextureView>>,
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
        world: &World,
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
        let device = context_device(world, renderer);
        surface.configure(&device, &config);

        let (depth, msaa) = create_attachments(
            &device,
            color_format,
            config.width,
            config.height,
            sample_count,
        );
        let mut graph = context_graph(world, renderer);
        let depth_view = graph.insert_strong(view_resource(&depth));
        let msaa_view = msaa.map(|texture| graph.insert_strong(view_resource(&texture)));
        drop(graph);

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
    pub fn resize(&mut self, world: &World, renderer: &mut Renderer, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        let device = context_device(world, renderer);
        self.surface.configure(&device, &self.config);

        let (depth, msaa) =
            create_attachments(&device, self.color_format, width, height, self.sample_count);
        let mut graph = context_graph(world, renderer);
        graph
            .replace(self.depth_view, view_resource(&depth))
            .expect("the depth view is in the graph");
        if let (Some(id), Some(texture)) = (self.msaa_view, msaa) {
            graph
                .replace(id, view_resource(&texture))
                .expect("the multisample view is in the graph");
        }
    }

    /// Release the swap chain.
    ///
    /// Drops the surface and the attachments it registered in the frame's
    /// graph, and unsets the renderer's render target, so nothing is left
    /// naming a swap chain that is gone. The device, the pipelines and
    /// everything else the scene draws from are untouched: the caller's own
    /// window presents the same scene again through a surface built from it.
    ///
    /// This is what a suspension needs. Android invalidates the native surface
    /// for as long as the app is not in the foreground, which outlives the swap
    /// chain but not the window, the scene or the GPU context.
    pub fn release(self, world: &World, renderer: &mut Renderer) {
        // The swap chain's image is the bound render target, so it is unset
        // before its view goes: an id left naming a removed view is not merely
        // dangling, because the graph recycles its slot — the renderer would
        // then either panic on a missing view or, worse, draw into whatever
        // unrelated texture took the slot.
        renderer.unset_render_target(world);

        let mut graph = context_graph(world, renderer);
        if let Some(id) = self.color_view {
            graph.remove_drop(id);
        }
        graph.remove_drop(self.depth_view);
        if let Some(id) = self.msaa_view {
            graph.remove_drop(id);
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
    pub fn acquire(&mut self, world: &World, renderer: &mut Renderer) -> Option<Frame> {
        let device = context_device(world, renderer);
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&device, &self.config);
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
        // one, and a skipped frame leaves the id — and the target bound to it —
        // untouched.
        let view = color_view_resource(&surface_texture.texture, self.color_format);
        let color = match self.color_view {
            Some(id) => {
                context_graph(world, renderer)
                    .replace(id, view)
                    .expect("the color view is in the graph");
                id
            }
            None => context_graph(world, renderer).insert_strong(view),
        };
        self.color_view = Some(color);
        renderer.set_render_target(world, Some(color), Some(self.depth_view), self.msaa_view);
        Some(Frame { surface_texture })
    }

    /// Rebuild the surface from the window after the swap chain was lost.
    ///
    /// A surface that cannot be rebuilt — the window is gone — leaves the
    /// current one in place, and the next acquire retries.
    fn recreate_surface(&mut self, device: &wgpu::Device) {
        if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
            self.surface = surface;
            self.surface.configure(device, &self.config);
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
    let view_formats_supported = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::SURFACE_VIEW_FORMATS);
    let (format, view_formats) = surface_format(&caps.formats, view_formats_supported);
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
        view_formats,
        desired_maximum_frame_latency: 2,
    }
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
    let srgb = formats.iter().copied().find(wgpu::TextureFormat::is_srgb);
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
    let view_formats = (view_formats_supported && !format.is_srgb())
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

/// A graph view of `texture` in its own format.
///
/// Used for the depth and multisample attachments, whose views are always in
/// their texture's own format.
fn view_resource(texture: &wgpu::Texture) -> TextureView {
    TextureExt::create_view(texture, &wgpu::TextureViewDescriptor::default())
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
}
