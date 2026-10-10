//! winit integration: presenting a renderer, and driving callbacks as behaviours.
//!
//! Three layers, each with one job:
//!
//! - [`surface`] presents a [`Renderer`](crate::renderer::Renderer) into a
//!   window. It owns the swap chain and its configuration; the caller's frame
//!   loop is acquire, bind, render, present.
//! - [`event`] bridges winit's callbacks to the world. Every
//!   [`ApplicationHandler`](winit::application::ApplicationHandler) callback
//!   has a behaviour component family, and [`event::WinitHost`] is the one
//!   `ApplicationHandler` that dispatches to them. It holds no application
//!   logic of its own.
//! - [`builtin`] is the common lifecycle behaviour built on top of the bridge:
//!   create the window on resume, exit on close, feed input, request redraws.
//!
//! A callback is an ordinary behaviour component, so an application composes
//! it the way it composes every other behaviour: spawn it on an entity, and
//! the host drives it. Nothing built in has a path an application cannot take.
//!
//! # A windowed application
//!
//! ```rust,no_run
//! use unlit3d::prelude::*;
//! use unlit3d::winit::builtin::{WindowSpec, create_window_on_resume, exit_on_close_requested};
//! use unlit3d::winit::event::WinitHost;
//! use winit::event_loop::EventLoop;
//! use winit::window::Window;
//!
//! # fn main() -> Result<(), winit::error::EventLoopError> {
//! let event_loop = EventLoop::new()?;
//! let mut host = WinitHost::new();
//! host.world_mut().spawn((
//!     WindowSpec(Window::default_attributes().with_title("unlit3d")),
//!     create_window_on_resume(),
//!     exit_on_close_requested(),
//! ));
//! host.run(event_loop)
//! # }
//! ```

pub mod builtin;
pub mod event;
pub mod surface;

pub use surface::{Frame, WindowSurface};
