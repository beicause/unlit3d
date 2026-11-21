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

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use super::scene::RunFrames;
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

/// Why a frame could not be read back for a capture.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureError {
    /// The host no longer holds the render sub-world the frame was drawn in.
    #[error("the render sub-world is gone")]
    NoRenderWorld,
    /// Nor a renderer to ask for the context the target lives in.
    #[error("the render world has no renderer")]
    NoRenderer,
    /// Nor the device and queue a copy needs.
    #[error("the render context has no device")]
    NoDevice,
    #[error("the render context has no queue")]
    NoQueue,
    /// Nor the graph that holds the target.
    #[error("the render context has no resource graph")]
    NoGraph,
    /// No single resource carries the label the capture reads back, or the
    /// one the graph found is not a texture.
    #[error("{0}")]
    Target(String),
    /// The target was made without the usage a copy out of it needs.
    #[error("the capture's target was not created with COPY_SRC, so it cannot be read back")]
    NotReadable,
    /// The copy itself failed: an unmappable format, or a device that gave up.
    #[error("reading the frame back failed: {0}")]
    Readback(#[from] unlit_wgpu::readback::ReadbackError),
}

/// Capture the frames a windowed run presents, reading them back after the
/// frame has been drawn.
///
/// The behaviour runs one order past the presenter's, which is what draws
/// the frame: it reads the target the presenter left behind rather than
/// drawing the scene a second time. The target is the one labelled
/// [`SCENE_COLOR_LABEL`](unlit3d::offscreen::SCENE_COLOR_LABEL), which is how
/// the capture names the picture without depending on the presenter that drew
/// it: a frame can be captured as soon as the target exists, whichever
/// presentation path drew it, and a host that presents through more than one
/// target names the one it means by its label rather than by the order the
/// behaviours were spawned in.
///
/// `frames` is how the behaviour knows which frame it is looking at: the run
/// advances the count once per frame it actually drew, so the frame a capture
/// keeps is numbered the way the budget counts them, and a turn that drew
/// nothing — a suspended app — is not mistaken for the frame it named.
///
/// Readback blocks the frame it captures: it is the frame the caller asked
/// for, and nothing else in the run waits on it.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub(crate) fn capture_presented(
    render: unlit_ecs::Entity,
    capture: Capture,
    frames: RunFrames,
) -> unlit3d::frame::OnFrame {
    use unlit3d::frame::{FrameBehaviourOrder, OnFrame};
    use unlit3d::winit::builtin::Resumed;
    use unlit3d::winit::presenter::PRESENT_ORDER;

    // The frame the last call looked at. A client-paced run draws the same
    // frame on every turn until the client steps again, and reading a target
    // back blocks the turn it runs in, so a frame is read once.
    let mut seen: Option<u32> = None;
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER + 1),
        move |world, _entity, _frame| {
            // A suspended host drew nothing this turn, so the target still
            // holds the last frame that did: reading it back under this
            // frame's number would write one picture twice, and a capture's
            // whole worth is that its frame N is frame N.
            let resumed = world
                .root_scope()
                .query::<&Resumed>()
                .iter()
                .next()
                .is_none_or(|state| state.0);
            if !resumed {
                return;
            }
            let Some(index) = frames.current() else {
                return;
            };
            if seen == Some(index) {
                return;
            }
            seen = Some(index);
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

/// Read back the picture the frame just drew, from the render sub-world's
/// target.
///
/// The target lives in the render sub-world's resource graph and the device
/// that drew it is the one its renderer holds, so both are needed; every way
/// this can come up short is a distinct error, since a capture that wrote
/// nothing has to say which of them it was.
///
/// The texture has to be created with `COPY_SRC`, which an offscreen target
/// is: the swap chain's image is not, and there is no handle to it anyway.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn read_presented(
    world: &unlit_ecs::World,
    render: unlit_ecs::Entity,
) -> Result<((u32, u32), Vec<u8>), CaptureError> {
    use unlit_wgpu::readback::try_readback_texture;
    use unlit_wgpu::resources::Resource;
    use unlit3d::offscreen::SCENE_COLOR_LABEL;
    use unlit3d::renderer::Renderer;

    let mut nested = world
        .root_scope()
        .try_query_one::<&unlit_ecs::World>(render)
        .map_err(|_| CaptureError::NoRenderWorld)?;
    let render_world = nested.get();
    let context = {
        let mut renderers = render_world.root_scope().query::<&Renderer>();
        let renderer = renderers.iter().next().ok_or(CaptureError::NoRenderer)?;
        renderer.context()
    };
    let device = context
        .device(&render_world.root_scope())
        .map_err(|_| CaptureError::NoDevice)?;
    let queue = context
        .queue(&render_world.root_scope())
        .map_err(|_| CaptureError::NoQueue)?;
    let texture = {
        let mut graph = context
            .try_graph(&render_world.root_scope())
            .map_err(|_| CaptureError::NoGraph)?;
        let graph = graph.get();
        let id = graph.id_labelled(SCENE_COLOR_LABEL).map_err(|_| {
            CaptureError::Target(format!(
                "no single resource is labelled {SCENE_COLOR_LABEL:?}, so there is no frame to read back"
            ))
        })?;
        let handle = graph
            .resolve(id)
            .ok_or_else(|| CaptureError::Target(format!("stale resource id {}", id.to_bits())))?;
        match graph.get::<Resource>(&handle) {
            Some(Resource::Texture(texture)) => texture.clone(),
            Some(Resource::TextureView(view)) => view.texture().clone(),
            _ => {
                return Err(CaptureError::Target(format!(
                    "the resource labelled {SCENE_COLOR_LABEL:?} is not a texture"
                )));
            }
        }
    };
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return Err(CaptureError::NotReadable);
    }
    let size = (texture.width(), texture.height());
    let pixels = pollster::block_on(try_readback_texture(&device, &queue, &texture))?;
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
