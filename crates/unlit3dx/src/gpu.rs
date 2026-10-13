//! The GPU context and its asynchronous request.
//!
//! A scene is set up once the adapter and device exist, and the setup
//! needs a mutable world — so the request is a future the frame behaviour
//! drives on a local pool, and the ready callback lands as a command
//! between two event callbacks. Nothing outside this module needs to know
//! any of that: [`request_gpu`] runs the request and calls back when the
//! context is ready.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{Command, Entity, World};

use crate::executor::LocalPool;
use unlit_wgpu::capabilities::{DeviceCapabilities, DeviceTier};
use unlit3d::frame::{FrameBehaviourOrder, OnFrame};
use unlit3d::winit::builtin::{DisplayHandle, ExitRequest, Resumed, window};

/// The frame-behaviour order the GPU request runs at: before everything
/// that draws.
pub const REQUEST_GPU_ORDER: i32 = 0;

/// The GPU context a scene draws with.
///
/// Everything here is a wgpu handle, so the handles may cross into
/// another thread's setup; the scene itself cannot cross threads and is
/// set up where the request started. The native backends' handles are
/// send-and-sync; the WebGPU backend's device holds an `Rc`, so on the
/// web the handles only ever cross tasks, never threads.
#[derive(Clone)]
pub struct Gpu {
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

// The native GPU handles cross a thread inside the request's answer.
#[cfg(not(target_arch = "wasm32"))]
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Gpu>();
};

impl Gpu {
    /// Request the adapter and device that can present to `surface`.
    ///
    /// The surface is a probe: it exists only so the adapter is required
    /// to present to the window, and is dropped before the device is
    /// requested.
    pub async fn request(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let adapter = request_adapter(&instance, Some(&surface)).await?;
        // The surface only probes the adapter; the device request must not
        // keep it alive.
        drop(surface);
        Self::from_adapter(instance, adapter).await
    }

    /// Request an adapter and device without a window to present to, for
    /// headless renders.
    pub async fn headless() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = request_adapter(&instance, None).await?;
        Self::from_adapter(instance, adapter).await
    }

    /// Request a device from `adapter` and gather the context around it.
    async fn from_adapter(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        log::debug!("adapter landed; requesting a device");
        let tier = DeviceTier::from_env();
        let capabilities = tier.capabilities_of(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: tier.limits(&adapter.limits()),
                ..Default::default()
            })
            .await?;
        log::debug!("device landed");
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            capabilities,
        })
    }
}

/// Ask `instance` for a high-performance adapter that can present to
/// `surface`, when one is given.
async fn request_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'static>>,
) -> Result<wgpu::Adapter, Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("requesting an adapter");
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            ..Default::default()
        })
        .await?;
    Ok(adapter)
}

/// Start the app's GPU request once the window exists, and run `ready`
/// with the context once it lands.
///
/// The request runs off the main thread and its answer is polled by this
/// behaviour on the frames that follow; the callback runs as a queued
/// command with a mutable world, between two event callbacks. A failed
/// request is logged and sets the app's [`ExitRequest`].
pub fn request_gpu<F>(app: Entity, ready: F) -> OnFrame
where
    F: FnOnce(&mut World, &Gpu) + 'static,
{
    // The pool lives inside the behaviour and is driven on the frame
    // thread, so the answer slot needs no lock: the request future and
    // the behaviour that reads it run on the same thread.
    let pool = LocalPool::new();
    let answer: Rc<RefCell<Option<Result<Gpu, String>>>> = Rc::default();
    let mut state = 0u8;
    let mut ready = Some(ready);
    OnFrame::new(
        FrameBehaviourOrder(REQUEST_GPU_ORDER),
        move |world, _entity, _frame| {
            if state == 2 {
                return;
            }
            if state == 1 {
                // Drive the request future; a wake from the wgpu callback
                // thread is queued and picked up here.
                pool.run_ready();
                // One poll takes the answer out for good, so it is moved
                // into the command even if the callback panics half-way.
                let Some(result) = answer.borrow_mut().take() else {
                    return;
                };
                state = 2;
                let Some(ready) = ready.take() else {
                    return;
                };
                world.queue().push(RunGpuReady { app, result, ready });
                return;
            }
            let Some(window) = window(world) else {
                return;
            };
            let resumed = world
                .query::<&Resumed>()
                .next()
                .is_some_and(|(_, resumed)| resumed.0);
            if !resumed {
                return;
            }
            let Some(display) = world
                .query::<&DisplayHandle>()
                .next()
                .map(|(_, handle)| handle.0.clone())
            else {
                return;
            };
            state = 1;
            log::debug!("requesting a GPU context");
            let slot = Rc::clone(&answer);
            pool.spawn(async move {
                // The display handle is owned, not borrowed: the instance
                // it builds has to outlive this callback.
                let instance = wgpu::util::new_instance_with_webgpu_detection(
                    wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)),
                )
                .await;
                let result = match instance.create_surface(window) {
                    Ok(probe) => Gpu::request(instance, probe)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                *slot.borrow_mut() = Some(result);
            });
        },
    )
}

/// Run the GPU request's ready callback with a mutable world.
struct RunGpuReady<F> {
    /// The app entity the failure exit names.
    app: Entity,
    /// The request's answer.
    result: Result<Gpu, String>,
    /// The callback to run on success.
    ready: F,
}

impl<F: FnOnce(&mut World, &Gpu) + 'static> Command for RunGpuReady<F> {
    fn apply(self: Box<Self>, world: &mut World) {
        match self.result {
            Ok(gpu) => (self.ready)(world, &gpu),
            Err(error) => {
                log::error!("failed to start the renderer: {error}");
                let _ = world.with_mut::<ExitRequest, _>(self.app, |request| request.0 = true);
            }
        }
    }
}
