//! Shared harness for the crate's GPU integration tests.
//!
//! Every test drives wgpu through a real `wgpu::Device` and reads
//! buffer/texture data back for assertions. The harness is deliberately
//! self-contained: besides `wgpu`, `glam`, `zerocopy`, `fast-ssim2` and
//! `image` (the dev-dependencies) plus `std` it depends on nothing else, so
//! test files include it with `mod common;` and never reach into the
//! library's internals for plumbing.
//!
//! This module is not a test binary itself — it is included by the per-topic
//! test files (`tests/*.rs`).

#![expect(
    dead_code,
    reason = "the harness is shared; one test binary uses a subset of it"
)]

use wgpu_unlit_render::util::busy_wait_block_on;

pub mod frame;
pub mod snapshot;

pub use frame::*;
pub use snapshot::*;

/// Pair a binding slot with a resource for a [`wgpu::BindGroupDescriptor`].
pub fn bg_entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

/// Decode an sRGB-encoded byte to linear light (matches the hardware
/// encoding applied by `Rgba8UnormSrgb` targets).
pub fn srgb_to_linear_u8(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A ready-to-use GPU context: the device every resource is created on and
/// the queue all work is submitted to.
///
/// Keep one `Ctx` alive per test process; every frame/readback helper takes
/// it by reference.
pub struct Ctx {
    /// The device all resources are created on.
    pub device: wgpu::Device,
    /// The queue all work is submitted to.
    pub queue: wgpu::Queue,
}

impl Ctx {
    /// Create a headless GPU context: no surface, no power preference, no
    /// backend filtering.
    ///
    /// `wgpu::RequestAdapterOptions::default()` accepts whatever adapter the
    /// system offers — on headless/CI machines that is frequently a software
    /// Vulkan device — so this never restricts the search.
    ///
    /// # Panics
    /// If no usable adapter/device is available (the tests require a real
    /// device; silently skipping would hide regressions).
    pub fn headless() -> Ctx {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            busy_wait_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("no graphics adapter available");
        let (device, queue) = busy_wait_block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wgpu_unlit_render::test"),
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

/// Blocking buffer readback: copy `source` into a `MAP_READ` buffer, map,
/// copy the bytes out, unmap. Returns the raw bytes.
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
    // wgpu 30's `Device::poll` returns a `Result`; wait without a timeout.
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("device poll failed during readback");
    rx.recv()
        .expect("map callback never fired")
        .expect("buffer map failed");

    // Copy the mapped view out before unmap (the view borrows the slice).
    let data = {
        let view = slice.get_mapped_range().expect("map range failed");
        view.to_vec()
    };
    readback.unmap();
    data
}
