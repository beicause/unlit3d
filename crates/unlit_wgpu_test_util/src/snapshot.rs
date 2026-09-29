//! Perceptual snapshot assertions.
//!
//! A snapshot is named, and where its bytes come from depends on where the
//! test runs. Natively they are read from the asset submodule beside the test
//! file, so a mismatch can be re-stored in place with `SNAPSHOT_UPDATE=1`. In a
//! browser there is no filesystem to read or write, so
//! [`snapshot!`](crate::snapshot) embeds the baseline into the wasm binary at
//! the call site instead — and the comparison itself, a real SSIMULACRA2 score,
//! runs there unchanged.
//!
//! Both paths decode the same WebP and score it the same way; only the origin
//! of the bytes differs.

use image::ImageEncoder;
use ssimulacra2::{ColorPrimaries, Rgb, TransferCharacteristic, compute_frame_ssimulacra2};

/// Where [`Snapshot::on_disk`] looks a snapshot up by name, relative to the
/// process's working directory — which for a test is its package root.
#[cfg(not(target_arch = "wasm32"))]
const SNAPSHOT_DIR: &str = "tests/snapshots";

/// The perceptual score a frame must reach to match its snapshot.
pub const DEFAULT_MIN_SCORE: f64 = 85.0;

/// How closely a frame must match its snapshot.
///
/// Every gate this names must pass. A gate left as `None` is one whose verdict
/// would be meaningless for this snapshot's content, and leaving it out says so
/// rather than naming a threshold that either never passes or never fails.
///
/// The two gates catch different mistakes. The perceptual score notices a
/// change spread thinly over the whole frame, which moves every pixel a little.
/// The outlier allowance notices a change confined to a few pixels, which moves
/// those a lot — the kind a second implementation of the same pipeline produces
/// where it resolves an edge or an occlusion differently, and the kind an
/// average over the frame barely registers.
#[derive(Clone, Copy)]
pub struct Tolerance {
    /// The perceptual score the frame must reach, on SSIMULACRA2's 0–100 scale,
    /// or `None` to judge the frame only by its outlier pixels.
    pub min_score: Option<f64>,
    /// The fraction of pixels that may differ from the baseline by more than
    /// [`channel_delta`](Tolerance::channel_delta), or `None` to judge the frame
    /// only by its score.
    pub max_outliers: Option<f64>,
    /// The per-channel difference, in 0–255 units, within which a pixel counts
    /// as matching however far the frame's score falls.
    pub channel_delta: u8,
}

/// The tolerance a snapshot gets when it names none: the perceptual floor
/// [`DEFAULT_MIN_SCORE`], with no bound on how many pixels may stand out.
///
/// The outlier allowance is off by default because it is the blunter gate: a
/// frame can stay inside it while differing over exactly the few pixels that
/// matter. A snapshot that needs it has content the two gates disagree about —
/// hard edges that a second implementation may place differently — and should
/// carry the measurements that justify its bound, as the instanced-cube test
/// does.
pub const DEFAULT_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(DEFAULT_MIN_SCORE),
    max_outliers: None,
    channel_delta: 8,
};

/// A snapshot baseline, and where its bytes come from.
///
/// Built by [`snapshot!`](crate::snapshot), which is the only way to get one:
/// the macro is what turns a literal name into an embedded byte array on the
/// web, and it has to see the name as a literal to do it.
#[derive(Clone, Copy)]
pub struct Snapshot {
    /// The baseline's name, such as `unlit_cube.webp`.
    name: &'static str,
    /// The baseline's bytes where the target has no filesystem to read them
    /// from, and `None` where it does.
    embedded: Option<&'static [u8]>,
}

impl Snapshot {
    /// A snapshot to be read from the asset submodule at compare time.
    #[cfg(not(target_arch = "wasm32"))]
    pub const fn on_disk(name: &'static str) -> Self {
        Self {
            name,
            embedded: None,
        }
    }

    /// A snapshot whose bytes were embedded into the binary.
    #[cfg(target_arch = "wasm32")]
    pub const fn embedded(name: &'static str, bytes: &'static [u8]) -> Self {
        Self {
            name,
            embedded: Some(bytes),
        }
    }

    /// The baseline's name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The baseline's bytes, where they were embedded.
    pub fn embedded_bytes(&self) -> Option<&'static [u8]> {
        self.embedded
    }
}

