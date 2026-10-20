//! Behaviour components: components that hold a list of callbacks.
//!
//! A behaviour component is the library's replacement for a system: a component
//! that holds closures and is invoked by whatever driver the caller writes.
//! [`CallbackList`] is the shape every family shares, which is what lets the
//! world run a callback out of its component with [`crate::World`].

/// A component holding a list of callbacks that a driver runs one at a time.
///
/// The list itself is a public field on every implementor, because a driver that
/// resolves an order across entities reads it as well; this trait is only what a
/// driver that does not care about the order needs. Implemented by the lifecycle
/// components of this crate and by every behaviour family of the crates built on
/// it.
pub trait CallbackList: 'static {
    /// One callback of this component.
    type Callback;

    /// Every callback, in the order they run.
    fn callbacks(&self) -> &[Self::Callback];

    /// Every callback, in the order they run, exclusively.
    fn callbacks_mut(&mut self) -> &mut Vec<Self::Callback>;
}
