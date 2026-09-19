//! Small runtime-free utilities.

/// Blocks on `future` until it completes by busy-waiting with a noop waker.
///
/// This drives wgpu's one-shot startup futures (`request_adapter`,
/// `request_device`) in synchronous code — examples, headless tools and tests
/// that have no async runtime of their own. It is not suitable for production
/// async code: it never yields, so it starves every other task on the thread,
/// and it ignores wakeups entirely.
#[cfg(not(target_arch = "wasm32"))]
pub fn busy_wait_block_on<T>(future: impl Future<Output = T>) -> T {
    use core::task::{Context, Poll};

    let mut future = core::pin::pin!(future);
    let cx = &mut Context::from_waker(core::task::Waker::noop());
    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

/// Headless device setup shared by the crate's unit tests.
#[cfg(test)]
pub(crate) mod test {
    /// A `(device, queue)` pair on wgpu's noop backend.
    ///
    /// The noop backend stubs every GPU operation out — except buffer creation
    /// and mapping — so it needs no adapter and works everywhere, including
    /// machines with no GPU at all. That is enough for the unit tests, which
    /// only exercise validation, resource bookkeeping and command encoding.
    pub(crate) fn noop_device() -> (wgpu::Device, wgpu::Queue) {
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
    }
}