/// Name a snapshot baseline, embedding its bytes where there is no filesystem.
///
/// Natively this is the name alone and the file is read when the frame is
/// compared, so `SNAPSHOT_UPDATE=1` can rewrite it. In a browser the bytes are
/// read out of the repository at compile time and travel inside the wasm
/// binary, resolved relative to the file the macro is invoked from.
///
/// The name must be a literal, because it is also the path `include_bytes!`
/// resolves.
///
/// # Example
///
/// ```
/// use unlit_wgpu_test_util::snapshot;
///
/// let baseline = snapshot!("unlit_cube.webp");
/// assert_eq!(baseline.name(), "unlit_cube.webp");
/// ```
#[macro_export]
macro_rules! snapshot {
    ($name:literal) => {{
        #[cfg(target_arch = "wasm32")]
        {
            $crate::Snapshot::embedded($name, include_bytes!(concat!("snapshots/", $name)))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            $crate::Snapshot::on_disk($name)
        }
    }};
}

/// The snapshots one scene's frames verify against, in frame order.
///
/// A scene that freezes a sequence of frames stores each as its own snapshot
/// under a directory named for the scene. Listing them here rather than
/// building the names at run time is what lets a web build carry them in the
/// binary: the wasm arm embeds every listed file, the way [`snapshot!`] embeds
/// one, and the host arm resolves them under the snapshot directory instead.
///
/// The list is the scene's own frame table, so it is written out in full
/// rather than derived — a frame whose snapshot is missing from the list is a
/// frame the test does not compare, and one named here that has no file fails
/// to compile on the web.
///
/// ```
/// use unlit_wgpu_test_util::snapshots;
///
/// // A scene storing two frames under its own directory.
/// let frames = snapshots![
///     "ecs_animated/frame_00_alloc3_free0_live3.webp",
///     "ecs_animated/frame_01_alloc2_free0_live5.webp",
/// ];
/// assert_eq!(frames.len(), 2);
/// assert_eq!(frames[0].name(), "ecs_animated/frame_00_alloc3_free0_live3.webp");
/// ```
#[macro_export]
macro_rules! snapshots {
    ($($name:literal),+ $(,)?) => {
        [$($crate::snapshot!($name)),+]
    };
}

/// What went wrong storing or comparing a snapshot.
///
/// The paths and scores a caller reports come from here rather than from a
/// panic, so a tool can turn a mismatch into its own exit code.
#[derive(Debug)]
pub enum SnapshotError {
    /// The frame's bytes do not describe `width` x `height` RGBA pixels.
    FrameSize {
        /// Bytes the frame holds.
        got: usize,
        /// Bytes the dimensions require.
        expected: usize,
    },
    /// The snapshot file could not be read or written.
    Io(std::io::Error),
    /// The snapshot could not be decoded as WebP.
    Decode(image::ImageError),
    /// The snapshot's dimensions differ from the frame's.
    Dimensions {
        /// The snapshot's dimensions.
        snapshot: (u32, u32),
        /// The frame's dimensions.
        frame: (u32, u32),
    },
    /// The two images could not be scored.
    Score(ssimulacra2::Ssimulacra2Error),
}

impl core::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FrameSize { got, expected } => {
                write!(f, "frame size mismatch: {got} bytes, expected {expected}")
            }
            Self::Io(error) => write!(f, "{error}"),
            Self::Decode(error) => write!(f, "decoding the snapshot failed: {error}"),
            Self::Dimensions { snapshot, frame } => write!(
                f,
                "the snapshot is {}x{} but the frame is {}x{}",
                snapshot.0, snapshot.1, frame.0, frame.1
            ),
            Self::Score(error) => write!(f, "scoring the frames failed: {error}"),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Score(error) => Some(error),
            Self::FrameSize { .. } | Self::Dimensions { .. } => None,
        }
    }
}

/// Turn an RGBA8 frame into the RGB the metric reads.
///
/// The metric works in XYB, which it derives from *linear* RGB, so the
/// values handed over are the sRGB ones the frame holds: the transfer
/// characteristic named here is what linearizes them, and naming it is also
/// what says the frame is sRGB rather than something else.
fn rgb_frame(rgba: &[u8], width: u32, height: u32) -> Rgb {
    let data: Vec<[f32; 3]> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| {
            [
                f32::from(px[0]) / 255.0,
                f32::from(px[1]) / 255.0,
                f32::from(px[2]) / 255.0,
            ]
        })
        .collect();
    Rgb::new(
        data,
        width as usize,
        height as usize,
        TransferCharacteristic::SRGB,
        ColorPrimaries::BT709,
    )
    .expect("the frame's dimensions and pixel count agree")
}

