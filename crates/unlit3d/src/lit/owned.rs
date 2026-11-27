//! A mesh description that owns its channels.
//!
//! [`LitMeshDesc`] borrows every channel, which is what the allocator wants but
//! not what geometry that was decoded, generated or carried across a thread
//! has to hand. This struct owns the same channels: [`OwnedLitMeshDesc::borrowed`]
//! lends them for the upload, and a producer fills one channel set rather than
//! inventing a representation of its own.

use crate::lit::LitMeshDesc;
use crate::mesh::MorphDeltas;

/// Every vertex stream a lit mesh can upload, owned by the description.
///
/// `positions` is the only channel a mesh cannot do without; the rest are
/// absent when the source does not state them, so a missing channel never
/// changes the pipeline the draw picks.
#[derive(Clone, Debug, Default)]
pub struct OwnedLitMeshDesc {
    /// One entry per vertex; every other channel is indexed by the same order.
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates, sampled only by a draw that binds a material.
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Straight (non-premultiplied) sRGB-alpha colours.
    pub colors: Option<Vec<[u8; 4]>>,
    /// Per-vertex normals; without them the lit pipeline infers a geometric
    /// normal per triangle from screen-space derivatives.
    pub normals: Option<Vec<[f32; 3]>>,
    /// Triangle indices into `positions`; without them the vertices draw in the
    /// order they appear, three to a primitive.
    pub indices: Option<Vec<u32>>,
    /// Joint indices per vertex, always paired with `weights`.
    pub joints: Option<Vec<[u16; 4]>>,
    /// Skin weights per vertex, always paired with `joints` and equal in length.
    pub weights: Option<Vec<[f32; 4]>>,
    /// Morph-target displacements, flattened as `vertex * targets * 3`.
    pub morph_deltas: Option<MorphDeltas>,
}

impl OwnedLitMeshDesc {
    /// The borrowed view the upload takes.
    ///
    /// The morph displacements are moved out rather than lent, because
    /// [`LitMeshDesc`] owns them and a description is spent on one upload
    /// anyway.
    pub fn borrowed(&mut self) -> LitMeshDesc<'_> {
        let morph_deltas = self.morph_deltas.take();
        LitMeshDesc {
            positions: &self.positions,
            uvs: self.uvs.as_deref(),
            colors: self.colors.as_deref(),
            normals: self.normals.as_deref(),
            indices: self.indices.as_deref(),
            joints: self.joints.as_deref(),
            weights: self.weights.as_deref(),
            morph_deltas,
        }
    }
}
