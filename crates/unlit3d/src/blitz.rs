//! A Blitz document, painted as a frame source.
//!
//! [`BlitzSource`] lays out an HTML document with `blitz-dom` and rasterizes the
//! page with the CPU renderer of `anyrender_vello_cpu`, then composites the
//! result over whatever the frame already holds — the counterpart of
//! [`EguiSource`](crate::egui::EguiSource) for pages rather than widgets.
//!
//! The page is rasterized on the CPU because every GPU backend of Vello builds
//! an adapter of its own, and a frame source is handed nothing but the frame's
//! device, queue and graph. The pixmap therefore reaches the frame as an
//! ordinary upload of [`wgpu::TextureFormat::Rgba8Unorm`] texels: the bytes are
//! sRGB-encoded and premultiplied, which is what the CPU backend writes, so the
//! composite decodes them for an sRGB target exactly as it decodes egui's own
//! pictures.
//!
//! The document is described by its markup alone: nothing reaches it from the
//! network, the pointer or the keyboard. A page is laid out with the single
//! font the source registers — see [`build_single_font_ctx`] — rather than the
//! host's fonts, so the same markup paints the same glyphs on every target.
//!
//! ```rust
//! use unlit3d::prelude::*;
//! use unlit_wgpu::resources::ResourceGraph;
//!
//! let (device, queue) =
//!     wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut world = World::new();
//! let _ctx = spawn_context(
//!     &mut world,
//!     RenderDevice::unwatched(device, queue),
//!     ResourceGraph::new(),
//! );
//!
//! let source = BlitzSource::new(
//!     "<body style=\"background: #204060\"><p>hello</p></body>",
//! );
//! world.root_scope_mut().spawn(bundle![Source::new(source)]);
//! ```

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::FontContext;
use blitz_dom::{BaseDocument, DocumentConfig, build_single_font_ctx};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use unlit_ecs::Scope;
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};
use unlit_wgpu::scene::{DrawOrder, Scene};

use crate::input::InputEvents;
use crate::pass::PassInfo;
use crate::screen::ScreenBlit;
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};

/// The format the painted page is uploaded in.
///
/// Plain rather than sRGB: the CPU backend's bytes are already sRGB-encoded, and
/// a plain texture hands them to the fragment untouched for the composite to
/// decode once for the target.
const PAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A page painted over the frame, kept across frames.
pub struct BlitzSource {
    /// The markup the document is built from.
    html: String,
    /// The fonts every document this source builds is laid out with.
    fonts: FontContext,
    /// The laid-out document, rebuilt whenever the markup changes.
    document: Option<BaseDocument>,
    /// The CPU rasterizer, sized to the last target it was told about.
    painter: Option<VelloCpuImageRenderer>,
    /// The size the painter and [`Self::pixels`] currently hold.
    painted_size: (u32, u32),
    /// The premultiplied RGBA8 texels of the last painted frame.
    pixels: Vec<u8>,
    /// The viewport the document was last laid out with.
    viewport: Option<Viewport>,
    /// One uploaded page per target size.
    targets: Vec<TextureTarget>,
    /// The frame the targets were last pruned against.
    frame: u32,
    /// The composite that draws the page.
    screen: ScreenBlit,
}

/// The upload and the view sampling it, for one page size.
///
/// The page is the same whatever pass it is composited into, so a size is all
/// that tells two uploads apart: passes of one frame that draw at one size share
/// the page, and a pass that draws at another size gets its own.
struct TextureTarget {
    /// The size of the page, in texels.
    size: (u32, u32),
    /// The texture the page is uploaded into.
    texture: ResHandle<wgpu::Texture>,
    /// The view the composite samples it through.
    view: ResHandle<TextureView>,
    /// The last frame that drew this target.
    frame: u32,
}

impl BlitzSource {
    /// A source painting `html` over the frame.
    pub fn new(html: impl Into<String>) -> Self {
        Self {
            html: html.into(),
            fonts: build_single_font_ctx(epaint_default_fonts::HACK_REGULAR),
            document: None,
            painter: None,
            painted_size: (0, 0),
            pixels: Vec::new(),
            viewport: None,
            targets: Vec::new(),
            frame: 0,
            screen: ScreenBlit::new(DrawOrder::After),
        }
    }

    /// Replace the markup, so the next frame lays out and paints a new document.
    pub fn set_html(&mut self, html: impl Into<String>) {
        self.html = html.into();
        self.document = None;
    }
}