/// Reject a frame whose bytes do not describe `width` x `height` RGBA pixels.
fn check_frame_size(rgba: &[u8], width: u32, height: u32) -> Result<(), SnapshotError> {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() == expected {
        Ok(())
    } else {
        Err(SnapshotError::FrameSize {
            got: rgba.len(),
            expected,
        })
    }
}

/// What comparing a frame against its baseline found.
///
/// Both numbers are carried to the assertion rather than judged here, because
/// which of them decides the frame is the snapshot's [`Tolerance`] to say.
#[derive(Clone, Copy)]
struct Comparison {
    /// The perceptual score, on SSIMULACRA2's 0–100 scale.
    score: f64,
    /// The fraction of pixels differing from the baseline by more than the
    /// tolerance's `channel_delta`.
    outliers: f64,
}

/// Measure `rgba` against a WebP baseline held in memory.
///
/// This is the whole comparison, and the only difference between the two
/// targets is where `reference_bytes` was read from.
fn compare_frame_bytes(
    reference_bytes: &[u8],
    rgba: &[u8],
    width: u32,
    height: u32,
    channel_delta: u8,
) -> Result<Comparison, SnapshotError> {
    check_frame_size(rgba, width, height)?;
    let reference = image::load_from_memory_with_format(reference_bytes, image::ImageFormat::WebP)
        .map_err(SnapshotError::Decode)?
        .to_rgba8();
    if (reference.width(), reference.height()) != (width, height) {
        return Err(SnapshotError::Dimensions {
            snapshot: (reference.width(), reference.height()),
            frame: (width, height),
        });
    }

    let reference_frame = rgb_frame(reference.as_raw(), width, height);
    let current_frame = rgb_frame(rgba, width, height);
    let score =
        compute_frame_ssimulacra2(reference_frame, current_frame).map_err(SnapshotError::Score)?;
    Ok(Comparison {
        score,
        outliers: outlier_fraction(reference.as_raw(), rgba, channel_delta),
    })
}

/// The fraction of pixels whose largest per-channel difference from the
/// baseline exceeds `channel_delta`.
///
/// A pixel the two implementations resolve differently moves far, so it lands
/// outside `channel_delta`; one they agree on, or which differs only by the
/// rounding of an interpolated value, stays inside it. Counting them is what
/// separates a difference confined to a few edges — which the perceptual score
/// is too coarse to see past — from one spread over the frame.
fn outlier_fraction(reference: &[u8], rgba: &[u8], channel_delta: u8) -> f64 {
    let pixels = rgba.len() / 4;
    if pixels == 0 {
        return 0.0;
    }
    let outliers = reference
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgba.as_chunks::<4>().0)
        .filter(|(reference, frame)| {
            (0..3).any(|channel| reference[channel].abs_diff(frame[channel]) > channel_delta)
        })
        .count();
    outliers as f64 / pixels as f64
}

/// The environment variable that collects mismatched frames on the host.
///
/// A mismatch is often the platform showing through rather than a regression:
/// the stored frames come from one GPU stack, and another driver's rounding can
/// put a frame below the bar with nothing wrong. Writing the frame that did not
/// match is what lets CI upload it as an artifact, so the difference can be
/// looked at instead of guessed at from a score.
///
/// Unset, frames go under [`DEFAULT_MISMATCH_DIR`]. Set to a path, they go there
/// instead — which is how a task that makes several passes gives each one its
/// own directory.
#[cfg(not(target_arch = "wasm32"))]
pub const MISMATCH_DIR_ENV: &str = "UNLIT3D_SNAPSHOT_MISMATCH_DIR";

/// Where mismatched frames are written when [`MISMATCH_DIR_ENV`] is unset.
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_MISMATCH_DIR: &str = "target/snapshot-mismatches";

/// Assert that `rgba` matches `snapshot` within [`DEFAULT_TOLERANCE`].
///
/// Natively a missing snapshot is written from this frame rather than failed
/// on, so the first run of a new test records its baseline, and
/// `SNAPSHOT_UPDATE=1` rewrites one that exists. On the web the baseline is
/// embedded in the binary, so there is nothing to write and a missing one
/// could not have compiled.
pub fn assert_image_snapshot(snapshot: Snapshot, rgba: &[u8], width: u32, height: u32) {
    assert_image_snapshot_with_tolerance(snapshot, rgba, width, height, DEFAULT_TOLERANCE);
}

