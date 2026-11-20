//! A run's capture: which frames it keeps, and where it writes them.
//!
//! A capture is both an instruction and a sink. It names the frames a run
//! should keep — the run answers [`Capture::wants`] as it advances — and it
//! writes them where it was told, as they arrive rather than at the end, so
//! a run killed before its last frame keeps the frames it reached.
//!
//! A capture with no directory writes nothing: it only keeps the frames for
//! the caller that drove the run, which is what an in-process test reads.

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use std::path::Path;
use std::path::PathBuf;

use crate::audio::AudioFormat;

/// The frames a run keeps, and the directory it writes them into.
///
/// [`Capture::new`] sorts and deduplicates the frames, so a frame named
/// twice is one picture and [`Capture::wants`] may answer by search.
#[derive(Clone, Debug, Default)]
pub struct Capture {
    frames: Vec<u32>,
    into: Option<PathBuf>,
}

impl Capture {
    /// A capture of `frames`, written under `into` when one is named.
    #[must_use]
    pub fn new(mut frames: Vec<u32>, into: Option<PathBuf>) -> Self {
        frames.sort_unstable();
        frames.dedup();
        Self { frames, into }
    }

    /// Write one captured frame, which this target cannot do at all: it has
    /// no filesystem. A capture that names no directory still works — the
    /// frames go back to the caller that drove the run.
    ///
    /// # Errors
    ///
    /// Returns the message to show when the capture names a directory.
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    pub fn write_frame(
        &self,
        _index: u32,
        _pixels: &[u8],
        _size: (u32, u32),
    ) -> Result<Option<PathBuf>, String> {
        match self.into {
            Some(_) => {
                Err("a capture cannot be written here: this target has no filesystem".to_owned())
            }
            None => Ok(None),
        }
    }

    /// Write the mix, which this target cannot do at all: it has no
    /// filesystem. See [`Capture::write_frame`].
    ///
    /// # Errors
    ///
    /// Returns the message to show when the capture names a directory.
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    pub fn write_audio(
        &self,
        _format: AudioFormat,
        _samples: &[f32],
    ) -> Result<Option<PathBuf>, String> {
        match self.into {
            Some(_) => {
                Err("a capture cannot be written here: this target has no filesystem".to_owned())
            }
            None => Ok(None),
        }
    }

    /// Whether `index` is one of the frames the capture names.
    #[must_use]
    pub fn wants(&self, index: u32) -> bool {
        self.frames.binary_search(&index).is_ok()
    }

    /// The first frame the capture names that a run which advanced
    /// `advanced` frames never reached.
    ///
    /// A run that ends before this frame — because its client asked it to
    /// exit, or its length ran out — has a capture it never completed, and
    /// reporting the frame is what keeps the shortfall from passing as a
    /// finished capture.
    #[must_use]
    pub fn missing(&self, advanced: u32) -> Option<u32> {
        self.frames.iter().copied().find(|frame| *frame >= advanced)
    }

    /// Write one captured frame as a PNG, when the capture writes.
    ///
    /// `pixels` is RGBA, row-major, top to bottom, at `size`.
    ///
    /// # Errors
    ///
    /// Returns the message to show when the directory cannot be made, the
    /// frame does not encode, or the file cannot be written.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn write_frame(
        &self,
        index: u32,
        pixels: &[u8],
        size: (u32, u32),
    ) -> Result<Option<PathBuf>, String> {
        use image::ImageEncoder as _;

        let Some(into) = &self.into else {
            return Ok(None);
        };
        make_directory(into)?;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(pixels, size.0, size.1, image::ExtendedColorType::Rgba8)
            .map_err(|error| format!("frame {index} could not be encoded: {error}"))?;
        let path = into.join(format!("frame_{index}.png"));
        std::fs::write(&path, png).map_err(|error| {
            format!("the frame {} could not be written: {error}", path.display())
        })?;
        Ok(Some(path))
    }

    /// Write the mix as a FLAC, when the capture writes and the run played
    /// anything.
    ///
    /// A run whose audio world never filled anything writes no file: an
    /// empty FLAC would claim a silence the run never played, and a silent
    /// run has nothing to write anyway.
    ///
    /// # Errors
    ///
    /// Returns the message to show when the directory cannot be made, the
    /// mix does not encode, or the file cannot be written.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn write_audio(
        &self,
        format: AudioFormat,
        samples: &[f32],
    ) -> Result<Option<PathBuf>, String> {
        let Some(into) = &self.into else {
            return Ok(None);
        };
        if samples.is_empty() {
            return Ok(None);
        }
        make_directory(into)?;
        let flac = crate::record::RecordingFormat::Flac
            .encode(format, samples)
            .map_err(|error| format!("the mix could not be encoded: {error}"))?;
        let path = into.join("audio.flac");
        std::fs::write(&path, flac)
            .map_err(|error| format!("the mix {} could not be written: {error}", path.display()))?;
        Ok(Some(path))
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn make_directory(into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|error| {
        format!(
            "the output directory {} could not be made: {error}",
            into.display()
        )
    })
}

