//! Copying GPU resources back to the host.
//!
//! A readback is the one GPU operation that produces something on the CPU:
//! render a frame into a texture, or fill a buffer, then copy it into a
//! `MAP_READ` buffer and map it. The two functions here do that in one call
//! each, so a caller that wants to inspect a frame — a test, a screenshot, a
//! debug tool — does not have to write the staging dance.
//!
//! Both are deliberately low-level: they take the wgpu handles and return raw
//! bytes, with no opinion about what the bytes mean. Decoding them into an
//! image, a vertex stream or anything else is the caller's.
//!
//! # Awaiting
//!
//! The calls are `async`, and the await is what waits for the copy: the bytes
//! cannot exist before the GPU has finished, so the future completes when the
//! mapping callback delivers them. A synchronous caller drives the future
//! through its executor — `pollster::block_on`, natively — and a test body,
//! which is already a future, awaits it.
//!
//! A blocking implementation is not an option, because one backend cannot
//! block: the browser's WebGPU backend reports `PollError::CantWait` rather
//! than waiting, and finishes the copy on its own event loop, where only a
//! future waiting across tasks can see it. Every other backend would let a
//! `poll(Wait)` do the waiting inside one call, but a readback that works on
//! some backends and panics on others would be a trap for the tests that run on
//! both.
//!
//! Natively the wait is bounded by [`READBACK_TIMEOUT`]: a device that has
//! stopped making progress ends the call with an error instead of hanging it.
//! A browser's event loop has no such bound to offer — the poll that would
//! carry the deadline is the one that cannot wait — so the harness's own
//! per-test timeout is what fails a page whose device wedged.
//!
//! Each call comes in two forms: [`readback_buffer`] and
//! [`readback_texture`] return the bytes and panic if there are none, which
//! is what a test or a demo wants; [`try_readback_buffer`] and
//! [`try_readback_texture`] return a [`ReadbackError`], which is what a tool
//! serving a request wants, so a device that went away becomes an answer
//! rather than the end of the process.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use core::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use std::sync::{Arc, Mutex, MutexGuard};

/// How long a readback waits for the GPU before it gives up.
///
/// A frame takes milliseconds, so this bounds a device that has stopped making
/// progress, not normal work: a readback that reaches it would never have
/// finished. It is the whole wait's budget, not one poll's, so a future woken
/// without its copy cannot restart the count. Only a backend that can wait —
/// every one but the browser's WebGPU — can enforce it.
pub const READBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a readback produced no bytes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadbackError {
    /// The device stopped making progress before the copy finished.
    #[error("the readback timed out after {0:?}")]
    Timeout(Duration),
    /// The staging buffer could not be mapped, so the copy never landed.
    #[error("the staging buffer could not be mapped: {0}")]
    Map(#[from] wgpu::BufferAsyncError),
    /// The device reported the wait itself as failed.
    #[error("polling the device for a readback failed: {0}")]
    Poll(#[from] wgpu::PollError),
}

/// Copy `size` bytes of `source`, starting at `offset`, back to the host.
///
/// The bytes are copied into a staging buffer and mapped, so the future
/// completes once the copy has run. `offset` and `size` are the same pair
/// [`wgpu::CommandEncoder::copy_buffer_to_buffer`] takes, and must satisfy the
/// same alignment: a multiple of [`wgpu::COPY_BUFFER_ALIGNMENT`] each, and
/// within `source`.
///
/// # Panics
///
/// If the copy is misaligned or out of range, if the device poll fails or
/// times out, or if the buffer cannot be mapped. [`try_readback_buffer`] is
/// the same call without the panic.
pub async fn readback_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Buffer,
    offset: u64,
    size: u64,
) -> Vec<u8> {
    try_readback_buffer(device, queue, source, offset, size)
        .await
        .unwrap_or_else(|error| {
            panic!("reading {size} bytes of a buffer from offset {offset} failed: {error}")
        })
}

/// Copy `size` bytes of `source`, starting at `offset`, back to the host.
///
/// # Errors
///
/// If the device poll fails or times out, or if the staging buffer cannot be
/// mapped. A copy that wgpu rejects as misaligned still panics: that is a
/// caller bug, not something a retry can fix.
pub async fn try_readback_buffer(
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
    .await
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
/// values, a `Rgba8Unorm` one linear ones — and the future completes once the
/// copy has run.
///
/// # Panics
///
/// If the texture's format has no block copy size (a depth-stencil format read
/// without choosing an aspect, say), or if the copy cannot run.
/// [`try_readback_texture`] is the same call without the panic.
pub async fn readback_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<u8> {
    try_readback_texture(device, queue, texture)
        .await
        .unwrap_or_else(|error| panic!("reading a texture back to the host failed: {error}"))
}

/// Copy `texture` back to the host as tight rows.
///
/// # Errors
///
/// If the device poll fails or times out, or if the staging buffer cannot be
/// mapped. A texture whose format has no block copy size panics: the call
/// cannot be spelled for it at all.
pub async fn try_readback_texture(
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
    )
    .await?;
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
async fn copy_and_map(
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
    // The callback hands the result over once, from whichever thread the
    // backend runs it on, so the slot it fills is shared rather than borrowed.
    let state = Arc::new(Mutex::new(MapState::default()));
    let sink = Arc::clone(&state);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        // The waker is taken out before it is used, so a backend that runs the
        // callback from another thread cannot re-enter this future while the
        // slot is locked.
        let waker = {
            let mut state = lock(&sink);
            state.result = Some(result);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    });
    MapFuture {
        state,
        device,
        submission,
        #[cfg(not(target_arch = "wasm32"))]
        deadline: Instant::now() + READBACK_TIMEOUT,
    }
    .await?;

    let bytes = {
        let view = slice.get_mapped_range().expect("the mapping is held");
        view.to_vec()
    };
    staging.unmap();
    Ok(bytes)
}

/// How the mapping callback and the future waiting for it hand the result over.
#[derive(Default)]
struct MapState {
    /// What the callback delivered, once it has run.
    result: Option<Result<(), wgpu::BufferAsyncError>>,
    /// The task to wake when it does.
    waker: Option<Waker>,
}

/// The wait between submitting the copy and the staging buffer being mapped.
///
/// Polling asks the device to make progress on the submission and then looks
/// for the callback's result. A backend that can wait — every native one —
/// finishes the copy inside that poll, so the first poll is the one that
/// completes; a browser's WebGPU backend refuses to wait, and the result
/// arrives from its event loop after the task has been woken.
struct MapFuture<'a> {
    /// The slot the mapping callback fills.
    state: Arc<Mutex<MapState>>,
    /// The device the submission went to, asked to make progress.
    device: &'a wgpu::Device,
    /// The submission the copy is part of.
    submission: Option<wgpu::SubmissionIndex>,
    /// When the wait as a whole gives up.
    ///
    /// Only a backend that waits can be held to it, so only a host build has
    /// one: a wasm build has no clock to read, and the backend that would
    /// consult it there is the one that refuses to wait.
    #[cfg(not(target_arch = "wasm32"))]
    deadline: Instant,
}