/// Assert that `rgba` matches `snapshot` within `tolerance`.
///
/// Like [`assert_image_snapshot`], a missing snapshot is written rather than
/// failed on — natively, where there is somewhere to write it. A freshly
/// written snapshot is not judged at all, since it was just made from this
/// frame.
///
/// A frame that does not match is kept before the assertion fails, so a run that
/// fails in CI leaves it behind for the job to upload: natively it is written
/// under [`DEFAULT_MISMATCH_DIR`] (or [`MISMATCH_DIR_ENV`]), and on the web it is
/// handed to the test runner, which is the process that has a filesystem.
/// Keeping it is a courtesy to whoever reads the result, so a failure to do so
/// is reported and dropped rather than replacing the mismatch it was meant to
/// explain.
pub fn assert_image_snapshot_with_tolerance(
    snapshot: Snapshot,
    rgba: &[u8],
    width: u32,
    height: u32,
    tolerance: Tolerance,
) {
    let name = snapshot.name();
    let comparison = compare(snapshot, rgba, width, height, tolerance.channel_delta)
        .unwrap_or_else(|error| panic!("snapshot {name}: {error}"));

    let failure = if let Some(min_score) = tolerance.min_score
        && comparison.score < min_score
    {
        Some(format!(
            "perceptual mismatch: SSIMULACRA2 score {:.2} < {min_score}",
            comparison.score,
        ))
    } else if let Some(max_outliers) = tolerance.max_outliers
        && comparison.outliers > max_outliers
    {
        Some(format!(
            "differs over {:.2}% of its pixels, above the {:.2}% allowed \
             beyond a channel difference of {}",
            comparison.outliers * 100.0,
            max_outliers * 100.0,
            tolerance.channel_delta,
        ))
    } else {
        None
    };

    if let Some(failure) = failure {
        store_mismatch(name, rgba, width, height);
        panic!(
            "snapshot `{name}` {failure}
             if the change is intentional, re-store with SNAPSHOT_UPDATE=1"
        );
    }
}

/// Keep a frame that did not match, so a failing run leaves it behind.
///
/// Natively, where a test owns its own filesystem: the snapshot's name is a path
/// relative to the snapshot directory, so it keeps each frame under the test it
/// belongs to rather than colliding on a file name.
#[cfg(not(target_arch = "wasm32"))]
fn store_mismatch(name: &str, rgba: &[u8], width: u32, height: u32) {
    let dir = std::env::var_os(MISMATCH_DIR_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_MISMATCH_DIR));
    let path = dir.join(name);
    if let Err(error) = store_frame_webp(&path, rgba, width, height) {
        // Reported, not raised: the mismatch is the result worth reporting, and
        // losing this frame must not replace it with a write error.
        log::warn!(
            "writing the mismatched frame to {}: {error}",
            path.display()
        );
    }
}

/// Keep a frame that did not match, by handing it to the runner.
///
/// A page has no filesystem, so the encoded frame is passed to the process that
/// has one — the Node test runner, which writes it under its own mismatch
/// directory. The encoding happens here because the frame's bytes only exist in
/// the browser.
#[cfg(target_arch = "wasm32")]
fn store_mismatch(name: &str, rgba: &[u8], width: u32, height: u32) {
    let webp = encode_frame_webp(rgba, width, height);
    crate::browser::record_mismatch(name, &webp);
}

/// Measure a frame against its baseline, storing one where none exists yet.
#[cfg(not(target_arch = "wasm32"))]
fn compare(
    snapshot: Snapshot,
    rgba: &[u8],
    width: u32,
    height: u32,
    channel_delta: u8,
) -> Result<Comparison, SnapshotError> {
    let path = snapshot_path(snapshot.name());
    let update = std::env::var_os("SNAPSHOT_UPDATE").is_some();

    if !path.exists() || update {
        // A snapshot name may carry subdirectories. `create_dir_all` is a
        // no-op for the directories that already exist, including the
        // `tests/snapshots` symlink into the asset repository.
        store_frame_webp(&path, rgba, width, height)?;
        log::info!(
            "snapshot `{}` {}",
            snapshot.name(),
            if update { "updated" } else { "stored" }
        );
        // A frame just stored from itself is by definition identical to its
        // baseline, so it is not scored — and `INFINITY` is what says so, since
        // no tolerance can exceed it.
        return Ok(Comparison {
            score: f64::INFINITY,
            outliers: 0.0,
        });
    }

    compare_frame_webp(&path, rgba, width, height, channel_delta)
}

