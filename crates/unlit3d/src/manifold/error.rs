//! Everything that stops a solid from being built, combined or read back.

use manifold_rust::types::Error as KernelError;

/// What a conversion between the mesh path and the CSG kernel can fail on.
///
/// The kernel reports a refused construction by handing back an empty solid
/// and an [`Error`](manifold_rust::types::Error) of its own, which stops short
/// of implementing [`std::error::Error`]; [`Self::Rejected`] carries that value
/// so the caller sees the kernel's own diagnosis rather than a paraphrase.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The mesh stated no vertices, so there is no solid to build.
    #[error("the mesh states no vertices")]
    EmptyInput,
    /// A boolean answered no solid at all.
    ///
    /// Separate from [`Self::Rejected`] because nothing went wrong: two solids
    /// that do not meet intersect into nothing, and the caller can tell that
    /// apart from a kernel that could not decide.
    #[error("the operation answered no solid")]
    EmptyResult,
    /// A non-indexed mesh stated a vertex count that is not a multiple of
    /// three, so it names no whole triangles.
    #[error("a non-indexed mesh states {count} vertices, which is not a whole number of triangles")]
    RaggedIndices {
        /// The vertex count the mesh stated.
        count: usize,
    },
    /// The mesh carried a channel a solid cannot hold.
    ///
    /// See the [module documentation](crate::manifold) for why attributes do
    /// not cross this boundary.
    #[error("the mesh carries {channel}, which a solid cannot hold")]
    UnsupportedChannel {
        /// The name of the description field that was set.
        channel: &'static str,
    },
    /// The solid carries an extra property layout this module does not
    /// recognise.
    ///
    /// A solid that went through [`Manifold::set_properties`] or
    /// [`Manifold::calculate_normals`] at a non-zero slot has an arbitrary
    /// number of extra properties, which this module cannot tell apart from
    /// normals. Three is positions alone and six is positions then normals;
    /// anything else is the caller's own layout to unpack.
    ///
    /// [`Manifold::set_properties`]: manifold_rust::manifold::Manifold::set_properties
    /// [`Manifold::calculate_normals`]: manifold_rust::manifold::Manifold::calculate_normals
    #[error(
        "the solid carries {num_prop} properties per vertex, which is not positions alone or positions then normals"
    )]
    UnknownChannels {
        /// The property count the exported mesh stated.
        num_prop: usize,
    },
    /// The solid holds a coordinate that is not finite.
    #[error("the solid holds a coordinate that is not finite")]
    NonFiniteVertex,
    /// The kernel refused the construction, naming its own reason.
    #[error("the kernel refused the solid: {status}")]
    Rejected {
        /// The kernel's diagnosis.
        status: KernelError,
    },
}
