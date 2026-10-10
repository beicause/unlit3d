//! The bridge between winit's callbacks and the world.
//!
//! Every [`ApplicationHandler`] callback has a behaviour component family of
//! its own, and [`WinitHost`] is the one `ApplicationHandler` that dispatches
//! to them. A family is an ordinary behaviour component: it holds a closure and
//! is driven by the host, exactly like the input callbacks are driven by
//! [`dispatch_input`](crate::input::dispatch_input).
//!
//! The callback signature deliberately carries no [`ActiveEventLoop`]. The loop
//! is only reachable inside a callback, so a component that took it could never
//! be called from a test — winit offers no way to build one. Instead the host
//! turns loop actions into request components: a behaviour writes
//! [`CreateWindowRequest`] or [`ExitRequest`] into the world, and the host reads
//! and executes it after the whole dispatch. This keeps every callback
//! unit-testable and gives a built-in behaviour no path an application cannot
//! take.
//!
//! # Borrow rules
//!
//! The same rules as the input callbacks apply:
//!
//! - A callback runs with `&World`. It must not re-enter the host, which would
//!   dispatch recursively.
//! - A callback must not reach for its own component family: that is a borrow
//!   panic, not a compile error. State that outlives one event belongs in a
//!   sibling component.
//! - Structural changes go through [`World::queue`] and land when the host
//!   applies the queue after the dispatch. Use the entity [`Commands::spawn`]
//!   returns to refer to an entity that does not exist yet.
//! - Different families on one entity are different cells and never conflict;
//!   two behaviours of the *same* family on one entity do. Put them on two
//!   entities.
//!
//! [`Commands::spawn`]: unlit_ecs::Commands::spawn

use std::marker::PhantomData;
use std::sync::Arc;

use unlit_ecs::{Entity, World};
use winit::application::ApplicationHandler;
use winit::error::EventLoopError;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{WindowAttributes, WindowId};

use crate::behaviour::{EventCallback, behaviour};
use crate::input::distribute_input;

use super::builtin::{CreateWindowRequest, DisplayHandle, ExitRequest, Resumed, WinitWindow};

/// A winit callback: a boxed closure the host drives with one event.
pub type WinitCallback<E> = EventCallback<E>;

behaviour!(
    OnNewEvents,
    StartCause,
    "Runs when the event loop begins a batch of events."
);
behaviour!(
    OnResumed,
    (),
    "Runs when the window system resumes the application."
);
behaviour!(
    OnSuspended,
    (),
    "Runs when the window system suspends the application."
);
behaviour!(
    OnWindowEvent,
    WinitWindowEvent,
    "Runs on every window event."
);
behaviour!(
    OnDeviceEvent,
    WinitDeviceEvent,
    "Runs on every device event."
);
behaviour!(
    OnAboutToWait,
    (),
    "Runs when the event loop is about to wait for events."
);
behaviour!(OnExiting, (), "Runs when the event loop is about to exit.");
behaviour!(
    OnMemoryWarning,
    (),
    "Runs when the system reports a memory warning."
);

/// The callback an [`OnUserEvent`] holds.
///
/// A user event is the application's own type and often carries something that
/// cannot be copied — an MCP command, a value from another thread — so the
/// event is handed to the callback mutably and the callback may take what it
/// needs out of it.
pub type WinitUserCallback<U> = Box<dyn FnMut(&World, Entity, &mut U)>;

/// Runs when the event loop receives a user event.
///
/// The event type is the generic parameter of the [`EventLoop`], so this one
/// cannot come from the `behaviour!` macro.
pub struct OnUserEvent<U: 'static>(pub WinitUserCallback<U>);

impl<U: 'static> OnUserEvent<U> {
    /// Wrap `f` as an [`OnUserEvent`].
    pub fn new(f: impl FnMut(&World, Entity, &mut U) + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Run this behaviour for `entity` with `event`.
    pub fn run(&mut self, world: &World, entity: Entity, event: &mut U) {
        (self.0)(world, entity, event);
    }
}

/// One window event, together with the window it came from.
pub struct WinitWindowEvent {
    /// The window the event belongs to.
    pub window_id: WindowId,
    /// The event itself.
    pub event: WindowEvent,
}