/// Measure a frame against the baseline embedded in this binary.
///
/// A browser has nowhere to store a baseline, so an absent one is not a case
/// that can arise: `include_bytes!` would not have compiled.
#[cfg(target_arch = "wasm32")]
fn compare(
    snapshot: Snapshot,
    rgba: &[u8],
    width: u32,
    height: u32,
    channel_delta: u8,
) -> Result<Comparison, SnapshotError> {
    let bytes = snapshot
        .embedded_bytes()
        .expect("a snapshot built by `snapshot!` carries its bytes on the web");
    compare_frame_bytes(bytes, rgba, width, height, channel_delta)
}

/// Where a snapshot named `name` lives, under [`SNAPSHOT_DIR`].
#[cfg(not(target_arch = "wasm32"))]
pub fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(SNAPSHOT_DIR).join(name)
}

/// Encode `rgba` as a lossless WebP.
///
/// Available on the web too: a frame that fails a comparison is encoded there
/// and handed to the runner, since the bytes only exist in the browser.
pub fn encode_frame_webp(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .expect("webp encode failed");
    webp
}

/// Write `rgba` to `path` as a lossless WebP, creating its directory.
#[cfg(not(target_arch = "wasm32"))]
pub fn store_frame_webp(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(), SnapshotError> {
    check_frame_size(rgba, width, height)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(SnapshotError::Io)?;
    }
    std::fs::write(path, encode_frame_webp(rgba, width, height)).map_err(SnapshotError::Io)
}

/// Score the frame `rgba` against the snapshot at `path`, on SSIMULACRA2's
/// 0–100 scale where higher is closer.
///
/// The snapshot must exist; a missing one is an [`std::io::Error`] rather
/// than a cue to store the frame, so a caller that wants to store it says
/// so itself.
#[cfg(not(target_arch = "wasm32"))]
pub fn score_frame_webp(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<f64, SnapshotError> {
    compare_frame_webp(path, rgba, width, height, DEFAULT_TOLERANCE.channel_delta)
        .map(|comparison| comparison.score)
}

/// Measure the frame `rgba` against the snapshot at `path`.
#[cfg(not(target_arch = "wasm32"))]
fn compare_frame_webp(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
    channel_delta: u8,
) -> Result<Comparison, SnapshotError> {
    let reference_bytes = std::fs::read(path).map_err(SnapshotError::Io)?;
    compare_frame_bytes(&reference_bytes, rgba, width, height, channel_delta)
}

#[cfg(test)]
mod tests {
    use super::outlier_fraction;

    /// A frame of `pixels` mid-grey pixels.
    fn frame(pixels: usize) -> Vec<u8> {
        vec![128; pixels * 4]
    }

    #[test]
    fn an_identical_frame_has_no_outliers() {
        let baseline = frame(16);
        assert_eq!(outlier_fraction(&baseline, &frame(16), 8), 0.0);
    }

    #[test]
    fn a_difference_inside_the_channel_delta_is_not_an_outlier() {
        let baseline = frame(16);
        let mut current = frame(16);
        // Every pixel is 8 away, which is the delta itself and not beyond it.
        for pixel in current.as_chunks_mut::<4>().0 {
            pixel[0] = 136;
        }
        assert_eq!(outlier_fraction(&baseline, &current, 8), 0.0);
    }

    #[test]
    fn a_difference_beyond_the_channel_delta_is_an_outlier() {
        let baseline = frame(16);
        let mut current = frame(16);
        for pixel in current.as_chunks_mut::<4>().0 {
            pixel[0] = 137;
        }
        assert_eq!(outlier_fraction(&baseline, &current, 8), 1.0);
    }

    #[test]
    fn only_the_differing_pixels_are_counted() {
        let baseline = frame(16);
        let mut current = frame(16);
        // One pixel of sixteen, in a channel other than the first.
        current[2 * 4 + 2] = 200;
        assert_eq!(outlier_fraction(&baseline, &current, 8), 1.0 / 16.0);
    }

    #[test]
    fn the_alpha_channel_is_not_compared() {
        let baseline = frame(16);
        let mut current = frame(16);
        // Every alpha differs, but the frames still draw the same picture.
        for pixel in current.as_chunks_mut::<4>().0 {
            pixel[3] = 0;
        }
        assert_eq!(outlier_fraction(&baseline, &current, 8), 0.0);
    }
}
