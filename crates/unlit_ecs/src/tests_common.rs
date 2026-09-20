//! Fixtures shared by the unit tests.

#![cfg(test)]

/// A component with no meaning beyond being a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker(pub u32);

/// A component holding a string, for identity checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name(pub &'static str);
