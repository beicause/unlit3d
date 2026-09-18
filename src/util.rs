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
pub(crate) mod test_device {
    /// A `(device, queue)` pair on whatever backend the machine offers.
    ///
    /// No adapter filtering: a software backend is fine for validation and
    /// command-encoding tests.
    pub(crate) fn device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = super::busy_wait_block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("an adapter");
        super::busy_wait_block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("a device")
    }
}
