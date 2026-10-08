//! Copying GPU resources back to the host.
//!
//! A readback is the one GPU operation that produces something on the CPU:
//! render a frame into a texture, or fill a buffer, then copy it into a
//! `MAP_READ` buffer and map it. The two functions here do that in one
//! blocking call each, so a caller that wants to inspect a frame — a test, a
//! screenshot, a debug tool — does not have to write the staging dance.
//!
//! Both are deliberately low-level: they take the wgpu handles and return raw
//! bytes, with no opinion about what the bytes mean. Decoding them into an
//! image, a vertex stream or anything else is the caller's.
//!
//! # Blocking
//!
//! Both calls submit the copy, poll the device to completion and map the
//! staging buffer, so they block until the GPU has finished. That is what a
//! readback is for: the bytes cannot exist before then. A frame loop should
//! only call one where it actually needs the pixels.

/// Copy `size` bytes of `source`, starting at `offset`, back to the host.
///
/// The bytes are copied into a staging buffer and mapped, so the call blocks
/// until the copy has run. `offset` and `size` are the same pair
/// [`wgpu::CommandEncoder::copy_buffer_to_buffer`] takes, and must satisfy the
/// same alignment: a multiple of [`wgpu::COPY_BUFFER_ALIGNMENT`] each, and
/// within `source`.
///
/// # Panics
///
/// If the copy is misaligned or out of range, if the device poll fails, or if
/// the buffer cannot be mapped.
pub fn readback_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Buffer,
    offset: u64,
    size: u64,
) -> Vec<u8> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unlit_wgpu::readback::buffer"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("unlit_wgpu::readback::buffer"),
    });
    encoder.copy_buffer_to_buffer(source, offset, &staging, 0, size);
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the device polls to completion during a readback");
    receiver
        .recv()
        .expect("the map callback fires once the device has polled")
        .expect("the staging buffer maps for reading");
    let bytes = {
        let view = slice.get_mapped_range().expect("the mapping is held");
        view.to_vec()
    };
    staging.unmap();
    bytes
}

/// Copy `texture` back to the host as tight rows.
///
/// The texture's own dimensions and format decide the copy, so the result is
/// its whole mip level 0 with no row padding: the buffer rows wgpu requires to
/// be aligned to [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] are stripped back off.
/// A row is the texture's width rounded up to whole format blocks, so a
/// block-compressed texture reads back block-aligned exactly as wgpu lays it
/// out.
///
/// The bytes are in the texture's format — an sRGB texture reads back sRGB
/// values, a `Rgba8Unorm` one linear ones — and the call blocks until the copy
/// has run.
///
/// # Panics
///
/// If the texture's format has no block copy size (a depth-stencil format read
/// without choosing an aspect, say), or if the copy cannot run.
pub fn readback_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<u8> {
    let format = texture.format();
    let (block_width, block_height) = format.block_dimensions();
    let block_bytes = format
        .block_copy_size(None)
        .expect("the texture's format has a block copy size to read it back in");
    // wgpu copies whole blocks: a row is the texture's width rounded up to a
    // block, and the copy runs for as many whole rows as the height needs.
    let rows = texture.height().div_ceil(block_height);
    let tight_bytes_per_row =
        u64::from(texture.width().div_ceil(block_width)) * u64::from(block_bytes);
    let padded_bytes_per_row =
        tight_bytes_per_row.next_multiple_of(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
    let size = padded_bytes_per_row * u64::from(rows);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unlit_wgpu::readback::texture"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("unlit_wgpu::readback::texture"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row as u32),
                rows_per_image: Some(rows),
            },
        },
        // Layer 0 only: the buffer is sized for one image, so a 3D or array
        // texture reads back its first layer.
        wgpu::Extent3d {
            width: texture.width(),
            height: texture.height(),
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let padded = readback_buffer(device, queue, &staging, 0, size);
    if padded_bytes_per_row == tight_bytes_per_row {
        return padded;
    }
    let mut tight = Vec::with_capacity((tight_bytes_per_row * u64::from(rows)) as usize);
    for row in padded.chunks_exact(padded_bytes_per_row as usize) {
        tight.extend_from_slice(&row[..tight_bytes_per_row as usize]);
    }
    tight
}
