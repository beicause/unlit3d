//! The one GPU bootstrap: an adapter, and the device requested from it.
//!
//! Every entry point in this repository needs the same steps before it can
//! draw: ask an instance for an adapter — one that can present to a window,
//! where there is a window — then ask that adapter for a device whose limits
//! are the device tier's, and keep the capabilities the adapter reported.
//! Written once rather than four times, because four copies are four chances
//! to drift apart in the limits they ask for and the capabilities they read.
//!
//! What stays with a caller is what differs between the entry points: which
//! instance to build (a windowed one carries the display handle, a browser's
//! names its backend), what to probe the adapter with, and what the handles
//! are for afterwards.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::capabilities::{DeviceCapabilities, DeviceTier};

/// A ready-to-use GPU context: the instance, the adapter, and the device with
/// the queue its work is submitted through.
///
/// Everything here is a wgpu handle, so the handles may cross into another
/// thread's setup; the scene that draws with them cannot cross threads and is
/// set up where the request started. The native backends' handles are
/// send-and-sync; the WebGPU backend's device holds an `Rc`, so on the web the
/// handles only ever cross tasks, never threads.
#[derive(Clone)]
pub struct GpuContext {
    /// The instance presentation surfaces are created from.
    pub instance: wgpu::Instance,
    /// The adapter the device was requested from.
    pub adapter: wgpu::Adapter,
    /// The device every buffer and pipeline belongs to.
    pub device: wgpu::Device,
    /// The queue frames are submitted to.
    pub queue: wgpu::Queue,
    /// What the device can do beyond the WebGPU baseline, captured while
    /// the adapter is still here to report it.
    pub capabilities: DeviceCapabilities,
    /// What the device said when it went away, if it has.
    ///
    /// A lost device stops submitting and reports nothing further, so this is
    /// the only record of it. It stays `None` for a healthy device, and for a
    /// device whose backend never calls back.
    health: DeviceHealth,
}

/// Whether the device behind it is still able to work, and why not if it is
/// not.
///
/// A lost device accepts no further work and reports nothing on its own: wgpu
/// hands the reason to a callback, once. This is the holder of that reason, so
/// a frame loop can ask rather than keep submitting into a device that does
/// nothing. A device set up without one never reports a loss.
#[derive(Clone, Default)]
pub struct DeviceHealth(Arc<Mutex<Option<DeviceLost>>>);

impl DeviceHealth {
    /// A health that no device reports to.
    ///
    /// What a caller gets when it built the device itself and did not install
    /// a callback; it always says the device is alive.
    #[must_use]
    pub fn unwatched() -> Self {
        Self::default()
    }

    /// What the device said when it was lost, or `None` while it is alive.
    #[must_use]
    pub fn lost(&self) -> Option<DeviceLost> {
        lock(&self.0).clone()
    }
}

/// What a device said when it was lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceLost {
    /// Why the device went away.
    pub reason: wgpu::DeviceLostReason,
    /// The backend's own account, which names the failure.
    pub message: String,
}

impl GpuContext {
    /// Request an adapter and a device without a surface to present to.
    ///
    /// The instance is the environment-driven one, so `WGPU_BACKEND` selects
    /// a backend the same way it does in an application. A browser hands out
    /// no WebGL adapter without a canvas, so its headless runs go through
    /// [`Self::present_to`] with a canvas nothing is drawn into.
    ///
    /// `tier` decides how much of the WebGPU baseline the device is asked
    /// for, and so which paths it takes; see [`DeviceTier`]. `label` names
    /// the device in validation messages and API traces, so a run that opens
    /// one says which entry point asked for it.
    pub async fn headless(
        tier: DeviceTier,
        power_preference: wgpu::PowerPreference,
        label: &str,
    ) -> Result<Self, GpuRequestError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = request_adapter(&instance, None, power_preference).await?;
        Self::from_adapter(instance, adapter, tier, label).await
    }

    /// Request an adapter that can present to `target`, and a device from it.
    ///
    /// `target` is a *probe*: a surface is created from it only so the adapter
    /// is required to be able to present, and is dropped again before the
    /// device is requested. The context deliberately ends up owning no
    /// surface, because a surface is tied to the native window it was created
    /// from — Android replaces that window across a suspension — while the
    /// adapter and device are not. Whoever presents builds a surface for the
    /// window it has at that moment.
    ///
    /// The instance is the caller's: a windowed entry point builds one that
    /// carries the display handle and checks whether the browser has WebGPU,
    /// which is an asynchronous step of its own; see
    /// [`wgpu::util::new_instance_with_webgpu_detection`]. `label` names the
    /// device the same way it does in [`Self::headless`].
    pub async fn present_to(
        instance: wgpu::Instance,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        tier: DeviceTier,
        power_preference: wgpu::PowerPreference,
        label: &str,
    ) -> Result<Self, GpuRequestError> {
        let probe = instance.create_surface(target)?;
        let adapter = request_adapter(&instance, Some(&probe), power_preference).await?;
        // The surface only probes the adapter; the device request must not
        // keep it alive.
        drop(probe);
        Self::from_adapter(instance, adapter, tier, label).await
    }

    /// Request a device from `adapter` and gather the context around it.
    async fn from_adapter(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        tier: DeviceTier,
        label: &str,
    ) -> Result<Self, GpuRequestError> {
        let capabilities = tier.capabilities_of(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some(label),
                required_limits: tier.limits(&adapter.limits()),
                ..Default::default()
            })
            .await?;
        // A lost device reports nothing on its own: the callback is the only
        // channel, and wgpu invokes it once. Recording it here means a caller
        // that notices frames have stopped can ask why.
        let health = DeviceHealth::default();
        let record = health.clone();
        device.set_device_lost_callback(move |reason, message| {
            *lock(&record.0) = Some(DeviceLost { reason, message });
        });
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            capabilities,
            health,
        })
    }

    /// The device's health, for a caller that wants to watch it elsewhere.
    ///
    /// A render world keeps one next to the device and queue it hands out, so
    /// the frame loop asks the same state this context holds: a device that
    /// went away stops every frame that would need it, and the reason survives
    /// the context being dropped.
    #[must_use]
    pub fn health(&self) -> DeviceHealth {
        self.health.clone()
    }

    /// What the device said when it was lost, or `None` while it is alive.
    ///
    /// A lost device accepts no further work: submissions stop and every
    /// resource it held is gone, so a caller that sees this should build a new
    /// context rather than draw again. The reason distinguishes a device the
    /// caller destroyed from one that failed on its own.
    #[must_use]
    pub fn lost(&self) -> Option<DeviceLost> {
        self.health.lost()
    }
}

/// Take `mutex`, ignoring poisoning.
///
/// A panic while the lock was held leaves the lost state as some other
/// thread wrote it, which is still the best answer available; reporting a
/// poisoning error from a query about the device would replace the real
/// failure with a spurious one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ask `instance` for an adapter, which must be able to present to `probe`
/// when one is given.
async fn request_adapter(
    instance: &wgpu::Instance,
    probe: Option<&wgpu::Surface<'static>>,
    power_preference: wgpu::PowerPreference,
) -> Result<wgpu::Adapter, GpuRequestError> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface: probe,
            ..Default::default()
        })
        .await?;
    Ok(adapter)
}

/// Why a GPU context could not be set up.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GpuRequestError {
    /// The probe surface could not be built from the target.
    #[error("failed to build the presentation surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    /// No adapter matched, or none of them can present to the probe.
    #[error("failed to request an adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    /// The adapter could not provide a device with the requested limits.
    #[error("failed to request a device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}