/// Capture the frames a windowed run presents, reading them back after the
/// frame has been drawn.
///
/// The behaviour runs one order past the presenter's, which is what draws
/// the frame: it reads the target the presenter left behind rather than
/// drawing the scene a second time. A scene that presents into the swap
/// chain has no target to read, so naming a capture also asks the windowed
/// run for an offscreen target.
///
/// Readback blocks the frame it captures: it is the frame the caller asked
/// for, and nothing else in the run waits on it.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub(crate) fn capture_presented(
    render: unlit_ecs::Entity,
    capture: Capture,
) -> unlit3d::frame::OnFrame {
    use unlit3d::frame::{FrameBehaviourOrder, OnFrame};
    use unlit3d::winit::presenter::PRESENT_ORDER;

    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER + 1),
        move |world, _entity, frame| {
            let index = frame.index;
            if !capture.wants(index) {
                return;
            }
            let (size, pixels) = match read_presented(world, render) {
                Ok(frame) => frame,
                // A frame the run drew before its presentation path existed
                // has no picture to read, so the capture names it rather
                // than passing the shortfall off as a frame it kept.
                Err(reason) => {
                    log::warn!("the run could not capture frame {index}: {reason}");
                    return;
                }
            };
            match capture.write_frame(index, &pixels, size) {
                Ok(Some(path)) => log::info!("the run wrote {}", path.display()),
                Ok(None) => {}
                Err(message) => log::error!("{message}"),
            }
        },
    )
}

/// Read back the picture the presenter left in its target this frame.
///
/// The presenter lives in the host world, the way the presentation path is
/// built there; the texture it holds a handle to lives in the render
/// sub-world's resource graph. Both are needed, so the reason a frame cannot
/// be read is carried out rather than swallowed.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn read_presented(
    world: &unlit_ecs::World,
    render: unlit_ecs::Entity,
) -> Result<((u32, u32), Vec<u8>), &'static str> {
    use unlit_wgpu::readback::readback_texture;
    use unlit3d::renderer::Renderer;
    use unlit3d::winit::presenter::Presenter;

    let handle = {
        let mut presenters = world.root_scope().query::<&Presenter>();
        let presenter = presenters
            .iter()
            .next()
            .ok_or("no presentation path is built")?;
        let target = presenter
            .target()
            .ok_or("the scene presents into the swap chain")?;
        target.texture.clone()
    };
    let mut nested = world
        .root_scope()
        .try_query_one::<&unlit_ecs::World>(render)
        .map_err(|_| "the render sub-world is gone")?;
    let render_world = nested.get();
    let context = {
        let mut renderers = render_world.root_scope().query::<&Renderer>();
        let renderer = renderers
            .iter()
            .next()
            .ok_or("the render world has no renderer")?;
        renderer.context()
    };
    let device = context
        .device(&render_world.root_scope())
        .map_err(|_| "the render context has no device")?;
    let queue = context
        .queue(&render_world.root_scope())
        .map_err(|_| "the render context has no queue")?;
    let texture = {
        let mut graph = context
            .try_graph(&render_world.root_scope())
            .map_err(|_| "the render context has no resource graph")?;
        graph
            .get()
            .get(&handle)
            .cloned()
            .ok_or("the capture's target is no longer a resource")?
    };
    let size = (texture.width(), texture.height());
    let pixels = pollster::block_on(readback_texture(&device, &queue, &texture));
    Ok((size, pixels))
}

#[cfg(test)]
mod tests {
    use super::Capture;

    #[test]
    fn the_frames_named_twice_are_one_picture() {
        let capture = Capture::new(vec![20, 0, 10, 20], None);
        assert!(capture.wants(0) && capture.wants(10) && capture.wants(20));
        assert!(!capture.wants(5));
    }

    #[test]
    fn a_short_capture_names_the_frame_it_never_reached() {
        let capture = Capture::new(vec![0, 10, 20], None);
        assert_eq!(capture.missing(0), Some(0));
        assert_eq!(capture.missing(12), Some(20));
        assert_eq!(capture.missing(21), None);
    }

    /// A run whose audio world never filled writes no mix: an empty FLAC would
    /// claim a silence the run never played.
    #[test]
    fn a_capture_with_no_audio_writes_no_mix() {
        let into = std::env::temp_dir();
        let capture = Capture::new(Vec::new(), Some(into));
        let format = crate::audio::AudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        assert!(
            capture
                .write_audio(format, &[])
                .expect("nothing to fail")
                .is_none(),
            "an empty mix writes no file"
        );
    }
}
