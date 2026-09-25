//! Shared GPU test harness for wgpu-unlit crates.
//!
//! Every test drives wgpu through a real `wgpu::Device` and reads
//! buffer/texture data back for assertions.  The harness is deliberately
//! self-contained: besides `wgpu`, `glam`, `log`, `pollster` (driving wgpu's
//! async adapter/device requests), the platform logger backend, plus
//! `fast-ssim2` and `image` (both behind the `snapshot` feature) it depends on
//! nothing else.
//!
//! # Logging
//!
//! The harness reports what it does through [`log`] rather than by printing:
//! [`Ctx::headless`] installs a backend on first use, so a test's own
//! `log` records — and wgpu's — reach the terminal.  `RUST_LOG` selects the
//! level; the default is `warn`.
//!
//! # Features
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `snapshot` | Enables perceptual snapshot assertions via SSIMULACRA2. |

#![forbid(unsafe_code)]

use std::sync::Once;

/// The logging facade the tests record through, re-exported so a test crate
/// needs no `log` dependency of its own.
///
/// Recording is what the harness's backend reports; see [`init_logging`].
pub use log;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Install the harness's logger backend once, for the whole test process.
///
/// Called by [`Ctx::headless`], so a test that builds a context logs without
/// asking.  Tests that never build a context — the ECS ones — call this
/// directly if they log.
///
/// `RUST_LOG` picks the level natively; without it the default is `warn`.
/// A process holds one logger, so repeated calls are no-ops — and a test that
/// installed a backend of its own first keeps it.
pub fn init_logging() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // `try_init`, not `init`: installing over a backend that is already
            // there would panic, and that backend is the one to keep.
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
                .try_init()
                .ok();
        }
        #[cfg(target_arch = "wasm32")]
        {
            // The browser console is the terminal here.
            console_log::init_with_level(log::Level::Warn).ok();
        }
    });
}

// ---------------------------------------------------------------------------
// GPU context
// ---------------------------------------------------------------------------

/// A ready-to-use GPU context.
pub struct Ctx {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Ctx {
    /// Create a headless GPU context with the default adapter.
    pub fn headless() -> Ctx {
        init_logging();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("no graphics adapter available");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wgpu_unlit_test_util"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .expect("failed to request device");
        Ctx { device, queue }
    }
}

/// Pair a binding slot with a resource.
pub fn bg_entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

/// Decode an sRGB-encoded byte to linear light.
pub fn srgb_to_linear_u8(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------------------
// Buffer readback
// ---------------------------------------------------------------------------

/// Blocking buffer readback: copy `source` into a `MAP_READ` buffer, map,
/// copy bytes out, unmap.
pub fn readback_buffer(ctx: &Ctx, source: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let readback = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_test::readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_test::copy"),
        });
    encoder.copy_buffer_to_buffer(source, 0, &readback, 0, size);
    ctx.queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("device poll failed during readback");
    rx.recv()
        .expect("map callback never fired")
        .expect("buffer map failed");

    let data = {
        let view = slice.get_mapped_range().expect("map range failed");
        view.to_vec()
    };
    readback.unmap();
    data
}

// ---------------------------------------------------------------------------
// Offscreen colour target and frame readback
// ---------------------------------------------------------------------------

/// An offscreen RGBA8 colour target plus its default view.
pub struct ColorTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl ColorTarget {
    /// `Rgba8UnormSrgb` target sized `(width, height)`.
    pub fn new(device: &wgpu::Device, label: &str, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

/// A read-back frame: tight RGBA8 bytes plus dimensions.
pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl core::ops::Deref for Frame {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.rgba
    }
}

impl Frame {
    pub fn pixel_u8(&self, x: u32, y: u32) -> [u8; 4] {
        let off = ((y as usize * self.width as usize) + x as usize) * 4;
        [
            self.rgba[off],
            self.rgba[off + 1],
            self.rgba[off + 2],
            self.rgba[off + 3],
        ]
    }
}

/// Opaque sRGB clear/draw colour from RGB components (alpha = 1).
pub fn rgb(r: f64, g: f64, b: f64) -> wgpu::Color {
    wgpu::Color { r, g, b, a: 1.0 }
}

/// Bytes per texel block of `texture`'s format.
pub fn texel_bytes(texture: &wgpu::Texture) -> u32 {
    texture
        .format()
        .block_copy_size(None)
        .expect("texture format has a block copy size")
}

/// Blocking full-texture readback as tight bytes (no row padding).
pub fn read_texture_bytes(
    ctx: &Ctx,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) -> Vec<u8> {
    let tight_bpr = (width * bytes_per_pixel) as u64;
    // A texture-to-buffer copy aligns every row to
    // `COPY_BYTES_PER_ROW_ALIGNMENT`, which is coarser than the buffer
    // alignment and is what the command encoder validates.
    let padded_bpr = tight_bpr.next_multiple_of(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
    let size = padded_bpr * height as u64;
    let dst = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_test::tex_readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_test::tex_copy"),
        });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &dst,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr as u32),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([encoder.finish()]);
    let padded = readback_buffer(ctx, &dst, size);

    if padded_bpr == tight_bpr {
        return padded;
    }
    let tight = (tight_bpr * height as u64) as usize;
    let mut out = Vec::with_capacity(tight);
    for row in 0..height as usize {
        let start = row * padded_bpr as usize;
        out.extend_from_slice(&padded[start..start + tight_bpr as usize]);
    }
    out
}

