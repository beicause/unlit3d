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
//!
//! The wait is bounded by [`READBACK_TIMEOUT`]: a device that has stopped
//! making progress ends the call with an error instead of hanging it. A
//! backend that cannot wait for a submission at all — the browser's WebGPU
//! one, which finishes copies on its own event loop — says so through
//! [`ReadbackError::NoSynchronousWait`] rather than blocking forever.
//!
//! Each call comes in two forms: [`readback_buffer`] and
//! [`readback_texture`] return the bytes and panic if there are none, which
//! is what a test or a demo wants; [`try_readback_buffer`] and
//! [`try_readback_texture`] return a [`ReadbackError`], which is what a tool
//! serving a request wants, so a device that went away becomes an answer
//! rather than the end of the process.

use core::time::Duration;

use ringbuf::StaticRb;
use ringbuf::traits::{Consumer, Producer, Split};

/// How long a readback waits for the GPU before it gives up.
///
/// A frame takes milliseconds, so this bounds a device that has stopped making
/// progress, not normal work: a readback that reaches it would never have
/// finished.
pub const READBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a readback produced no bytes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadbackError {
    /// The device stopped making progress before the copy finished.
    #[error("the readback timed out after {0:?}")]
    Timeout(Duration),
    /// The backend cannot wait for a submission synchronously.
    ///
    /// The browser's WebGPU backend is the one that says this: it invokes the
    /// mapping callback from its own event loop, which a blocking call never
    /// reaches. Every other backend waits.
    #[error("the backend cannot wait synchronously, so a blocking readback cannot complete on it")]
    NoSynchronousWait,
    /// The staging buffer could not be mapped, so the copy never landed.
    #[error("the staging buffer could not be mapped: {0}")]
    Map(#[from] wgpu::BufferAsyncError),
    /// The staging buffer was still unmapped after the device reported the
    /// wait as finished.
    #[error("the device finished the wait without mapping the staging buffer")]
    Unmapped,
    /// The device reported the wait itself as failed.
    #[error("polling the device for a readback failed: {0}")]
    Poll(#[from] wgpu::PollError),
}

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
/// If the copy is misaligned or out of range, if the device poll fails or
/// times out, or if the buffer cannot be mapped. [`try_readback_buffer`] is
/// the same call without the panic.
pub fn readback_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Buffer,
    offset: u64,
    size: u64,
) -> Vec<u8> {
    try_readback_buffer(device, queue, source, offset, size).unwrap_or_else(|error| {
        panic!("reading {size} bytes of a buffer from offset {offset} failed: {error}")
    })
}

/// Copy `size` bytes of `source`, starting at `offset`, back to the host.
///
/// # Errors
///
/// If the device poll fails or times out, if the staging buffer cannot be
/// mapped, or if the backend cannot wait for the copy synchronously. A copy
/// that wgpu rejects as misaligned still panics: that is a caller bug, not
/// something a retry can fix.
pub fn try_readback_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Buffer,
    offset: u64,
    size: u64,
) -> Result<Vec<u8>, ReadbackError> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unlit_wgpu::readback::buffer"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    copy_and_map(
        device,
        queue,
        "unlit_wgpu::readback::buffer",
        &staging,
        |encoder| encoder.copy_buffer_to_buffer(source, offset, &staging, 0, size),
    )
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
/// [`try_readback_texture`] is the same call without the panic.
pub fn readback_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<u8> {
    try_readback_texture(device, queue, texture)
        .unwrap_or_else(|error| panic!("reading a texture back to the host failed: {error}"))
}

/// Copy `texture` back to the host as tight rows.
///
/// # Errors
///
/// If the device poll fails or times out, if the staging buffer cannot be
/// mapped, or if the backend cannot wait for the copy synchronously. A texture
/// whose format has no block copy size panics: the call cannot be spelled for
/// it at all.
pub fn try_readback_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, ReadbackError> {
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
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let padded = copy_and_map(
        device,
        queue,
        "unlit_wgpu::readback::texture",
        &staging,
        |encoder| {
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
                // Layer 0 only: the buffer is sized for one image, so a 3D or
                // array texture reads back its first layer.
                wgpu::Extent3d {
                    width: texture.width(),
                    height: texture.height(),
                    depth_or_array_layers: 1,
                },
            );
        },
    )?;
    if padded_bytes_per_row == tight_bytes_per_row {
        return Ok(padded);
    }
    let mut tight = Vec::with_capacity((tight_bytes_per_row * u64::from(rows)) as usize);
    for row in padded.chunks_exact(padded_bytes_per_row as usize) {
        tight.extend_from_slice(&row[..tight_bytes_per_row as usize]);
    }
    Ok(tight)
}

/// Record `encode` against `staging`, submit it, and return everything the
/// buffer holds once the copy has run.
///
/// The submit's own index is what the wait names, so a queue shared with other
/// work still waits for this copy rather than for whatever was submitted last.
fn copy_and_map(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    staging: &wgpu::Buffer,
    encode: impl FnOnce(&mut wgpu::CommandEncoder),
) -> Result<Vec<u8>, ReadbackError> {
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    encode(&mut encoder);
    let submission = queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    // The callback hands the result over once, so a one-slot ring is all the
    // handoff needs; the poll below is what makes the callback run. A
    // `StaticRb` keeps the slot inline instead of allocating it on the heap.
    let (mut sender, mut receiver) =
        StaticRb::<Result<(), wgpu::BufferAsyncError>, 1>::default().split();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.try_push(result);
    });
    wait_for_map(device, submission)?;
    receiver.try_pop().ok_or(ReadbackError::Unmapped)??;
    let bytes = {
        let view = slice.get_mapped_range().expect("the mapping is held");
        view.to_vec()
    };
    staging.unmap();
    Ok(bytes)
}

/// Block until `submission` has finished, or [`READBACK_TIMEOUT`] passes.
fn wait_for_map(
    device: &wgpu::Device,
    submission: Option<wgpu::SubmissionIndex>,
) -> Result<(), ReadbackError> {
    let wait = wgpu::PollType::Wait {
        submission_index: submission,
        timeout: Some(READBACK_TIMEOUT),
    };
    match device.poll(wait) {
        Ok(_) => Ok(()),
        Err(wgpu::PollError::Timeout) => Err(ReadbackError::Timeout(READBACK_TIMEOUT)),
        Err(wgpu::PollError::CantWait) => Err(ReadbackError::NoSynchronousWait),
        Err(error) => Err(ReadbackError::Poll(error)),
    }
}
