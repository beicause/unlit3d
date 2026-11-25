//! The GPU context and its asynchronous request.
//!
//! A scene is set up once the adapter and device exist, and the setup
//! needs a mutable world — so the request is a future the world itself
//! runs, driven by whoever drives the world, and the ready callback lands
//! as a command between two event callbacks. Nothing outside this module
//! needs to know any of that: [`request_gpu`] runs the request and calls
//! back when the context is ready.
//!
//! The adapter and device themselves are requested by
//! [`unlit_wgpu::context`], which every entry point in this repository shares;
//! what this module adds is the frame-driven form of the request and the
//! callback a scene is set up in.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{Command, Entity, EntityAccessError, World};

use crate::executor::LocalTasks;
use unlit_wgpu::capabilities::DeviceTier;
use unlit3d::frame::{FrameBehaviourOrder, OnFrame};
use unlit3d::winit::builtin::{DisplayHandle, ExitRequest, Resumed, window};

/// The GPU context a scene draws with.
pub use unlit_wgpu::context::{GpuContext as Gpu, GpuRequestError};

/// The frame-behaviour order the GPU request runs at: before the default
/// band, so the GPU is there before any behaviour that states no order runs.
pub(crate) const REQUEST_GPU_ORDER: i32 = -1;

/// Ask for the adapter and device that can present to `target`, at the
/// device tier `UNLIT3D_DEVICE_TIER` names.
///
/// This is [`Gpu::present_to`] with the choices a windowed app makes: a
/// high-performance adapter, so a machine with two GPUs draws with the
/// discrete one.
async fn request(
    instance: wgpu::Instance,
    target: impl Into<wgpu::SurfaceTarget<'static>>,
) -> Result<Gpu, GpuRequestError> {
    let tier = DeviceTier::from_env();
    Gpu::present_to(
        instance,
        target,
        tier,
        wgpu::PowerPreference::HighPerformance,
        "unlit3dx",
    )
    .await
}

/// Why the GPU request could not start the renderer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GpuStartError {
    /// The adapter or device could not be opened.
    #[error("failed to start the renderer: {0}")]
    Request(#[from] GpuRequestError),
    /// The request never ran: the world that was to run it has no task set.
    #[error("the world has no task set to run the GPU request on")]
    Unstartable,
}

/// The answer a GPU request settles on.
enum GpuRequest {
    /// The adapter and device are ready for the scene setup.
    Ready(Gpu),
    /// The request failed; the app is asked to exit instead.
    Failed(GpuRequestError),
    /// The request never ran: the world that was to run it has no task set.
    Unstartable,
}

/// The stages a GPU request moves through, one per frame behaviour.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestStage {
    /// Waiting for a resumed window to build the request from.
    WaitingForWindow,
    /// The request future is running; its answer is read on a later frame.
    Requesting,
    /// The ready callback has been queued; nothing more to do.
    Done,
}

/// Start the app's GPU request once the window exists, and run `ready`
/// with the context once it lands.
///
/// The request runs as a task of the world it will set up, driven by the
/// pump that opens each frame; this behaviour reads the answer on the frames
/// that follow and runs the callback as a queued command with a mutable
/// world, between two event callbacks. A failed request is logged and sets
/// the app's [`ExitRequest`]. A failure callback runs before requesting exit.
pub fn request_gpu<F, E>(app: Entity, ready: F, failed: E) -> OnFrame
where
    F: FnOnce(&mut World, &Gpu) + 'static,
    E: FnOnce(&World, GpuStartError) + 'static,
{
    // The request future runs among the world's own tasks, so the answer
    // slot needs no lock: the future and the behaviour that reads it run on
    // the same thread, and the world's pump drives the future for us.
    let mut answer: Rc<RefCell<Option<GpuRequest>>> = Rc::default();
    let mut requested_window = None;
    let mut stage = RequestStage::WaitingForWindow;
    let mut callbacks = Some((ready, failed));
    OnFrame::with_order(
        FrameBehaviourOrder(REQUEST_GPU_ORDER),
        move |world, _entity, _frame| {
            if stage == RequestStage::Done {
                return;
            }
            let Some(window) = window(world) else {
                return;
            };
            if !world
                .root_scope()
                .query::<&Resumed>()
                .iter()
                .any(|state| state.0)
            {
                return;
            }
            if stage == RequestStage::Requesting {
                if requested_window == Some(window.id()) {
                    let Some(request) = answer.borrow_mut().take() else {
                        return;
                    };
                    let (ready, failed) = callbacks.take().expect("the GPU callbacks run once");
                    stage = RequestStage::Done;
                    world.root_scope().queue().push(RunGpuReady {
                        app,
                        request,
                        ready,
                        failed,
                    });
                    return;
                }
                // A previous request can still resolve, but writes only to
                // its own slot and cannot be used for the replacement window.
                answer = Rc::default();
            }
            let mut display_handle_rows = world.root_scope().query::<&DisplayHandle>();

            let Some(display) = display_handle_rows
                .iter()
                .next()
                .map(|display_handle| display_handle.0.clone())
            else {
                return;
            };
            stage = RequestStage::Requesting;
            requested_window = Some(window.id());
            let slot = Rc::clone(&answer);
            let spawned = LocalTasks::spawn(world, async move {
                // The display handle is owned, not borrowed: the instance
                // it builds has to outlive this callback.
                let instance = wgpu::util::new_instance_with_webgpu_detection(
                    wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)),
                )
                .await;
                let request = match request(instance, window).await {
                    Ok(gpu) => GpuRequest::Ready(gpu),
                    Err(error) => GpuRequest::Failed(error),
                };
                *slot.borrow_mut() = Some(request);
            });
            if !spawned {
                // Nothing would ever have polled that future, so the answer
                // slot would stay empty and the request would wait forever.
                // Report it the way a failed request is reported, rather
                // than leave the app waiting for a renderer that never
                // comes.
                let (ready, failed) = callbacks.take().expect("the GPU callbacks run once");
                stage = RequestStage::Done;
                world.root_scope().queue().push(RunGpuReady {
                    app,
                    request: GpuRequest::Unstartable,
                    ready,
                    failed,
                });
            }
        },
    )
}

/// Run the GPU request's ready callback with a mutable world.
struct RunGpuReady<F, E> {
    /// The app entity the failure exit names.
    app: Entity,
    /// The request's answer.
    request: GpuRequest,
    /// The callback to run on success.
    ready: F,
    failed: E,
}

impl<F, E> Command for RunGpuReady<F, E>
where
    F: FnOnce(&mut World, &Gpu) + 'static,
    E: FnOnce(&World, GpuStartError) + 'static,
{
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let error = match self.request {
            GpuRequest::Ready(gpu) => {
                (self.ready)(world, &gpu);
                return Ok(());
            }
            GpuRequest::Failed(error) => GpuStartError::Request(error),
            GpuRequest::Unstartable => GpuStartError::Unstartable,
        };
        (self.failed)(world, error);
        world
            .root_scope()
            .try_query_one::<&mut ExitRequest>(self.app)?
            .get()
            .0 = true;
        Ok(())
    }
}
