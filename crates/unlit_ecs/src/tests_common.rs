//! Fixtures shared by the unit tests.

#![cfg(test)]

/// A component with no meaning beyond being a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker(pub u32);

/// A component to add and remove at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Addable(pub u32);

impl crate::AddableComponent for Addable {}

/// A component holding a string, for identity checks across archetype moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name(pub &'static str);
