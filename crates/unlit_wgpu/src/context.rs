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
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            capabilities,
        })
    }
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
