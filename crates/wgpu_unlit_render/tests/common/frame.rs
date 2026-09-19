//! Offscreen render targets, frame readback and render-pass encoding for the
//! GPU integration tests.
//!
//! Every test renders into an offscreen [`ColorTarget`] and inspects the
//! returned [`Frame`] bytes; these helpers collapse the per-test boilerplate
//! (texture + view creation, pass encoding, texture→buffer copy with row
//! alignment, map readback) into one reusable path. Like the rest of the
//! harness they hold no hidden state: each call encodes fresh commands
//! against caller-owned resources.

use super::{Ctx, readback_buffer};

/// An offscreen RGBA8 color target (`RENDER_ATTACHMENT | COPY_SRC`) plus its
/// default view. Created once per test scenario; rendered into any number of
/// times (every pass clears on load).
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
///
/// Deref-coerces to `&[u8]`, so the snapshot/scoring helpers taking byte
/// slices accept `&frame` directly.
pub struct Frame {
    /// Row-major tightly-packed RGBA8 bytes (no row padding).
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
    /// Raw pixel bytes.
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

/// Opaque sRGB clear/draw color from RGB components (alpha = 1).
pub fn rgb(r: f64, g: f64, b: f64) -> wgpu::Color {
    wgpu::Color { r, g, b, a: 1.0 }
}

/// Bytes per texel block of `texture`'s format, derived from
/// [`wgpu::TextureFormat::block_copy_size`] — never hardcode pixel sizes.
///
/// # Panics
/// If the format has no single copy size (multi-planar or combined
/// depth-stencil without an aspect) — none of this harness's formats.
pub fn texel_bytes(texture: &wgpu::Texture) -> u32 {
    texture
        .format()
        .block_copy_size(None)
        .expect("texture format has a block copy size")
}

/// Blocking full-texture readback as **tight** bytes (no row padding):
/// copies the whole texture into a scratch buffer (rows padded up to
/// `COPY_BUFFER_ALIGNMENT` as required), submits, maps, and compacts rows.
///
/// Pass [`texel_bytes`] for `bytes_per_pixel`.
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
    // Compact padded rows into the tight layout the callers expect.
    let tight = (tight_bpr * height as u64) as usize;
    let mut out = Vec::with_capacity(tight);
    for row in 0..height as usize {
        let start = row * padded_bpr as usize;
        out.extend_from_slice(&padded[start..start + tight_bpr as usize]);
    }
    out
}

// ---------------------------------------------------------------------------
// Frame analysis helpers
// ---------------------------------------------------------------------------

/// Count pixels whose color differs from `background` beyond `tolerance`
/// (any RGB channel) — "how much did this draw cover".
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