/// Count pixels whose colour differs from `background` beyond `tolerance`.
pub fn count_pixels_off_background(px: &[u8], background: [f64; 3], tolerance: u8) -> usize {
    px.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| {
            p[..3]
                .iter()
                .zip(background)
                .any(|(&c, bg)| (c as f64 / 255.0 - bg).abs() * 255.0 > tolerance as f64)
        })
        .count()
}

// ---------------------------------------------------------------------------
// Snapshot helpers (require the `snapshot` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "snapshot")]
mod snapshot_impl {
    use fast_ssim2::{LinearRgbImage, ToLinearRgb, compute_ssimulacra2, srgb_u8_to_linear};
    use image::ImageEncoder;

    /// Where [`assert_image_snapshot`] looks a snapshot up by name, relative to
    /// the process's working directory.
    const SNAPSHOT_DIR: &str = "tests/snapshots";

    /// The perceptual score a frame must reach to match its snapshot.
    pub const DEFAULT_MIN_SCORE: f64 = 85.0;

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
        Score(fast_ssim2::Ssimulacra2Error),
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

    /// Encode `rgba` as a lossless WebP.
    pub fn encode_frame_webp(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
            .expect("webp encode failed");
        webp
    }

    /// Where a snapshot named `name` lives, under [`SNAPSHOT_DIR`].
    pub fn snapshot_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(SNAPSHOT_DIR).join(name)
    }

    /// Write `rgba` to `path` as a lossless WebP, creating its directory.
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
    pub fn score_frame_webp(
        path: &std::path::Path,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<f64, SnapshotError> {
        check_frame_size(rgba, width, height)?;
        let reference_bytes = std::fs::read(path).map_err(SnapshotError::Io)?;
        let reference =
            image::load_from_memory_with_format(&reference_bytes, image::ImageFormat::WebP)
                .map_err(SnapshotError::Decode)?
                .to_rgba8();
        if (reference.width(), reference.height()) != (width, height) {
            return Err(SnapshotError::Dimensions {
                snapshot: (reference.width(), reference.height()),
                frame: (width, height),
            });
        }

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
        compute_ssimulacra2(reference_frame, current_frame).map_err(SnapshotError::Score)
    }

    /// Reject a frame whose bytes do not describe `width` x `height` RGBA
    /// pixels.
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

    pub fn assert_image_snapshot(name: &str, rgba: &[u8], width: u32, height: u32) {
        assert_image_snapshot_with_threshold(name, rgba, width, height, DEFAULT_MIN_SCORE);
    }

    pub fn assert_image_snapshot_with_threshold(
        name: &str,
        rgba: &[u8],
        width: u32,
        height: u32,
        min_score: f64,
    ) {
        let path = snapshot_path(name);
        let update = std::env::var_os("SNAPSHOT_UPDATE").is_some();

        if !path.exists() || update {
            // A snapshot name may carry subdirectories. `create_dir_all` is a
            // no-op for the directories that already exist, including the
            // `tests/snapshots` symlink into the asset repository.
            store_frame_webp(&path, rgba, width, height)
                .unwrap_or_else(|e| panic!("store snapshot {name}: {e}"));
            log::info!(
                "snapshot `{name}` {}",
                if update { "updated" } else { "stored" }
            );
            return;
        }

        let score = score_frame_webp(&path, rgba, width, height)
            .unwrap_or_else(|e| panic!("compare snapshot {name}: {e}"));
        assert!(
            score >= min_score,
            "snapshot `{name}` perceptual mismatch: SSIMULACRA2 score {score:.2} < {min_score}
             if the change is intentional, re-store with SNAPSHOT_UPDATE=1"
        );
    }
}

#[cfg(feature = "snapshot")]
pub use snapshot_impl::*;
