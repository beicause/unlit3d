//! A Blitz page, painted as a draw source.
//!
//! [`BlitzSource`] draws the [`BlitzDocument`] its scope carries: `blitz-dom` lays
//! the page out, `blitz-paint` records what it painted, and Vello's GPU backend
//! rasterizes the recording into a texture the frame composites — the
//! counterpart of [`EguiSource`](crate::egui::EguiSource) for pages rather than
//! widgets.
//!
//! The page is rasterized in the build phase of the frame, into a texture of
//! [`wgpu::TextureFormat::Rgba8Unorm`] at the pass's own size. Vello draws in
//! sRGB-encoded values and writes them premultiplied, which is what the
//! composite expects of a plain texture: it decodes them once for an sRGB
//! target exactly as it decodes egui's own pictures.
//!
//! A page is not a picture of one. The source feeds it the frame's input and
//! lets Blitz dispatch it, so hover, focus, scrolling, text editing and the
//! checked state of a form control all happen in the document, and the page
//! loads what its markup references from the bytes the document serves. What
//! Blitz cannot do without a scripting engine is left to [`OnBlitzEvent`]: a
//! Rust callback sees every DOM event and may read or change the page there.
//!
//! A page is laid out with the fonts the crate carries rather than the host's,
//! so the same markup paints the same glyphs on every target. The default font
//! carries the Latin text and the bundled Noto Sans SC subset the Chinese
//! characters; `blitz_system_fonts` adds the host's fonts behind them for
//! whatever they do not cover.
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
//! let source = world.root_scope_mut().spawn(bundle![Source::new(SourceOrder::BLITZ, BlitzSource::new())]);
//! world.scope_mut(source).spawn(bundle![
//!     BlitzDocument::new("<body style=\"background: #204060\"><p>hello</p></body>"),
//! ]);
//! ```

use std::sync::Arc;

use blitz_traits::shell::{ColorScheme, ShellProvider as _, Viewport};
use unlit_ecs::{Entity, Scope};
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};
use unlit_wgpu::scene::{DrawOrder, Scene};

use crate::input::{InputBaseline, InputEvent, InputEvents, InputState};
use crate::screen::ScreenBlit;
use crate::source::{SceneBuild, SceneSource};
use crate::target::{FrameId, RenderTargetId};

mod document;
mod events;
mod fonts;
mod keys;
mod painter;

pub use document::BlitzDocument;
pub use events::{BlitzEvent, OnBlitzEvent};

use events::{BlitzHandler, Frame, Translator};
use painter::Painter;

/// The format a page is rasterized and composited in.
///
/// Plain rather than sRGB: Vello computes in sRGB-encoded values, so a plain
/// texture hands them to the composite untouched, which decodes them once for
/// the target. An sRGB texture would encode them a second time.
const PAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Paints the Blitz page its scope carries.
///
/// A page is not a field of the source but a [`BlitzDocument`] entity beneath
/// it, so the markup lives in the world like any other behaviour: the code that
/// owns the entity replaces it, serves the bytes it loads and watches its
/// events. The source draws the first page its scope carries, at the size of the
/// pass it is drawn in, and paints nothing when there is no page.
///
/// A page keeps what a document keeps — its layout, its form state, its scroll
/// position — for as long as it is drawn, so a source that draws it is drawing
/// the state the page is in rather than a copy of it: two sources that see one
/// page paint the same page, and the tick's input reaches it once, from
/// whichever of them the frame builds first. A page below no source is never
/// painted at all.
pub struct BlitzSource {
    /// Converts the frame's input into the events a document dispatches.
    translator: Translator,
    /// The last tick's input events, copied out of the world so it is not
    /// borrowed while the document it feeds is. Kept between frames to be
    /// reused.
    batch: Vec<InputEvent>,
    /// The state the copied batch was queued against, copied out beside it for
    /// the same reason. Kept between frames to be reused.
    start: InputBaseline,
    /// One uploaded texture and one rasterizer per target and size the page is
    /// drawn at.
    targets: Vec<TextureTarget>,
    /// The frame the targets were last used in.
    frame: FrameId,
    /// Whether the world was focused on the last frame this source was built.
    focused: bool,
    /// The composite the page reaches the frame through.
    screen: ScreenBlit,
}