impl MapFuture<'_> {
    /// What is left of [`READBACK_TIMEOUT`], for the wait this poll asks for.
    ///
    /// Bounding each poll by the remainder rather than by a fresh timeout makes
    /// [`READBACK_TIMEOUT`] the wait's whole budget: a task woken without its
    /// copy — a busy device, a callback on a late submission — cannot restart
    /// the count and hold the call open indefinitely.
    #[cfg(not(target_arch = "wasm32"))]
    fn budget(&self) -> Option<Duration> {
        Some(self.deadline.saturating_duration_since(Instant::now()))
    }

    /// [`READBACK_TIMEOUT`], for the wait this poll asks for.
    ///
    /// A wasm build has no clock to read, so it cannot tell how much of the
    /// budget is left; each poll gets the whole of it instead. The WebGPU
    /// backend there refuses to wait before any timeout matters, so this bounds
    /// only the WebGL2 path, where the fence returns by itself.
    #[cfg(target_arch = "wasm32")]
    fn budget(&self) -> Option<Duration> {
        Some(READBACK_TIMEOUT)
    }

    /// End the wait with `error`, and forget the task that was waiting on it.
    ///
    /// The callback holds its own clone of the state and may still deliver a
    /// result after this future ends; a waker left behind would be woken for a
    /// future that is already done.
    fn fail(&self, error: ReadbackError) -> ReadbackError {
        lock(&self.state).waker = None;
        error
    }
}

impl Future for MapFuture<'_> {
    type Output = Result<(), ReadbackError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(result) = take_result(&this.state) {
            return Poll::Ready(result.map_err(ReadbackError::Map));
        }
        // Registered before the wait, so a backend that finishes the copy in
        // another task's context has somewhere to wake this one.
        lock(&this.state).waker = Some(cx.waker().clone());
        // Cloned rather than taken: a backend that cannot wait leaves the
        // future pending, so this poll runs again.
        match this.device.poll(wgpu::PollType::Wait {
            submission_index: this.submission.clone(),
            timeout: this.budget(),
        }) {
            Ok(_) => {}
            // A backend that finishes copies on its own event loop refuses to
            // wait; the callback's wake is what resumes this future.
            Err(wgpu::PollError::CantWait) => {}
            Err(wgpu::PollError::Timeout) => {
                let error = this.fail(ReadbackError::Timeout(READBACK_TIMEOUT));
                return Poll::Ready(Err(error));
            }
            Err(error) => {
                let error = this.fail(ReadbackError::Poll(error));
                return Poll::Ready(Err(error));
            }
        }
        if let Some(result) = take_result(&this.state) {
            return Poll::Ready(result.map_err(ReadbackError::Map));
        }
        Poll::Pending
    }
}

impl Drop for MapFuture<'_> {
    fn drop(&mut self) {
        // A dropped future — a cancelled task — leaves a waker no task is
        // waiting on; the callback may still run, so clear it here.
        lock(&self.state).waker = None;
    }
}

/// Take the callback's result, if it has arrived, and forget the waiting task.
fn take_result(state: &Mutex<MapState>) -> Option<Result<(), wgpu::BufferAsyncError>> {
    let mut state = lock(state);
    let result = state.result.take();
    if result.is_some() {
        state.waker = None;
    }
    result
}

/// Take `mutex`, ignoring poisoning.
///
/// A panic while the lock was held leaves the slot as some other task wrote it,
/// which is still the best answer available; reporting a poisoning error from a
/// readback would replace the real failure with a spurious one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
