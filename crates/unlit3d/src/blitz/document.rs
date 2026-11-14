//! The page a [`BlitzSource`](crate::blitz::BlitzSource) draws, and the bytes it loads.
//!
//! A page is a component rather than a field of the source. The source is only
//! its driver: it draws a document its own subtree carries, so the markup lives
//! in the world like any other behaviour, and the code that owns the entity can
//! replace it or serve what it loads.
//!
//! # Bytes
//!
//! Nothing reaches the network. A page loads what its markup carries inline, a
//! `data:` URL decoded on the spot, and whatever
//! [`BlitzDocument::insert_resource`] serves for the path it asks for.
//!
//! Every request is answered, including one nothing serves: a stylesheet the
//! page blocks on would otherwise stay pending, and a page waiting on it never
//! paints. An unserved path therefore arrives as empty bytes, which a stylesheet
//! reads as an empty sheet and an image as a load failure that leaves a hole.
//! The cost of the choice is that the page caches the answer, so bytes
//! registered afterwards are not loaded until the markup is set again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use blitz_dom::{BaseDocument, DocumentConfig, build_single_font_ctx};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request, Url};
use blitz_traits::shell::{ShellProvider, Viewport};
use data_url::DataUrl;
use hashbrown::HashMap;
use unlit_ecs::sync;

/// The base a page's relative references resolve against.
///
/// A synthetic origin: nothing is fetched, and a path is only a name the
/// embedded bytes are keyed by. The `invalid` top-level domain is reserved, so
/// the address can never reach a real host even if something tried.
const DEFAULT_BASE: &str = "https://page.invalid/";

/// A page: its markup, the document laid out from it, and the bytes it loads.
///
/// Spawn a page inside the subtree of the [`BlitzSource`](crate::blitz::BlitzSource)
/// that draws it — the source draws the first page of its own subtree, so a page
/// it cannot see is never drawn, and one page is drawn by one source.
///
/// The source lays it out at the size of the pass it is drawn in, and lays it
/// out again only when the markup, the base, the size or the scale changes, or
/// while the page animates.
///
/// # Inspecting and changing the page
///
/// The document itself is reached from an [`OnBlitzEvent`](crate::blitz::OnBlitzEvent)
/// callback, which is handed the page the event was dispatched into: that is
/// where a click reads a form field or changes the DOM. Outside a callback only
/// the markup is available, so a page that has to react to something other than
/// its own events is redrawn by setting its markup again.
pub struct BlitzDocument {
    /// The markup the document is laid out from.
    markup: String,
    /// The viewport the document was last laid out at.
    viewport: Option<Viewport>,
    /// The laid-out document, once a source has asked for it.
    document: Option<BaseDocument>,
    /// The bytes served for the paths the page loads, and the base they resolve
    /// against.
    resources: Arc<BlitzResources>,
    /// What the page asks of the frame it is drawn in.
    pub(crate) shell: Arc<BlitzShell>,
    /// Which page this is, counted up whenever the document is replaced.
    ///
    /// A target that was drawn from an older page is not composed, so replacing
    /// the markup never shows the page before it.
    pub(crate) generation: u64,
    /// The tick whose input this page has already taken.
    fed: Option<u64>,
}

impl BlitzDocument {
    /// Lay `markup` out as a page.
    ///
    /// The markup is parsed on the first frame that draws the page, not here,
    /// so a component is cheap to spawn and a page that is never drawn costs
    /// nothing.
    pub fn new(markup: impl Into<String>) -> Self {
        Self {
            markup: markup.into(),
            viewport: None,
            document: None,
            resources: Arc::default(),
            shell: Arc::default(),
            generation: 0,
            fed: None,
        }
    }

    /// Replace the markup and lay the page out again from scratch.
    ///
    /// Everything the page held -- its focus, its form state, its scroll
    /// position -- goes with the old document.
    pub fn set_markup(&mut self, markup: impl Into<String>) {
        self.markup = markup.into();
        self.document = None;
        self.generation += 1;
        self.shell.request_redraw();
    }

    /// Set the base a relative reference in the markup resolves against.
    ///
    /// A path served through [`Self::insert_resource`] is resolved against the
    /// base in force when the page loads it, not when it was registered, so
    /// changing the base serves the same bytes at the addresses the new base
    /// gives them.
    ///
    /// A value that is not an absolute URL is logged and ignored, and the base
    /// stays what it was: a page whose references cannot resolve would panic
    /// when it loads them.
    pub fn set_base_url(&mut self, base_url: impl AsRef<str>) {
        match Url::parse(base_url.as_ref()) {
            Ok(url) => {
                *sync::write(&self.resources.base) = url;
                self.document = None;
                self.generation += 1;
                self.shell.request_redraw();
            }
            Err(error) => log::warn!("a page base URL that is not absolute: {error}"),
        }
    }

