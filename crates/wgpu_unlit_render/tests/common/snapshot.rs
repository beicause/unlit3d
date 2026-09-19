//! Snapshot testing without `insta`: reference images live as plain WebP
//! files under `tests/snapshots/`, and comparisons use the **fast-ssim2**
//! implementation of the SSIMULACRA2 perceptual metric instead of
//! byte-exact diffs.
//!
//! Why: byte-exact snapshots break on any driver/llvmpipe float scheduling
//! difference, while perceptual scoring still catches real rendering
//! regressions (wrong colors, missing draws, broken depth).
//!
//! Workflow:
//! - first run (or `SNAPSHOT_UPDATE=1`): stores `tests/snapshots/<name>.webp`
//!   and passes;
//! - otherwise: decodes the reference, scores it against the current frame,
//!   and asserts `score >= min_score` (SSIMULACRA2: 0 = terrible, ~100 =
//!   near-identical; identical inputs score ~100).

use fast_ssim2::{LinearRgbImage, ToLinearRgb, compute_ssimulacra2, srgb_u8_to_linear};
use image::ImageEncoder;

/// Default acceptance threshold for [`assert_image_snapshot`].
pub const DEFAULT_MIN_SCORE: f64 = 85.0;

/// Directory (relative to the crate root) holding the WebP snapshot files.
const SNAPSHOT_DIR: &str = "tests/snapshots";

/// RGBA8 frame view implementing fast-ssim2's [`ToLinearRgb`] (alpha dropped,
/// sRGB 8-bit → linear f32 via the crate's lookup table).
struct RgbaFrame<'a> {
    pixels: &'a [u8],
    width: usize,
    height: usize,
}

impl ToLinearRgb for RgbaFrame<'_> {
    fn to_linear_rgb(&self) -> LinearRgbImage {
        let data: Vec<[f32; 3]> = self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| {
                [
                    srgb_u8_to_linear(px[0]),
                    srgb_u8_to_linear(px[1]),
                    srgb_u8_to_linear(px[2]),
                ]
            })
            .collect();
        LinearRgbImage::new(data, self.width, self.height)
    }
}

/// Encode an RGBA8 frame as lossless WebP bytes.
pub fn encode_frame_webp(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .expect("webp encode failed");
    webp
}

/// Snapshot assertion with the default score threshold
/// ([`DEFAULT_MIN_SCORE`]).
pub fn assert_image_snapshot(name: &str, rgba: &[u8], width: u32, height: u32) {
    assert_image_snapshot_with_threshold(name, rgba, width, height, DEFAULT_MIN_SCORE);
}

/// Snapshot assertion with an explicit SSIMULACRA2 score threshold.
///
/// - `name`: full snapshot file name, e.g. `"terrain_spot.webp"`, resolved
///   under `tests/snapshots/`.
/// - `rgba`: the current frame (RGBA8, row-major, no padding).
/// - `min_score`: minimum acceptable perceptual score (see module docs).
///
/// Stores the reference when it is missing or when `SNAPSHOT_UPDATE` is set;
/// otherwise compares and asserts.
pub fn assert_image_snapshot_with_threshold(
    name: &str,
    rgba: &[u8],
    width: u32,
    height: u32,
    min_score: f64,
) {
    assert_eq!(
        rgba.len(),
        (width as usize) * (height as usize) * 4,
        "frame size mismatch"
    );
    let path = std::path::Path::new(SNAPSHOT_DIR).join(name); // full file name
    let update = std::env::var_os("SNAPSHOT_UPDATE").is_some();

    if !path.exists() || update {
        std::fs::create_dir_all(SNAPSHOT_DIR).expect("create snapshots dir");
        std::fs::write(&path, encode_frame_webp(rgba, width, height))
            .unwrap_or_else(|e| panic!("write snapshot {name}: {e}"));
        eprintln!(
            "snapshot `{name}` {}",
            if update { "updated" } else { "stored" }
        );
        return;
    }

    let score = score_against_reference(&path, name, rgba, width, height);
    assert!(
        score >= min_score,
        "snapshot `{name}` perceptual mismatch: SSIMULACRA2 score {score:.2} < {min_score}\n\
         if the change is intentional, re-store with SNAPSHOT_UPDATE=1"
    );
}

/// Decode the stored reference image and return its SSIMULACRA2 score
/// against `rgba` (dimensions must match).
fn score_against_reference(
    path: &std::path::Path,
    name: &str,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> f64 {
    let reference_bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("read snapshot {name}: {e}"));
    let reference = image::load_from_memory_with_format(&reference_bytes, image::ImageFormat::WebP)
        .unwrap_or_else(|e| panic!("decode snapshot {name}: {e}"))
        .to_rgba8();
    assert_eq!(
        (reference.width(), reference.height()),
        (width, height),
        "snapshot `{name}` dimensions changed"
    );

    let reference_frame = RgbaFrame {
        pixels: reference.as_raw(),
        width: width as usize,
        height: height as usize,
    };
    let current_frame = RgbaFrame {
        pixels: rgba,
        width: width as usize,
        height: height as usize,
    };
    compute_ssimulacra2(reference_frame, current_frame)
        .unwrap_or_else(|e| panic!("ssimulacra2 failed for {name}: {e}"))
}
