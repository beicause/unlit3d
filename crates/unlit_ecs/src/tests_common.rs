//! Fixtures shared by the unit tests.

#![cfg(test)]

/// A component with no meaning beyond being a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker(pub u32);

/// A component holding a string, for identity checks.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(dead_code, reason = "only the unit tests that need it construct one")]
pub struct Name(pub &'static str);
