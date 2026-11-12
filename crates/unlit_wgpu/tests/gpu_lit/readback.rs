//! The readback paths: rows that are not copy aligned, and the library's own readback.

use super::*;

/// A readback of a texture whose row is not a multiple of the copy alignment
/// comes back tight and correct.
///
/// A texture-to-buffer copy aligns every row to `COPY_BYTES_PER_ROW_ALIGNMENT`,
/// which is 256 bytes and much coarser than the buffer alignment. A width that
/// is not a multiple of 64 RGBA pixels — 256 bytes — therefore needs padding
/// *and* the padding stripped. Every other readback in the suite is 256 wide,
/// where the two alignments coincide, so an unpadded copy validates there by
/// luck and fails on any other width.
pub(super) async fn a_readback_handles_a_row_that_is_not_copy_aligned() {
    // 60 RGBA pixels is 240 bytes: a multiple of the buffer alignment (4) but
    // not of the copy row alignment (256), which is the case a copy that pads
    // rows without stripping the padding gets wrong.
    const WIDTH: u32 = 60;
    const HEIGHT: u32 = 8;
    let ctx = Ctx::headless().await;

    let ft = create_render_target(&ctx.device, COLOR_FORMAT, WIDTH, HEIGHT, 1);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_lit::unaligned_clear"),
        });
    {
        let mut pass = ft.attachments.begin_pass(
            &mut encoder,
            "test pass",
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass, None);
    }
    ctx.queue.submit([encoder.finish()]);

    let frame = Frame {
        rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &ft.color),
        width: WIDTH,
        height: HEIGHT,
    };

    // Tight bytes, not padded ones: the buffer rows are stripped back off.
    assert_eq!(
        frame.rgba.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "the readback must return exactly the frame's pixels"
    );
    // Every row is the clear colour, so a row that lost its stride would show
    // up as a mismatch somewhere in the image. The readback is the texture's
    // own sRGB encoding, so the clear value is encoded the same way before
    // comparing.
    let encode = |linear: f64| {
        let c = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round().clamp(0.0, 255.0) as u8
    };
    let expect = [encode(CLEAR[0]), encode(CLEAR[1]), encode(CLEAR[2])];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let px = frame.pixel_u8(x, y);
            assert!(
                px[..3]
                    .iter()
                    .zip(expect)
                    .all(|(&got, want)| got.abs_diff(want) <= 2),
                "pixel ({x}, {y}) should be the clear colour, got {px:?}"
            );
        }
    }
}

/// The library's own texture readback returns tight rows.
///
/// The same not-copy-aligned width the harness test uses, but through
/// [`unlit_wgpu::readback::readback_texture`] rather than the test harness, so the
/// crate's readback is what is checked. The buffer rows must be stripped back
/// off, and the texture's format — not a caller's guess — decides the width.
pub(super) async fn the_readback_module_returns_a_texture_as_tight_rows() {
    // 60 RGBA pixels is 240 bytes: a multiple of the buffer alignment but not
    // of the copy row alignment, so the copy has to be padded and stripped.
    const WIDTH: u32 = 60;
    const HEIGHT: u32 = 8;
    let ctx = Ctx::headless().await;

    let ft = create_render_target(&ctx.device, COLOR_FORMAT, WIDTH, HEIGHT, 1);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_lit::readback_module"),
        });
    {
        let mut pass = ft.attachments.begin_pass(
            &mut encoder,
            "test pass",
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass, None);
    }
    ctx.queue.submit([encoder.finish()]);

    let pixels = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &ft.color);
    assert_eq!(
        pixels.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "the readback returns exactly the frame's pixels, with no row padding",
    );
    // Every pixel is the clear colour, so a row that kept its padding would
    // show up as a mismatch. The texture encodes sRGB, so the clear value is
    // encoded the same way before comparing.
    let encode = |linear: f64| {
        let c = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round().clamp(0.0, 255.0) as u8
    };
    let expect = [encode(CLEAR[0]), encode(CLEAR[1]), encode(CLEAR[2])];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let off = ((y * WIDTH + x) * 4) as usize;
            let got = &pixels[off..off + 3];
            assert!(
                got.iter()
                    .zip(expect)
                    .all(|(&got, want)| got.abs_diff(want) <= 2),
                "pixel ({x}, {y}) should be the clear colour, got {got:?}"
            );
        }
    }
}

/// The library's buffer readback returns the requested slice.
pub(super) async fn the_readback_module_returns_a_buffer_slice() {
    let ctx = Ctx::headless().await;
    let values: [u32; 16] = core::array::from_fn(|index| index as u32 * 3 + 1);
    let source = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_lit::readback_source"),
        size: core::mem::size_of_val(&values) as u64,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue
        .write_buffer(&source, 0, zerocopy::IntoBytes::as_bytes(&values));

    // A slice of the middle, offset to a second element and aligned to the
    // buffer copy alignment.
    let element = core::mem::size_of::<u32>() as u64;
    let bytes = unlit_wgpu::readback::readback_buffer(
        &ctx.device,
        &ctx.queue,
        &source,
        element * 4,
        element * 3,
    );
    let expected: Vec<u8> = values[4..7]
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect();
    assert_eq!(
        bytes, expected,
        "the readback returns exactly the slice asked for"
    );
}
