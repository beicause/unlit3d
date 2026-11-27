//! The fonts a page is laid out with.
//!
//! A page draws from the fonts the crate carries rather than the host's, so the
//! same markup paints the same glyphs wherever it runs, which is what lets one
//! snapshot hold across machines. The bytes are the
//! [`fonts`](crate::fonts) module's; enabling `blitz_system_fonts` adds the
//! host's fonts behind them for whatever they do not cover, at the cost of that
//! guarantee.
//!
//! The collection is built here rather than by
//! [`build_single_font_ctx`](blitz_dom::build_single_font_ctx) because a page
//! carries more than one font: the default font covers the Latin text, the
//! bundled Noto Sans SC subset covers the Chinese characters, and the second
//! only reaches a glyph run through the fallback list the shaper consults. The
//! result is built once for the process, because reading a font file and
//! enumerating the host's fonts are work no second page needs to repeat. What
//! each page gets is a copy of that baseline with a font store of its own: a
//! page registers the faces its `@font-face` rules name, and a family one page
//! names must not be matched by another page built before or after it.

#[cfg(feature = "fonts")]
use std::sync::{Arc, OnceLock};

#[cfg(feature = "fonts")]
use blitz_dom::decode_font_bytes;
#[cfg(feature = "fonts")]
use parley::FontContext;
#[cfg(feature = "fonts")]
use parley::fontique::{Blob, Collection, CollectionOptions, FamilyId, GenericFamily, SourceCache};
#[cfg(feature = "sc_fonts")]
use parley::fontique::{FallbackKey, Script};

/// No font is carried, so the collection is left to `blitz-dom`: it registers
/// nothing beyond the font it draws list markers with, and a page paints what
/// the host supplies, or no glyphs at all where the host supplies nothing.
#[cfg(not(feature = "fonts"))]
pub(crate) fn fonts() -> Option<blitz_dom::FontContext> {
    None
}

/// The fonts a page is laid out with: the bundled baseline with a store of its
/// own.
///
/// The baseline is unshared, so the copy each page takes is where the page's own
/// faces go: a `@font-face` rule registers into the collection of the document
/// that loaded it, and a second page sees the families every page starts with
/// and nothing else. Sharing one collection across pages instead would make a
/// family one page names resolvable by another, which would leave the glyphs a
/// page paints depending on which pages were built before it.
///
/// The store is created per copy rather than shared, which costs one walk of the
/// baseline's families; the font bytes behind them, the host's font list and the
/// decoded source cache are the baseline's and are shared.
#[cfg(feature = "fonts")]
pub(crate) fn fonts() -> Option<blitz_dom::FontContext> {
    static FONTS: OnceLock<FontContext> = OnceLock::new();
    let mut fonts = FONTS.get_or_init(build).clone();
    fonts.collection.make_shared();
    Some(fonts)
}

#[cfg(feature = "fonts")]
fn build() -> FontContext {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    // `blitz-dom` registers the list-marker font only in a collection it builds
    // itself, and the family that font is named after is what a list item falls
    // back through, so the page's own collection carries it too.
    register(&mut collection, blitz_dom::BULLET_FONT);
    let default = register(&mut collection, crate::fonts::latin());
    #[cfg(feature = "sc_fonts")]
    let sc = register(&mut collection, crate::fonts::chinese());
    for generic in [
        GenericFamily::SansSerif,
        GenericFamily::Serif,
        GenericFamily::Monospace,
        GenericFamily::SystemUi,
    ] {
        // The default font comes first in every family, so a page that sets no
        // font of its own paints the glyphs it always painted.
        collection.append_generic_families(generic, default.iter().copied());
        #[cfg(feature = "sc_fonts")]
        collection.append_generic_families(generic, sc.iter().copied());
    }
    // A glyph run consults the fallback list after the families the markup
    // names, and the shaper asks for the Han script's list whatever the text's
    // own script is, so one entry covers the Chinese in a page that declares no
    // font family at all.
    #[cfg(feature = "sc_fonts")]
    collection.append_fallbacks(
        FallbackKey::new(Script::from_bytes(*b"Hani"), None),
        sc.iter().copied(),
    );
    #[cfg(feature = "blitz_system_fonts")]
    collection.load_system_fonts();
    FontContext {
        collection,
        source_cache: SourceCache::new_shared(),
    }
}

/// The families a font file registers, in the order the file names them.
#[cfg(feature = "fonts")]
fn register(collection: &mut Collection, bytes: &[u8]) -> Vec<FamilyId> {
    let decoded = decode_font_bytes(bytes).into_owned();
    collection
        .register_fonts(Blob::new(Arc::new(decoded) as _), None)
        .into_iter()
        .map(|(family, _)| family)
        .collect()
}

#[cfg(all(test, feature = "fonts"))]
mod tests {
    use super::*;
    use parley::fontique::FontInfoOverride;

    /// Register a face under a family name of its own, which is what a
    /// `@font-face` rule does for the document that loaded it.
    fn register_under(fonts: &mut FontContext, family: &str, bytes: &[u8]) {
        let decoded = decode_font_bytes(bytes).into_owned();
        fonts.collection.register_fonts(
            Blob::new(Arc::new(decoded) as _),
            Some(FontInfoOverride {
                family_name: Some(family),
                ..Default::default()
            }),
        );
    }

    /// A family one page names is one only that page matches: a page built
    /// before the face was loaded and one built after both lay out with the
    /// families every page starts with.
    #[test]
    fn a_family_one_page_registers_is_not_one_another_page_matches() {
        let mut before = fonts().expect("the bundled fonts build");
        let baseline = before.collection.family_names().count();
        let mut loader = fonts().expect("the bundled fonts build");
        register_under(&mut loader, "Only This Page", crate::fonts::latin());
        let mut after = fonts().expect("the bundled fonts build");

        assert!(
            loader.collection.family_by_name("Only This Page").is_some(),
            "the page that registered the face should match its family"
        );
        assert!(
            before.collection.family_by_name("Only This Page").is_none(),
            "a page built before the face was loaded should not match its family"
        );
        assert!(
            after.collection.family_by_name("Only This Page").is_none(),
            "a page built after the face was loaded should not match its family"
        );
        assert_eq!(
            before.collection.family_names().count(),
            baseline,
            "the baseline keeps the families every page starts with"
        );
        assert_eq!(
            after.collection.family_names().count(),
            baseline,
            "a later page starts from the same baseline"
        );
    }

    /// Two pages naming one family each match the face they registered, and
    /// neither inherits the other's: the faces a page adds are held by the
    /// collection of that page.
    #[test]
    fn two_pages_naming_one_family_each_keep_their_own_face() {
        let mut page_a = fonts().expect("the bundled fonts build");
        let mut page_b = fonts().expect("the bundled fonts build");
        register_under(&mut page_a, "Shared Family", crate::fonts::latin());
        register_under(
            &mut page_b,
            "Shared Family",
            epaint_default_fonts::UBUNTU_LIGHT,
        );

        let faces = |page: &mut FontContext| {
            page.collection
                .family_by_name("Shared Family")
                .map_or(0, |family| family.fonts().len())
        };
        assert_eq!(faces(&mut page_a), 1, "a page holds the one face it added");
        assert_eq!(faces(&mut page_b), 1, "a page holds the one face it added");
        assert!(
            fonts()
                .expect("the bundled fonts build")
                .collection
                .family_by_name("Shared Family")
                .is_none(),
            "a page built afterwards starts without either face"
        );
    }
}
