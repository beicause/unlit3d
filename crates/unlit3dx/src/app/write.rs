//! Writing a headless capture: the frames as PNG, the mix as FLAC.

use std::path::{Path, PathBuf};

use image::ImageEncoder;

use super::headless::{AUDIO_CHANNELS, HeadlessConfig, HeadlessOutput};
use crate::audio::AudioFormat;
use crate::record::RecordingFormat;

/// Write a capture under `out` and answer the paths it wrote.
///
/// One PNG per captured frame, named by the frame's index, and one FLAC of
/// the whole mix. A run that captured no frames writes no pictures, and the
/// paths answer only what was written.
pub(crate) fn capture(
    output: &HeadlessOutput,
    config: &HeadlessConfig,
    out: &Path,
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(out).map_err(|error| {
        format!(
            "the output directory {} could not be made: {error}",
            out.display()
        )
    })?;
    let mut written = Vec::new();
    let format = AudioFormat {
        sample_rate: config.audio_sample_rate,
        channels: AUDIO_CHANNELS as u16,
    };
    let flac = RecordingFormat::Flac
        .encode(format, &output.audio)
        .map_err(|error| format!("the mix could not be encoded: {error}"))?;
    let path = out.join("audio.flac");
    std::fs::write(&path, flac)
        .map_err(|error| format!("the mix {} could not be written: {error}", path.display()))?;
    written.push(path);
    for (frame, index) in output.frames.iter().zip(&config.capture) {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                frame,
                config.size.0,
                config.size.1,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| format!("frame {index} could not be encoded: {error}"))?;
        let path = out.join(format!("frame_{index}.png"));
        std::fs::write(&path, png).map_err(|error| {
            format!("the frame {} could not be written: {error}", path.display())
        })?;
        written.push(path);
    }
    Ok(written)
}