/// One device event, together with the device it came from.
pub struct WinitDeviceEvent {
    /// The device the event belongs to.
    pub device_id: DeviceId,
    /// The event itself.
    pub event: DeviceEvent,
}

/// Drives every winit callback behaviour in one world.
///
/// The host owns the world and is the single [`ApplicationHandler`]; it holds no
/// application logic of its own. Build the world, spawn the behaviours, then
/// hand the host to [`WinitHost::run`].
///
/// ```no_run
/// use unlit3d::winit::builtin::{WindowSpec, create_window_on_resume, exit_on_close_requested};
/// use unlit3d::winit::event::WinitHost;
/// use winit::event_loop::EventLoop;
/// use winit::window::Window;
///
/// # fn main() -> Result<(), winit::error::EventLoopError> {
/// let event_loop = EventLoop::new()?;
/// let mut host = WinitHost::new();
/// host.world_mut().spawn((
///     WindowSpec(Window::default_attributes().with_title("unlit3d")),
///     create_window_on_resume(),
///     exit_on_close_requested(),
/// ));
/// host.run(event_loop)
/// # }
/// ```
pub struct WinitHost<U: 'static = ()> {
    world: World,
    /// The host only dispatches; the user event type appears in the world, not
    /// in a field, so it is held by a marker that keeps the host `Send`.
    marker: PhantomData<fn() -> U>,
}

impl<U: 'static> WinitHost<U> {
    /// An empty host over an empty world.
    pub fn new() -> Self {
        Self {
            world: World::new(),
            marker: PhantomData,
        }
    }

    /// The world the behaviours live in.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The world the behaviours live in, mutably.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Run the event loop, driving the world's behaviours until it exits.
    ///
    /// This writes the host's own state — [`Resumed`] and [`DisplayHandle`] —
    /// into the world before the first callback. Everything else the host does
    /// is dispatch.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run(mut self, event_loop: EventLoop<U>) -> Result<(), EventLoopError> {
        self.prepare(&event_loop);
        event_loop.run_app(&mut self)
    }

    /// Run the event loop, driving the world's behaviours until it exits.
    ///
    /// This writes the host's own state — [`Resumed`] and [`DisplayHandle`] —
    /// into the world before the first callback. Everything else the host does
    /// is dispatch.
    ///
    /// On the web the event loop is spawned and this returns immediately; the
    /// `Ok(())` is nominal, because no run has finished yet.
    #[cfg(target_arch = "wasm32")]
    pub fn run(mut self, event_loop: EventLoop<U>) -> Result<(), EventLoopError> {
        use winit::platform::web::EventLoopExtWebSys;

        self.prepare(&event_loop);
        event_loop.spawn_app(self);
        Ok(())
    }

    /// Write the state every callback may read before the loop starts.
    fn prepare(&mut self, event_loop: &EventLoop<U>) {
        let display = event_loop.owned_display_handle();
        self.world.spawn((Resumed(false), DisplayHandle(display)));
    }

    /// Set the host's [`Resumed`] state.
    fn set_resumed(&mut self, resumed: bool) {
        let Some(entity) = self
            .world
            .query::<&Resumed>()
            .next()
            .map(|(entity, _)| entity)
        else {
            return;
        };
        let _ = self
            .world
            .with_mut::<Resumed, _>(entity, |state| state.0 = resumed);
    }

    /// The actions that follow a dispatch, in a fixed order.
    ///
    /// 1. Apply the queue, so a structural change a callback queued lands.
    /// 2. Execute the window and exit requests.
    /// 3. Lay the input handle into every nested world.
    fn settle(&mut self, event_loop: &ActiveEventLoop) {
        self.world.apply();
        self.create_windows(event_loop);
        self.exit_requested(event_loop);
        distribute_input(&self.world);
    }

    /// Create the window of every pending [`CreateWindowRequest`].
    ///
    /// The window lands on a fresh entity rather than the one that requested
    /// it: a component cannot be added to an entity that is already alive, and
    /// the request's own entity holds the behaviour. [`window`] finds it.
    ///
    /// [`window`]: super::builtin::window
    fn create_windows(&mut self, event_loop: &ActiveEventLoop) {
        let requests: Vec<WindowAttributes> = self
            .world
            .query::<&mut CreateWindowRequest>()
            .filter_map(|(_, mut request)| request.0.take())
            .collect();
        for attributes in requests {
            match event_loop.create_window(attributes) {
                Ok(window) => {
                    self.world.spawn((WinitWindow(Arc::new(window)),));
                }
                Err(error) => {
                    log::error!("could not create the window: {error}");
                    event_loop.exit();
                }
            }
        }
    }

    /// Exit the event loop when any [`ExitRequest`] is set.
    fn exit_requested(&mut self, event_loop: &ActiveEventLoop) {
        let mut exit = false;
        for (_entity, mut request) in self.world.query::<&mut ExitRequest>() {
            if request.0 {
                request.0 = false;
                exit = true;
            }
        }
        if exit {
            event_loop.exit();
        }
    }
}

