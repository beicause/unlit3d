//! Combining two solids into one.

use manifold_rust::manifold::Manifold;

use crate::manifold::Error;

/// Which of the two set operations to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BooleanOp {
    /// The volume either solid fills.
    Union,
    /// The volume the first fills and the second does not — the operation that
    /// cuts a bore or a notch.
    Difference,
    /// The volume both solids fill.
    Intersection,
}

/// Combine two solids with the set operation `op` names.
///
/// Both inputs are closed solids, and the result is one: a difference leaves the
/// cut surface closed and consistently wound, so the two pieces a caller wants
/// apart are separated by an actual cut rather than by a hole with an inside.
///
/// The kernel answers an empty solid instead of reporting a failure, so this
/// reports the two cases a caller has to tell apart: an input with nothing in
/// it ([`Error::EmptyInput`]) is a mistake to fix, while a result with nothing
/// in it ([`Error::EmptyResult`]) is a legitimate answer — two solids that do
/// not meet intersect into nothing.
///
/// # Errors
///
/// [`Error::EmptyInput`] when either solid holds no volume,
/// [`Error::Rejected`] when the kernel cannot decide the operation, and
/// [`Error::EmptyResult`] when the answer holds no triangles.
pub fn boolean(a: &Manifold, b: &Manifold, op: BooleanOp) -> Result<Manifold, Error> {
    if a.is_empty() || b.is_empty() {
        return Err(Error::EmptyInput);
    }
    let result = match op {
        BooleanOp::Union => a.union(b),
        BooleanOp::Difference => a.difference(b),
        BooleanOp::Intersection => a.intersection(b),
    };
    match result.status() {
        manifold_rust::types::Error::NoError if result.is_empty() => Err(Error::EmptyResult),
        manifold_rust::types::Error::NoError => Ok(result),
        status => Err(Error::Rejected { status }),
    }
}