/// A page's pixels uploaded for one target, at one size.
///
/// The target and the size say which texture a page's pixels go into; the
/// generation says which page they hold. A target is reused for as long as the
/// frame keeps drawing at that target and size, and one that no frame has drawn
/// for two frames is dropped.
///
/// The rasterizer is the target's rather than the source's, so a page drawn into
/// two differently sized passes is recorded once for each of them and reused
/// for as long as the layout holds: a page that is painted at one size on one
/// frame and at another on the next would otherwise be laid out again on every
/// frame.
struct TextureTarget {
    /// The target the texture is composed into.
    id: RenderTargetId,
    /// The size the texture was created at.
    size: (u32, u32),
    /// The drawing commands of the page, and what rasterizes them.
    painter: Painter,
    /// The rasterized page.
    ///
    /// It depends on nothing but the texture created with it, which the graph
    /// keeps alive through this handle for as long as the target lists it.
    view: ResHandle<TextureView>,
    /// The last frame this target was drawn in.
    frame: FrameId,
    /// The generation of the page uploaded into it, or `None` while it holds
    /// nothing.
    ///
    /// The page can change under a texture that was already drawn — new markup,
    /// a new base — and that change may be laid out a frame later than the one
    /// that made it. A texture holding an older page is not composed, so the
    /// frame shows nothing where the page is rather than the page before it.
    painted: Option<u64>,
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
            start: InputBaseline::default(),
            targets: Vec::new(),
            frame: FrameId::default(),
            focused: true,
            screen: ScreenBlit::new(DrawOrder::After),
        }
    }
}

