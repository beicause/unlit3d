//! Headless helpers shared by the crate's tests.

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
