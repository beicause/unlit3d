//! Procedural solids and booleans, through [manifold-rust].
//!
//! [manifold-rust]: https://crates.io/crates/manifold-rust
//!
//! A [`Manifold`] is a closed, manifold triangle mesh with a halfedge topology
//! of its own, and the kernel's booleans are exact on that topology: a union,
//! difference or intersection of two solids states a new closed solid rather
//! than stitching two soups together. That is the property procedural geometry
//! needs — a tunnel cut through a block stays watertight, so the result draws
//! with back-face culling and never shows a seam.
//!
//! This module is the seam between that kernel and the lit mesh path: it turns
//! the streams a mesh is described by into a solid, and turns a solid back into
//! the streams [`MeshSource`](crate::mesh_source::MeshSource) uploads. The
//! kernel's own API is re-exported as [`manifold_rust`], so a caller that needs
//! more of it than these two directions reads the crate's documentation rather
//! than a copy of it maintained here.
//!
//! # Usage
//!
//! ```rust,no_run
//! use unlit3d::manifold::{BooleanOp, boolean, from_mesh, streams};
//! use unlit3d::manifold::manifold_rust::linalg::Vec3;
//! use unlit3d::manifold::manifold_rust::manifold::Manifold;
//! # fn run() -> Result<(), unlit3d::manifold::Error> {
//! let block = Manifold::cube(Vec3::new(2.0, 1.0, 2.0), true);
//! let bore = Manifold::sphere(0.75, 48);
//! let bored = boolean(&block, &bore, BooleanOp::Difference)?;
//! let mesh = streams(&bored)?;
//! # Ok(()) }
//! ```
//!
//! `mesh` is the [`OwnedLitMeshDesc`] handed to
//! [`allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh).
//!
//! To bring in geometry that was generated as streams rather than by the
//! kernel's own constructors, use [`from_mesh`] instead of starting from a
//! primitive.
//!
//! # Channels
//!
//! Only positions and indices cross this boundary. UVs, colours, normals, skin
//! and morph channels are refused with [`Error::UnsupportedChannel`] rather
//! than dropped, because a boolean rewrites the vertex set: an attribute the
//! caller supplied would come back either silently missing or, worse,
//! interpolated across a cut where it means nothing.
//!
//! UVs are the case worth naming, since a procedural mesh usually wants them.
//! The kernel has no notion of UV — its extra property slots are a flat array
//! the caller numbers — and its booleans interpolate whatever those slots hold
//! across the new triangles, so a UV seam becomes a smear. A procedural mesh
//! almost always derives UVs from the vertex position instead (a box, cylinder
//! or triplanar projection), and that projection is cheaper and more
//! predictable to run after the boolean than to carry through it. Assign them
//! to the returned [`OwnedLitMeshDesc::uvs`]
//! at that point.
//!
//! Normals follow the same rule, with one addition: a caller that wants shading
//! normals calls [`Manifold::calculate_normals`] with a crease angle before
//! [`streams`], and gets them back as [`OwnedLitMeshDesc::normals`]. Whether
//! that is worth doing depends on the look — see [`from_mesh`] for what
//! leaving the channel empty gives instead.
//!
//! # Cost
//!
//! [`from_mesh`] is zero-copy: the positions move into the kernel's array and
//! the indices move next to them, so building a solid from a
//! [`OwnedLitMeshDesc`] allocates nothing. [`streams`] does allocate, because
//! the kernel interleaves its output — one row per vertex-and-property pair —
//! and a mesh upload wants each channel flat.
//!
//! Booleans run on the CPU, synchronously, over the whole mesh. They are a
//! build-time operation: a solid belongs in the assembly that produces a
//! [`GpuMesh`](crate::components::GpuMesh), not in a frame.
//!
//! # Determinism
//!
//! The kernel's parallel feature is left off, so a boolean is single-threaded
//! and bit-for-bit reproducible on one target. Snapshots of procedural geometry
//! therefore hold the same way hand-authored geometry does. Results are not
//! promised to match across architectures or across kernel versions.

use crate::lit::OwnedLitMeshDesc;

pub use manifold_rust;
pub use manifold_rust::manifold::Manifold;

mod boolean;
mod convert;
mod error;
pub use self::boolean::{BooleanOp, boolean};
pub use self::convert::{from_mesh, from_mesh_robust, streams};
pub use self::error::Error;

#[cfg(test)]
mod tests;

/// The channels a mesh description carried that a solid has no room for.
///
/// Shared by the conversion to report which channel it refused; the name is the
/// field's, so the message names the thing the caller set.
pub(crate) const fn channel_of(desc: &OwnedLitMeshDesc) -> Option<&'static str> {
    if desc.uvs.is_some() {
        Some("uvs")
    } else if desc.colors.is_some() {
        Some("colors")
    } else if desc.normals.is_some() {
        Some("normals")
    } else if desc.joints.is_some() || desc.weights.is_some() {
        Some("joints")
    } else if desc.morph_deltas.is_some() {
        Some("morph_deltas")
    } else {
        None
    }
}