impl Default for BlitzSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneSource for BlitzSource {
    fn build(&mut self, build: SceneBuild<'_>, scene: &mut Scene) {
        let SceneBuild {
            scope,
            ctx,
            info,
            encoder,
        } = build;
        let scope = &scope;
        // The graph settles on every way out of this build: the uploads and the
        // composite's own nodes are registered here and realised at the end of
        // the scope.
        let maintain = ctx.maintain_scope(*scope);

        // The page lives in the world, so a source with no page beneath it
        // draws nothing rather than a blank rectangle of its own.
        let Some(page) = first_page(scope) else {
            return;
        };
        let Ok(mut row) = scope.try_query_one::<&mut BlitzDocument>(page) else {
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
        //
        // The rasterizer addresses its pixmap with 16-bit extents, like the
        // textures it is uploaded into, so a target larger than that is painted
        // at the largest size either of them can name.
        let limit = device
            .limits()
            .max_texture_dimension_2d
            .min(u32::from(u16::MAX));
        let size = (info.width.clamp(1, limit), info.height.clamp(1, limit));

        // The state the world's events left behind describes the devices now,
        // so it is read whether or not this build dispatches a batch: the
        // pointer a page hovers with is where the pointer is.
        let mut batch = core::mem::take(&mut self.batch);
        batch.clear();
        // One physical pixel per logical point until the world says otherwise.
        let mut scale_factor = 1.0;
        let mut focused = true;
        {
            // The singleton key names the one entity holding the buffer and the
            // state beside it, so both come from the same tick.
            let input = scope.singleton::<InputEvents>().and_then(|entity| {
                scope
                    .try_query_one::<(&InputEvents, &InputState)>(entity)
                    .ok()
            });
            if let Some(mut row) = input {
                let (events, state) = row.get();
                // The size and scale factor are the window now, and a batch's
                // own resize is what this frame lays out for. The pointer state
                // is not: it is what the batch has already folded into, so it
                // is the wrong thing to replay the batch against.
                scale_factor = state.scale_factor;
                focused = state.focused;
                // A page is built by every source and pass that see it, and each
                // of them reads the same batch, so the page takes a tick's events
                // once: a click is one click however many times the frame draws
                // what it changed. A source the page has already been given this
                // tick still replays from the same start, so it stays in step
                // with the one that dispatched.
                events.copy_baseline_into(&mut self.start);
                if page.feeds_input(events.tick()) {
                    events.with_batch(|events| batch.extend_from_slice(events));
                }
            }
        }
        if scale_factor <= 0.0 {
            scale_factor = 1.0;
        }

        // A page is laid out for a light colour scheme: nothing in the world
        // states a preference, and one answer is what keeps a snapshot holding.
        // `prefers-color-scheme` therefore never matches.
        let viewport = Viewport::new(size.0, size.1, scale_factor, ColorScheme::Light);
        // The clock is the frame's own time base, so a sequence of frames paints
        // the same animations at the same times whatever the wall clock reads.
        let time = ctx
            .clock(scope)
            .map(|clock| clock.time as f64)
            .unwrap_or(0.0);

        let shell = Arc::clone(&page.shell);
        let generation = page.generation;
        let document = page.document(viewport);
        let scroll = document.viewport_scroll();
        let logical_size = document.viewport().logical_size();
        // A window that has lost focus takes the page's focus with it, or the
        // field the user was in keeps its caret and its editing state in a
        // window that is no longer theirs. Losing focus is the one change the
        // page does not announce: clearing a focus that is already unset changes
        // nothing and asks for nothing, so the frame that lost it repaints here.
        if !focused && self.focused {
            document.clear_focus();
            shell.request_redraw();
        }
        self.focused = focused;

        // The state behind the pointer is handed over on every frame, not only on
        // the ones that carry a batch: the scale factor a position is divided by
        // and the buttons the page holds change without an event of their own.
        let events = self.translator.translate(Frame {
            batch: &batch,
            scale: scale_factor,
            start: &self.start,
            scroll: [scroll.x, scroll.y],
            viewport: [logical_size.0, logical_size.1],
        });
        // Input first, so what the callbacks change is what this frame paints.
        let mut handler = BlitzHandler::new(*scope, &shell);
        for event in events {
            handler.dispatch(&mut *document, event);
        }
        self.batch = batch;

        // The page asks for a frame whenever it changed: a new document, new
        // markup, a different viewport, a resource that landed, or a callback
        // that touched it. Taking the request is what turns it into this frame's
        // repaint.
        let mut repaint = shell.take_redraw();

        // A target is kept for the target and the size it is drawn at, and one
        // that no frame has drawn for two frames is dropped, which leaves the
        // graph to reclaim its resources once the last handle is gone. The
        // rasterizer and the buffer it paints into live with the target, so a
        // page drawn at two sizes lays each of them out once rather than laying
        // the page out again on every frame that switches between them.
        let frame = info.frame;
        if self.frame != frame {
            self.frame = frame;
            self.targets.retain(|target| target.frame.keeps(frame));
        }

        let mut graph = ctx.graph(scope);
        let index = match self
            .targets
            .iter()
            .position(|target| target.id == info.target && target.size == size)
        {
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
                    // Vello renders into the texture through a pass of its own,
                    // so it is an attachment as well as a picture.
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view =
                    TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
                let graph = graph.get();
                let texture = graph.insert(texture, None);
                let view = graph.insert(view, None);
                // The view keeps the texture it was made from alive, so the
                // target only has to hold the view.
                graph.add_dependency(&view, &texture);
                self.targets.push(TextureTarget {
                    id: info.target,
                    size,
                    painter: Painter::new(&device, &queue, size),
                    view,
                    frame,
                    painted: None,
                });
                // A target that holds nothing is drawn from a page that is yet to
                // be rasterized into it, so the frame that created it paints.
                repaint = true;
                self.targets.len() - 1
            }
        };
        let target = &mut self.targets[index];
        target.frame = frame;

        // A page repaints when the frame that built it changed, and otherwise
        // only while it animates: Blitz reports no damage once layout settled, so
        // a static page is rasterized exactly once per target that shows it.
        //
        // Resolving is also what drains the resources the last layout asked for,
        // so it runs even when the page is still waiting on one: skipping it
        // would leave that request unanswered forever, and the page would never
        // paint again. The pixels are only rasterized once nothing it blocks on
        // is outstanding.
        if repaint || document.is_animating() {
            document.resolve(time);
            if !document.has_pending_critical_resources() {
                // The painter keeps the commands of the frame it last recorded,
                // so a page that replaces its document paints the successor on
                // top of it unless the commands are dropped first. The texture
                // is cleared by the pass Vello opens for it, so a page that
                // covers less ground than its predecessor leaves nothing of it.
                target.painter.clear();
                let paint_scale = document.viewport().scale_f64();
                blitz_paint::paint_scene(
                    &mut target.painter,
                    document,
                    paint_scale,
                    size.0,
                    size.1,
                    0,
                    0,
                );
                // The commands are recorded into the frame's own encoder, so the
                // page is drawn along with everything else the frame draws.
                let view = graph
                    .get()
                    .get(&target.view)
                    .map(|view| view.view().clone());
                if let Some(view) = view {
                    match target.painter.render(encoder, &view) {
                        Ok(()) => target.painted = Some(generation),
                        // A page that did not rasterize is not composed: the
                        // texture holds as much of the page as the failed pass
                        // drew, which is not the page.
                        Err(error) => {
                            log::error!("a Blitz page failed to rasterize: {error}");
                            target.painted = None;
                        }
                    }
                }
            }
        }
        let view = target.view.clone();
        let painted = target.painted;
        drop(graph);
        if painted != Some(generation) {
            return;
        }

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
}

/// The page this scope carries.
///
/// A scope that carries more than one page draws the one whose handle sorts
/// first, so which page a source paints is settled by the world rather than by
/// the order a query happened to walk. An entity's handle is its row index and
/// the generation it was allocated at, so a page spawned after another one was
/// despawned still sorts after the one that stayed.
fn first_page(scope: &Scope<'_>) -> Option<Entity> {
    scope
        .query::<(&BlitzDocument, Entity)>()
        .iter()
        .min_by_key(|(_document, entity)| entity.to_bits())
        .map(|(_document, entity)| entity)
}
