//! A Blitz page, painted as a frame source.
//!
//! [`BlitzSource`] draws the [`BlitzDocument`] its scope carries: `blitz-dom` lays
//! the page out, `anyrender_vello_cpu` rasterizes it, and the result is
//! composited over whatever the frame already holds — the counterpart of
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
//! A page is not a picture of one. The source feeds it the frame's input and
//! lets Blitz dispatch it, so hover, focus, scrolling, text editing and the
//! checked state of a form control all happen in the document, and the page
//! loads what its markup references from the bytes the document serves. What
//! Blitz cannot do without a scripting engine is left to [`OnBlitzEvent`]: a
//! Rust callback sees every DOM event and may read or change the page there.
//!
//! A page is laid out with the single font `blitz-dom` registers for it rather
//! than the host's fonts, so the same markup paints the same glyphs on every
//! target.
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
//! let source = world.root_scope_mut().spawn(bundle![Source::new(BlitzSource::new())]);
//! world.scope_mut(source).expect("the source is alive").spawn(bundle![
//!     BlitzDocument::new("<body style=\"background: #204060\"><p>hello</p></body>"),
//! ]);
//! ```

use std::sync::Arc;

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_traits::shell::{ColorScheme, Viewport};
use unlit_ecs::{Entity, Scope};
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};
use unlit_wgpu::scene::{DrawOrder, Scene};

use crate::input::{InputEvent, InputEvents, MouseButtons};
use crate::pass::PassInfo;
use crate::screen::ScreenBlit;
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};

mod document;
mod events;

pub use document::BlitzDocument;
pub use events::{BlitzEvent, OnBlitzEvent};

use events::{BlitzHandler, Frame, Translator};

/// The format the painted page is uploaded in.
///
/// Plain rather than sRGB: the CPU backend's bytes are already sRGB-encoded, and
/// a plain texture hands them to the fragment untouched for the composite to
/// decode once for the target.
const PAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Paints the Blitz page its scope carries.
///
/// A page is not a field of the source but a [`BlitzDocument`] entity beneath
/// it, so the markup lives in the world like any other behaviour: the code that
/// owns the entity replaces it, serves the bytes it loads and watches its
/// events. The source draws the first page its scope carries, at the size of the
/// pass it is drawn in, and paints nothing when there is no page.
///
/// One source drives one page, and the page keeps what a document keeps — its
/// layout, its form state, its scroll position — for as long as it is drawn.
pub struct BlitzSource {
    /// Converts the frame's input into the events a document dispatches.
    translator: Translator,
    /// The frame's input events, copied out of the world to be translated.
    batch: Vec<InputEvent>,
    /// The renderer the page is rasterized with.
    painter: Option<VelloCpuImageRenderer>,
    /// The size the painter was last built or resized for.
    painted_size: (u32, u32),
    /// The page's pixels, kept between frames so a repaint reuses the buffer.
    pixels: Vec<u8>,
    /// One uploaded texture per size the page is drawn at.
    targets: Vec<TextureTarget>,
    /// The frame the targets were last used in.
    frame: u32,
    /// The composite the page reaches the frame through.
    screen: ScreenBlit,
}

/// A page's pixels uploaded at one size.
///
/// A size is all that tells two pages apart, so a target is reused for as long
/// as the frame keeps drawing at that size; one that no frame has drawn for two
/// frames is dropped.
struct TextureTarget {
    /// The size the texture was created at.
    size: (u32, u32),
    /// The uploaded page.
    texture: ResHandle<wgpu::Texture>,
    /// The view the composite samples.
    view: ResHandle<TextureView>,
    /// The last frame this target was drawn in.
    frame: u32,
}

impl BlitzSource {
    /// Paint the page the source is spawned with.
    ///
    /// The page is a component, so a source on its own paints nothing until a
    /// [`BlitzDocument`] is spawned beneath it.
    pub fn new() -> Self {
        Self {
            translator: Translator::default(),
            batch: Vec::new(),
            painter: None,
            painted_size: (0, 0),
            pixels: Vec::new(),
            targets: Vec::new(),
            frame: 0,
            screen: ScreenBlit::new(DrawOrder::After),
        }
    }
}

impl Default for BlitzSource {
    fn default() -> Self {
        Self::new()
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

        // The page lives in the world, so a source with no page beneath it
        // draws nothing rather than a blank rectangle of its own.
        let Some(page) = first_page(scope) else {
            return;
        };
        let Ok(mut row) = scope.query_one::<&mut BlitzDocument>(page) else {
            return;
        };
        let page = row.get();

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
        let mut batch = core::mem::take(&mut self.batch);
        let mut scale_factor = 1.0;
        let mut cursor = None;
        let mut buttons = MouseButtons::empty();
        {
            let mut input = scope.query::<&InputEvents>();
            if let Some(events) = input.iter().next() {
                let state =
                    events.with_state(|state| (state.scale_factor, state.cursor, state.buttons));
                scale_factor = state.0;
                cursor = state.1;
                buttons = state.2;
                events.with_batch(|events| batch.extend_from_slice(events));
            }
        }
        if scale_factor <= 0.0 {
            scale_factor = 1.0;
        }

        let viewport = Viewport::new(size.0, size.1, scale_factor, ColorScheme::Light);
        // The clock is the frame's own time base, so a sequence of frames paints
        // the same animations at the same times whatever the wall clock reads.
        let time = ctx
            .clock(scope)
            .map(|clock| clock.time as f64)
            .unwrap_or(0.0);

        // The page asks for a frame whenever it changed: a new document, new
        // markup, a different viewport, a resource that landed, or a callback
        // that touched it. Taking the request is what turns it into this frame's
        // repaint.
        let shell = Arc::clone(page.shell());
        let document = page.document(viewport);
        let scroll = document.viewport_scroll();

        // Input first, so what the callbacks change is what this frame paints.
        let events = self.translator.translate(Frame {
            batch: &batch,
            scale: scale_factor,
            cursor,
            buttons,
            scroll: [scroll.x, scroll.y],
        });
        let mut handler = BlitzHandler::new(*scope, &shell);
        for event in events {
            handler.dispatch(&mut *document, event);
        }
        self.batch = batch;

        let mut repaint = shell.take_redraw();

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
        // A page still waiting for a stylesheet has nothing to paint yet, and it
        // asks for another frame when the bytes land.
        if (repaint || document.is_animating()) && !document.has_pending_critical_resources() {
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

/// The entity of the first page this scope carries.
fn first_page(scope: &Scope<'_>) -> Option<Entity> {
    let mut pages = scope.query::<(&BlitzDocument, Entity)>();
    let mut rows = pages.iter();
    rows.next().map(|(_document, entity)| entity)
}