impl FrameSource for BlitzSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        // The graph settles on every way out of this build: the uploads and the
        // composite's own nodes are registered here and realised at the end of
        // the scope.
        let maintain = ctx.maintain_scope(*scope);

        let device = ctx
            .device(scope)
            .expect("the context's device resource exists");
        let queue = ctx
            .queue(scope)
            .expect("the context's queue resource exists");

        // The page is painted at the target's own pixel size, which is also what
        // the document states as its viewport: a logical pixel is a physical one
        // divided by the world's scale factor, so the two describe one page.
        let limit = device.limits().max_texture_dimension_2d;
        let size = (info.width.clamp(1, limit), info.height.clamp(1, limit));

        // One physical pixel per logical point until the world says otherwise.
        let scale_factor = scope
            .query::<&InputEvents>()
            .iter()
            .next()
            .map(|events| events.with_state(|state| state.scale_factor))
            .filter(|scale| *scale > 0.0)
            .unwrap_or(1.0);
        let viewport = Viewport::new(size.0, size.1, scale_factor, ColorScheme::Light);

        // The clock is the frame's own time base, so a sequence of frames paints
        // the same animations at the same times whatever the wall clock reads.
        let time = ctx
            .clock(scope)
            .map(|clock| clock.time as f64)
            .unwrap_or(0.0);

        let mut repaint = false;

        // A document is rebuilt only when the markup changed: laying out a page
        // parses its stylesheets and builds its layout tree, which the frames
        // after it do not need.
        if self.document.is_none() {
            let config = DocumentConfig {
                viewport: Some(viewport.clone()),
                font_ctx: Some(self.fonts.clone()),
                ..Default::default()
            };
            self.document = Some(HtmlDocument::from_html(&self.html, config).into_inner());
            self.viewport = Some(viewport.clone());
            // A page that was never painted leaves the texture empty.
            repaint = true;
        }
        let document = self
            .document
            .as_mut()
            .expect("the document was built above");

        if self.viewport.as_ref() != Some(&viewport) {
            document.set_viewport(viewport.clone());
            self.viewport = Some(viewport);
            repaint = true;
        }

        if self.painter.is_none() {
            self.painter = Some(VelloCpuImageRenderer::new(size.0, size.1));
            self.painted_size = size;
            repaint = true;
        } else if self.painted_size != size {
            if let Some(painter) = self.painter.as_mut() {
                painter.resize(size.0, size.1);
            }
            self.painted_size = size;
            repaint = true;
        }

        // A page repaints when the frame that built it changed, and otherwise
        // only while it animates: Blitz reports no damage once layout settled,
        // so a static page is rasterized exactly once per frame that needs it.
        if repaint || document.is_animating() {
            document.resolve(time);
            let painter = self.painter.as_mut().expect("the painter was built above");
            let paint_scale = document.viewport().scale_f64();
            painter.render_to_vec(
                |scene| {
                    blitz_paint::paint_scene(scene, document, paint_scale, size.0, size.1, 0, 0)
                },
                &mut self.pixels,
            );
            repaint = true;
        }

        // A size is all that tells two pages apart, so a target is reused for as
        // long as the frame keeps drawing at that size. One that no frame has
        // drawn for two frames is dropped, and the graph reclaims its resources
        // once the last handle is gone.
        let frame = ctx.clock(scope).map(|clock| clock.index).unwrap_or(0);
        if self.frame != frame {
            self.frame = frame;
            self.targets.retain(|target| target.frame + 1 >= frame);
        }

        let mut graph = ctx.graph(scope);
        let index = match self.targets.iter().position(|target| target.size == size) {
            Some(index) => index,
            None => {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("blitz::page"),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: PAGE_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view =
                    TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
                let graph = graph.get();
                let texture = graph.insert(texture, None);
                let view = graph.insert(view, None);
                graph.add_dependency(&view, &texture);
                self.targets.push(TextureTarget {
                    size,
                    texture,
                    view,
                    frame,
                });
                self.targets.len() - 1
            }
        };
        let target = &mut self.targets[index];
        target.frame = frame;
        // The target is created only for a size the painter was just resized or
        // freshly built for, which is a repaint: an existing one is rewritten
        // only when the page itself is.
        if repaint && let Some(texture) = graph.get().get(&target.texture) {
            queue.write_texture(
                texture.as_image_copy(),
                &self.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.0 * 4),
                    rows_per_image: Some(size.1),
                },
                wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = self.targets[index].view.clone();
        drop(graph);

        self.screen.record(
            scope,
            ctx,
            info,
            scene,
            view,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        drop(maintain);
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::BLITZ
    }
}
