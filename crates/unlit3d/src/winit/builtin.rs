//! The winit integration unlit3d ships.
//!
//! Everything here is an ordinary component. The behaviours are constructor
//! functions that return the matching callback family, so a built-in has no
//! dispatch path an application cannot take: it is driven by the same
//! [`query`](unlit_ecs::World::query) the host runs over any other behaviour.
//!
//! State the host owns lives in plain components ([`Resumed`],
//! [`DisplayHandle`]); a behaviour that wants the host to act writes a request
//! component ([`CreateWindowRequest`], [`ExitRequest`]) that the host executes
//! after the dispatch.
//!
//! Two behaviours of the same family must sit on two entities, because a
//! behaviour is borrowed while it runs. Two behaviours of different families
//! never conflict.

use std::sync::Arc;

use unlit_ecs::World;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{EventLoopProxy, OwnedDisplayHandle};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes};

use crate::input::winit::WinitInput;

use super::event::{OnAboutToWait, OnResumed, OnWindowEvent};

/// The window attributes [`create_window_on_resume`] builds the window from.
#[derive(Clone)]
pub struct WindowSpec(pub WindowAttributes);

/// The window the host created.
///
/// It sits on its own entity, separate from the behaviour that asked for it.
pub struct WinitWindow(pub Arc<Window>);

/// The one window of the world, if the host has created it.
///
/// A world with more than one window is not modelled yet; this returns the
/// first, in query order.
pub fn window(world: &World) -> Option<Arc<Window>> {
    world
        .query::<&WinitWindow>()
        .next()
        .map(|(_, window)| window.0.clone())
}

/// Asks the host to create the window in this component.
///
/// The behaviour writes the attributes here; the host reads them after the
/// dispatch and clears the slot, so the same component is ready for the next
/// request. A window lands on a fresh entity, not on this one.
pub struct CreateWindowRequest(pub Option<WindowAttributes>);

/// Asks the host to exit the event loop.
///
/// The host clears the flag once it has acted, so a behaviour may set it again
/// later.
pub struct ExitRequest(pub bool);

/// Whether the window system has resumed the application.
///
/// The host maintains it: `true` between [`resumed`](winit::application::ApplicationHandler::resumed)
/// and [`suspended`](winit::application::ApplicationHandler::suspended).
pub struct Resumed(pub bool);

/// The display handle of the event loop.
///
/// The host writes it before the first callback, so a behaviour can request a
/// second window or another handle-dependent resource.
pub struct DisplayHandle(pub OwnedDisplayHandle);

/// The event loop proxy, for a component that has to send a user event.
///
/// A component that talks to another thread carries one; the proxy is `Clone`.
pub struct WinitProxy<U: 'static>(pub EventLoopProxy<U>);

/// Create the window on the first resume.
///
/// Idempotent: it returns when the world already has a window, so winit's
/// back-to-back `resumed` calls create it only once. Otherwise it copies this
/// entity's [`WindowSpec`] into this entity's [`CreateWindowRequest`]; the host
/// creates the window after the dispatch.
///
/// The entity must carry both a [`WindowSpec`] and a [`CreateWindowRequest`].
pub fn create_window_on_resume() -> OnResumed {
    OnResumed::new(|world, entity, ()| {
        if window(world).is_some() {
            return;
        }
        let Some(attributes) = world.get::<WindowSpec>(entity).map(|spec| spec.0.clone()) else {
            log::warn!(
                "create_window_on_resume: the entity carries no WindowSpec, so no window is created"
            );
            return;
        };
        let _ = world
            .with_mut::<CreateWindowRequest, _>(entity, |request| request.0 = Some(attributes));
    })
}

/// Exit when the window is closed.
pub fn exit_on_close_requested() -> OnWindowEvent {
    OnWindowEvent::new(|world, entity, payload| {
        if matches!(payload.event, WindowEvent::CloseRequested) {
            let _ = world.with_mut::<ExitRequest, _>(entity, |request| request.0 = true);
        }
    })
}

/// Exit when Escape is pressed.
///
/// A repeat is ignored, so holding the key down does not fire twice.
pub fn exit_on_escape() -> OnWindowEvent {
    OnWindowEvent::new(|world, entity, payload| {
        if let WindowEvent::KeyboardInput { event, .. } = &payload.event
            && event.state == ElementState::Pressed
            && !event.repeat
            && event.physical_key == PhysicalKey::Code(KeyCode::Escape)
        {
            let _ = world.with_mut::<ExitRequest, _>(entity, |request| request.0 = true);
        }
    })
}

/// Request a redraw every turn while the application is resumed.
///
/// "Foreground" is approximated by the host's [`Resumed`] state. An
/// application with a finer policy writes its own [`OnAboutToWait`].
pub fn request_redraw_while_foreground() -> OnAboutToWait {
    OnAboutToWait::new(|world, _entity, ()| {
        let resumed = world
            .query::<&Resumed>()
            .next()
            .is_some_and(|(_, state)| state.0);
        if resumed && let Some(window) = window(world) {
            window.request_redraw();
        }
    })
}

/// Feed every window event into a [`WinitInput`].
///
/// The adapter translates the event and pushes it into the shared
/// [`InputHandle`](crate::input::InputHandle); dispatch it with
/// [`dispatch_input`](crate::input::dispatch_input).
pub fn winit_input_behaviour(input: WinitInput) -> OnWindowEvent {
    OnWindowEvent::new(move |_world, _entity, payload| {
        input.on_window_event(&payload.event);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::winit::event::WinitWindowEvent;
    use winit::window::WindowId;

    fn window_event(event: WindowEvent) -> WinitWindowEvent {
        WinitWindowEvent {
            window_id: WindowId::dummy(),
            event,
        }
    }

    #[test]
    fn exit_on_close_requested_asks_the_host_to_exit() {
        let mut world = World::new();
        let entity = world.spawn((ExitRequest(false),));
        world.apply();
        let mut behaviour = exit_on_close_requested();
        behaviour.run(&world, entity, &window_event(WindowEvent::CloseRequested));
        assert_eq!(
            world.with_mut::<ExitRequest, _>(entity, |request| request.0),
            Some(true)
        );
    }

    #[test]
    fn exit_on_close_requested_ignores_other_events() {
        let mut world = World::new();
        let entity = world.spawn((ExitRequest(false),));
        world.apply();
        let mut behaviour = exit_on_close_requested();
        behaviour.run(&world, entity, &window_event(WindowEvent::Focused(true)));
        assert_eq!(
            world.with_mut::<ExitRequest, _>(entity, |request| request.0),
            Some(false)
        );
    }

    #[test]
    fn create_window_on_resume_copies_the_spec_into_the_request() {
        let mut world = World::new();
        let spec = WindowSpec(Window::default_attributes().with_title("unlit3d"));
        let entity = world.spawn((spec, CreateWindowRequest(None), create_window_on_resume()));
        world.apply();
        let mut behaviour = world
            .get_mut::<OnResumed>(entity)
            .expect("the behaviour is there");
        behaviour.run(&world, entity, &());
        let attributes = world
            .with_mut::<CreateWindowRequest, _>(entity, |request| request.0.take())
            .flatten()
            .expect("the request is filled");
        assert_eq!(attributes.title, "unlit3d");
    }

    #[test]
    fn request_redraw_while_foreground_needs_the_resumed_state() {
        let mut world = World::new();
        let entity = world.spawn((Resumed(false), request_redraw_while_foreground()));
        world.apply();
        let mut behaviour = world
            .get_mut::<OnAboutToWait>(entity)
            .expect("the behaviour is there");
        behaviour.run(&world, entity, &());
    }
}
