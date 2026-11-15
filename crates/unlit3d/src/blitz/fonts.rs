//! The fonts a page is laid out with.
//!
//! A page draws from the fonts the crate carries rather than the host's, so the
//! same markup paints the same glyphs wherever it runs, which is what lets one
//! snapshot hold across machines. Enabling `blitz_system_fonts` adds the host's
//! fonts behind them for whatever they do not cover, at the cost of that
//! guarantee.
//!
//! The collection is built here rather than by
//! [`build_single_font_ctx`](blitz_dom::build_single_font_ctx) because a page
//! carries more than one font: the default font covers the Latin text, the
//! bundled Noto Sans SC subset covers the Chinese characters, and the second
//! only reaches a glyph run through the fallback list the shaper consults. The
//! result is cached for the process, because reading a font file and
//! enumerating the host's fonts are work no second page needs to repeat.

#[cfg(feature = "blitz_epaint_fonts")]
use std::sync::{Arc, OnceLock};

#[cfg(feature = "blitz_epaint_fonts")]
use blitz_dom::decode_font_bytes;
#[cfg(feature = "blitz_epaint_fonts")]
use parley::FontContext;
#[cfg(feature = "blitz_epaint_fonts")]
use parley::fontique::{Blob, Collection, CollectionOptions, FamilyId, GenericFamily, SourceCache};
#[cfg(feature = "blitz_sc_fonts")]
use parley::fontique::{FallbackKey, Script};

/// No font is carried, so the collection is left to `blitz-dom`: it registers
/// nothing beyond the font it draws list markers with, and a page paints what
/// the host supplies, or no glyphs at all where the host supplies nothing.
#[cfg(not(feature = "blitz_epaint_fonts"))]
pub(crate) fn fonts() -> Option<blitz_dom::FontContext> {
    None
}

/// The fonts every page is laid out with, built once per process.
///
/// The clone handed to a document shares the collection, which is what a
/// collection built unshared has to be made into before it leaves here.
#[cfg(feature = "blitz_epaint_fonts")]
pub(crate) fn fonts() -> Option<blitz_dom::FontContext> {
    static FONTS: OnceLock<FontContext> = OnceLock::new();
    Some(FONTS.get_or_init(build).clone())
}

#[cfg(feature = "blitz_epaint_fonts")]
fn build() -> FontContext {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    // `blitz-dom` registers the list-marker font only in a collection it builds
    // itself, and the family that font is named after is what a list item falls
    // back through, so the page's own collection carries it too.
    register(&mut collection, blitz_dom::BULLET_FONT);
    let default = register(&mut collection, epaint_default_fonts::HACK_REGULAR);
    #[cfg(feature = "blitz_sc_fonts")]
    let sc = register(&mut collection, sc_font());
    for generic in [
        GenericFamily::SansSerif,
        GenericFamily::Serif,
        GenericFamily::Monospace,
        GenericFamily::SystemUi,
    ] {
        // The default font comes first in every family, so a page that sets no
        // font of its own paints the glyphs it always painted.
        collection.append_generic_families(generic, default.iter().copied());
        #[cfg(feature = "blitz_sc_fonts")]
        collection.append_generic_families(generic, sc.iter().copied());
    }
    // A glyph run consults the fallback list after the families the markup
    // names, and the shaper asks for the Han script's list whatever the text's
    // own script is, so one entry covers the Chinese in a page that declares no
    // font family at all.
    #[cfg(feature = "blitz_sc_fonts")]
    collection.append_fallbacks(
        FallbackKey::new(Script::from_bytes(*b"Hani"), None),
        sc.iter().copied(),
    );
    #[cfg(feature = "blitz_system_fonts")]
    collection.load_system_fonts();
    collection.make_shared();
    FontContext {
        collection,
        source_cache: SourceCache::new_shared(),
    }
}

/// The families a font file registers, in the order the file names them.
#[cfg(feature = "blitz_epaint_fonts")]
fn register(collection: &mut Collection, bytes: &[u8]) -> Vec<FamilyId> {
    let decoded = decode_font_bytes(bytes).into_owned();
    collection
        .register_fonts(Blob::new(Arc::new(decoded) as _), None)
        .into_iter()
        .map(|(family, _)| family)
        .collect()
}

/// The bundled Chinese subset.
///
/// Read behind a function rather than held in a constant so the bytes are not
/// copied into the crate's metadata, which every dependent would then rebuild.
#[cfg(feature = "blitz_sc_fonts")]
#[inline(never)]
fn sc_font() -> &'static [u8] {
    include_bytes!("../../assets/NotoSansSC-Regular-subset.woff2")
}
