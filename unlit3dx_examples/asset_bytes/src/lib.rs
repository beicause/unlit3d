#![doc = include_str!("../README.md")]

/// The soundfont the demo plays, embedded at build time by `build.rs`.
///
/// When the soundfont file is missing the build writes an empty stub, so the
/// demo starts and runs silent rather than failing to compile.
///
/// `#[inline(never)]` keeps the fourteen megabytes out of this crate's
/// metadata: an inlinable body carries the bytes it embeds along with it.
#[inline(never)]
pub fn soundfont() -> &'static [u8] {
    include_bytes!(env!("SOUNDFONT_FILE"))
}