impl<U: 'static> Default for WinitHost<U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U: 'static> ApplicationHandler<U> for WinitHost<U> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        for (entity, mut behaviour) in self.world.query::<&mut OnNewEvents>() {
            behaviour.run(&self.world, entity, &cause);
        }
        self.settle(event_loop);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.set_resumed(true);
        for (entity, mut behaviour) in self.world.query::<&mut OnResumed>() {
            behaviour.run(&self.world, entity, &());
        }
        self.settle(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        for (entity, mut behaviour) in self.world.query::<&mut OnSuspended>() {
            behaviour.run(&self.world, entity, &());
        }
        self.settle(event_loop);
        self.set_resumed(false);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, mut event: U) {
        for (entity, mut behaviour) in self.world.query::<&mut OnUserEvent<U>>() {
            behaviour.run(&self.world, entity, &mut event);
        }
        self.settle(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let event = WinitWindowEvent { window_id, event };
        for (entity, mut behaviour) in self.world.query::<&mut OnWindowEvent>() {
            behaviour.run(&self.world, entity, &event);
        }
        self.settle(event_loop);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: DeviceId,
        event: DeviceEvent,
    ) {
        let event = WinitDeviceEvent { device_id, event };
        for (entity, mut behaviour) in self.world.query::<&mut OnDeviceEvent>() {
            behaviour.run(&self.world, entity, &event);
        }
        self.settle(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        for (entity, mut behaviour) in self.world.query::<&mut OnAboutToWait>() {
            behaviour.run(&self.world, entity, &());
        }
        self.settle(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        for (entity, mut behaviour) in self.world.query::<&mut OnExiting>() {
            behaviour.run(&self.world, entity, &());
        }
        self.settle(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        for (entity, mut behaviour) in self.world.query::<&mut OnMemoryWarning>() {
            behaviour.run(&self.world, entity, &());
        }
        self.settle(event_loop);
    }
}
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::winit::builtin::Resumed;
    use winit::window::WindowId;

    #[test]
    fn set_resumed_writes_the_host_state() {
        let mut host = WinitHost::<()>::new();
        host.world_mut().spawn((Resumed(false),));
        host.set_resumed(true);
        assert_eq!(
            host.world()
                .query::<&Resumed>()
                .next()
                .map(|(_, state)| state.0),
            Some(true)
        );
    }

    #[test]
    fn every_behaviour_of_a_family_runs_with_the_payload() {
        let world = World::new();
        let seen = Rc::new(RefCell::new(0u32));
        let payload = WinitWindowEvent {
            window_id: WindowId::dummy(),
            event: WindowEvent::Focused(true),
        };
        let first = OnWindowEvent::new({
            let seen = seen.clone();
            move |_world, _entity, event| {
                assert!(matches!(event.event, WindowEvent::Focused(true)));
                *seen.borrow_mut() += 1;
            }
        });
        let second = OnWindowEvent::new({
            let seen = seen.clone();
            move |_world, _entity, _event| *seen.borrow_mut() += 1
        });
        for mut behaviour in [first, second] {
            behaviour.run(&world, Entity::from_bits(0), &payload);
        }
        assert_eq!(*seen.borrow(), 2);
    }

    #[test]
    fn a_user_event_carries_its_own_type() {
        let world = World::new();
        let seen = Rc::new(RefCell::new(None));
        let mut behaviour = OnUserEvent::<u32>::new({
            let seen = seen.clone();
            move |_world, _entity, event| *seen.borrow_mut() = Some(*event)
        });
        behaviour.run(&world, Entity::from_bits(0), &mut 7);
        assert_eq!(*seen.borrow(), Some(7));
    }
}
