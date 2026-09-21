//! Shared GPU test harness for wgpu-unlit crates.
//!
//! Every test drives wgpu through a real `wgpu::Device` and reads
//! buffer/texture data back for assertions.  The harness is deliberately
//! self-contained: besides `wgpu`, `glam`, plus `fast-ssim2` and `image`
//! (both behind the `snapshot` feature) it depends on nothing else.
//!
//! # Features
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `snapshot` | Enables perceptual snapshot assertions via SSIMULACRA2. |

#![forbid(unsafe_code)]

use core::task::{Context, Poll};
use std::future::Future;

// ---------------------------------------------------------------------------
// Busy-wait future driver (no async runtime needed)
// ---------------------------------------------------------------------------

/// Blocks on `future` by busy-waiting with a noop waker.
///
/// Drives wgpu's one-shot startup futures (`request_adapter`,
/// `request_device`) in synchronous code — examples, headless tools and
/// tests that have no async runtime of their own.
pub fn busy_wait_block_on<T>(future: impl Future<Output = T>) -> T {
    let mut future = core::pin::pin!(future);
    let cx = &mut Context::from_waker(core::task::Waker::noop());
    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
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
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            busy_wait_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("no graphics adapter available");
        let (device, queue) = busy_wait_block_on(adapter.request_device(&wgpu::DeviceDescriptor {
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
    let padded_bpr = tight_bpr.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
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

    const SNAPSHOT_DIR: &str = "tests/snapshots";

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

    pub fn encode_frame_webp(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
            .expect("webp encode failed");
        webp
    }

    pub const DEFAULT_MIN_SCORE: f64 = 85.0;

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
        assert_eq!(
            rgba.len(),
            (width as usize) * (height as usize) * 4,
            "frame size mismatch"
        );
        let path = std::path::Path::new(SNAPSHOT_DIR).join(name);
        let update = std::env::var_os("SNAPSHOT_UPDATE").is_some();

        if !path.exists() || update {
            let dir = std::path::Path::new(SNAPSHOT_DIR);
            // The symlink target is always a real directory; there is
            // nothing to create.
            if !dir.is_symlink() && !dir.is_dir() {
                std::fs::create_dir_all(dir).expect("create snapshots dir");
            }
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
            "snapshot `{name}` perceptual mismatch: SSIMULACRA2 score {score:.2} < {min_score}
             if the change is intentional, re-store with SNAPSHOT_UPDATE=1"
        );
    }

    fn score_against_reference(
        path: &std::path::Path,
        name: &str,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> f64 {
        let reference_bytes =
            std::fs::read(path).unwrap_or_else(|e| panic!("read snapshot {name}: {e}"));
        let reference =
            image::load_from_memory_with_format(&reference_bytes, image::ImageFormat::WebP)
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
}

#[cfg(feature = "snapshot")]
pub use snapshot_impl::*;