    /// Serve `bytes` for `path`.
    ///
    /// `path` is resolved against the page's base URL, so both the page's own
    /// spelling (`logo.png`) and the absolute URL it resolves to select the
    /// same bytes. A `data:` URL needs no entry: the page decodes it itself.
    ///
    /// A page loads a path once, when its markup is laid out. Bytes registered
    /// afterwards are not fetched until the markup is set again, which is also
    /// what drops a failed load from the page's cache.
    pub fn insert_resource(&self, path: impl AsRef<str>, bytes: impl AsRef<[u8]>) {
        let path = path.as_ref();
        let bytes = Bytes::copy_from_slice(bytes.as_ref());
        sync::write(&self.resources.files).insert(path.to_owned(), bytes);
    }

    /// Whether this page still has to take input from the tick `tick`.
    ///
    /// One page is built by every source and pass that see it, and each of them
    /// reads the same batch. The page takes a tick's events once, so a click is
    /// dispatched once however many times the frame draws what it changed.
    pub(crate) fn feeds_input(&mut self, tick: u64) -> bool {
        if self.fed == Some(tick) {
            return false;
        }
        self.fed = Some(tick);
        true
    }

    /// The laid-out document, at `viewport`, building or re-laying it out when
    /// either the markup or the viewport changed.
    pub(crate) fn document(&mut self, viewport: Viewport) -> &mut BaseDocument {
        let base = sync::read(&self.resources.base).clone();
        if self.document.is_none() {
            let config = DocumentConfig {
                viewport: Some(viewport.clone()),
                base_url: Some(base.to_string()),
                net_provider: Some(Arc::clone(&self.resources) as Arc<dyn NetProvider>),
                shell_provider: Some(Arc::clone(&self.shell) as Arc<dyn ShellProvider>),
                font_ctx: Some(build_single_font_ctx(epaint_default_fonts::HACK_REGULAR)),
                ..Default::default()
            };
            let document = HtmlDocument::from_html(&self.markup, config).into_inner();
            self.viewport = Some(viewport.clone());
            // Laying a page out is the one change nothing else announces: the
            // page did not exist a moment ago, so the next frame has to draw it.
            self.shell.request_redraw();
            self.document = Some(document);
        }
        let document = self
            .document
            .as_mut()
            .expect("the document was built just above");
        if self.viewport.as_ref() != Some(&viewport) {
            document.set_viewport(viewport.clone());
            self.viewport = Some(viewport);
        }
        document
    }
}

/// The bytes a page loads, and the base its paths resolve against.
///
/// A page asks for whatever a `src` or a stylesheet `href` names, already
/// resolved against the page's base URL. This keeps the paths as the caller
/// spelled them and resolves them against the base at load time, which is what
/// makes a base change move the served bytes with it; a `data:` URL is decoded
/// here, and anything else is answered with nothing.
pub(crate) struct BlitzResources {
    /// The bytes served for a path, keyed by the path as it was registered.
    files: RwLock<HashMap<String, Bytes>>,
    /// The base a path resolves against.
    base: RwLock<Url>,
}

impl Default for BlitzResources {
    fn default() -> Self {
        Self {
            files: RwLock::default(),
            base: RwLock::new(
                DEFAULT_BASE
                    .parse()
                    .expect("the default page base URL is valid"),
            ),
        }
    }
}

impl NetProvider for BlitzResources {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.to_string();
        let base = sync::read(&self.base).clone();
        let served = sync::read(&self.files)
            .iter()
            .find(|(path, _)| base.join(path).is_ok_and(|served| served == request.url))
            .map(|(_, bytes)| bytes.clone());
        if let Some(bytes) = served {
            handler.bytes(url, bytes);
            return;
        }
        if request.url.scheme() == "data"
            && let Ok(data) = DataUrl::process(&url)
            && let Ok((bytes, _fragment)) = data.decode_to_vec()
        {
            handler.bytes(url, Bytes::from(bytes));
            return;
        }
        // Nothing served it, but the request still has to be answered: a
        // stylesheet the page blocks on stays pending until it is, and the page
        // never paints while one is.
        handler.bytes(url, Bytes::new());
    }
}

/// What a page asks of the shell it is laid out in.
///
/// A page has one thing to ask of a frame source: another frame. The document
/// reports that it scrolled, that a resource landed, or that a callback changed
/// it, and the source paints again the next time it is built. The rest of a
/// shell -- the cursor, the window title, the clipboard, the input method --
/// has no counterpart here, so the defaults answer them.
#[derive(Default)]
pub(crate) struct BlitzShell {
    redraw: AtomicBool,
}

impl BlitzShell {
    /// Whether the page asked to be drawn again since this was last called.
    pub(crate) fn take_redraw(&self) -> bool {
        self.redraw.swap(false, Ordering::Relaxed)
    }
}

impl ShellProvider for BlitzShell {
    fn request_redraw(&self) {
        self.redraw.store(true, Ordering::Relaxed);
    }
}
