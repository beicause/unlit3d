//! The fonts the crate carries.
//!
//! A build that carries its own fonts paints the same glyphs wherever it runs,
//! which is what lets one snapshot hold across machines. The bytes are owned
//! here rather than by a drawing backend, so a page laid out by
//! [`blitz`](crate::blitz) and an overlay laid out by [`egui`](crate::egui)
//! draw from the same fonts.
//!
//! [`latin`] is the default font, which covers the Latin text. [`chinese`] is a
//! Noto Sans SC subset covering the 3500 level-1 and 3000 level-2 characters of
//! the Table of General Standard Chinese Characters, and sits behind `sc_fonts`
//! because it costs 977 kB of the binary.
//!
//! The two backends differ in the format they read: `blitz-dom` decompresses
//! WOFF2 itself, while epaint takes only TTF and OTF. The subset is therefore
//! stored as WOFF2 — the smallest form — and [`chinese_ttf`] inflates it once
//! for epaint.

#[cfg(feature = "sc_fonts")]
use std::sync::OnceLock;

/// The default font, which carries the Latin text.
///
/// Read behind a function rather than held in a constant so the bytes are not
/// copied into the crate's metadata, which every dependent would then rebuild.
#[inline(never)]
pub fn latin() -> &'static [u8] {
    epaint_default_fonts::HACK_REGULAR
}

/// The bundled Chinese subset, in WOFF2.
///
/// Read behind a function rather than held in a constant so the bytes are not
/// copied into the crate's metadata, which every dependent would then rebuild.
#[cfg(feature = "sc_fonts")]
#[inline(never)]
pub fn chinese() -> &'static [u8] {
    include_bytes!("../assets/NotoSansSC-Regular-subset.woff2")
}

/// The bundled Chinese subset, inflated to TTF.
///
/// The bundled asset always inflates, so a failure here is a broken build
/// rather than a condition a caller can act on.
#[cfg(feature = "sc_fonts")]
pub fn chinese_ttf() -> &'static [u8] {
    static TTF: OnceLock<Vec<u8>> = OnceLock::new();
    TTF.get_or_init(|| {
        wuff::decompress_woff2(chinese()).expect("the bundled Noto Sans SC subset decompresses")
    })
}

/// The family name the Chinese subset is registered under.
#[cfg(feature = "sc_fonts")]
pub const CHINESE_FAMILY: &str = "Noto Sans SC";

/// Add the bundled Chinese subset to an egui font configuration.
///
/// The subset is appended behind the fonts already in the family, so Latin text
/// keeps the glyphs it paints with and Chinese falls through to the subset. The
/// Latin font is not added: it only restates what egui already carries.
#[cfg(all(feature = "egui", feature = "sc_fonts"))]
pub fn egui_fonts(fonts: &mut egui::FontDefinitions) {
    use egui::FontFamily;

    fonts.font_data.insert(
        CHINESE_FAMILY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(chinese_ttf())),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(CHINESE_FAMILY.to_owned());
    }
}

/// Lay an egui overlay out with the fonts the crate carries.
///
/// Without `sc_fonts` this leaves the context on egui's own fonts, which carry
/// the Latin and Cyrillic alphabets and nothing else.
#[cfg(feature = "egui")]
pub fn set_egui_fonts(context: &egui::Context) {
    #[cfg(feature = "sc_fonts")]
    {
        let mut fonts = egui::FontDefinitions::default();
        egui_fonts(&mut fonts);
        context.set_fonts(fonts);
    }
    #[cfg(not(feature = "sc_fonts"))]
    let _ = context;
}

#[cfg(all(test, feature = "egui", feature = "sc_fonts"))]
mod tests {
    use super::*;

    #[test]
    fn the_chinese_subset_inflates_to_a_ttf() {
        let ttf = chinese_ttf();
        assert!(ttf.len() > chinese().len(), "a TTF is the inflated form");
        assert_eq!(&ttf[..4], [0x00, 0x01, 0x00, 0x00], "a TrueType header");
        assert!(
            std::ptr::eq(ttf, chinese_ttf()),
            "the subset is inflated once rather than per call"
        );
    }

    #[test]
    fn an_egui_overlay_lays_a_chinese_character_out() {
        let context = egui::Context::default();
        set_egui_fonts(&context);
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            ui.label("汉字");
        });
        // Nothing draws the overlay here, so the glyph atlas it built is dropped
        // rather than uploaded.
        output.textures_delta.clear();
        let font = egui::FontId::proportional(14.0);
        assert!(
            context.fonts_mut(|fonts| fonts.has_glyph(&font, '汉')),
            "the overlay has no glyph for a Chinese character"
        );
    }
}
