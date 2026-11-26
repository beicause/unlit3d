//! A node's skin and morph weights, read out of the document.
//!
//! The pose is the deformation a mesh's joint stream addresses; the weights
//! are the morph targets' blend factors. Both are per-node state: a model node
//! carries them as [`LitModelSkin`](crate::lit::LitModelSkin) and
//! [`LitModelMorph`](crate::lit::LitModelMorph), and these accessors answer the
//! same questions about the document alone, for a caller inspecting one before
//! building a model.

use super::{LitGltf, NodeId};
use crate::components::SkinPose;
use crate::lit::LitModelSkin;

/// One skin as the document states it: its joints and whatever inverse bind
/// matrices it lists.
///
/// This is the single decoder both readers of a skin take their data through —
/// [`LitGltf::skin_pose`] for the document alone, and
/// [`LitGltf::to_model`](LitGltf::to_model) for the node a model carries — so
/// the two read a document whose matrix count disagrees with its joint count
/// identically. The two lists are as the document states them;
/// [`LitModelSkin::new`](crate::lit::LitModelSkin::new) is what pairs them,
/// and it is the one place the rule for a short or long list lives.
pub(super) struct DecodedSkin {
    /// The document node index of each joint, in joint order.
    pub(super) joints: Vec<usize>,
    /// The inverse bind matrices the skin lists, in joint order.
    pub(super) inverse_bind: Vec<glam::Mat4>,
}

impl LitGltf {
    /// The joints `skin` names and the inverse bind matrices it lists, read
    /// through the document's buffers.
    pub(super) fn decode_skin(&self, skin: &gltf::Skin<'_>) -> DecodedSkin {
        let reader = skin.reader(|buffer| {
            self.buffers
                .get(buffer.index())
                .map(|data| data.0.as_slice())
        });
        let joints: Vec<usize> = skin.joints().map(|joint| joint.index()).collect();
        let inverse_bind: Vec<glam::Mat4> = reader
            .read_inverse_bind_matrices()
            .map(|matrices| {
                matrices
                    .map(|matrix| glam::Mat4::from_cols_array_2d(&matrix))
                    .collect()
            })
            .unwrap_or_default();
        DecodedSkin {
            joints,
            inverse_bind,
        }
    }

    /// The [`SkinPose`] of a node's skin, at the node's rest transform.
    ///
    /// A document carries a skin as a list of joint nodes and one inverse bind
    /// matrix per joint; the pose a renderer wants is a matrix per joint, which
    /// is what this builds:
    ///
    /// ```text
    /// joint matrix = joint node world * inverse bind matrix
    /// ```
    ///
    /// The pose is the *rest* pose — the document's node transforms as loaded,
    /// which is the transform each joint was bound in, so the deformation is
    /// the identity everywhere. A caller that animates joints moves the joint
    /// nodes and rebuilds the pose, or writes the matrices itself; what this
    /// gives is a valid pose to start from and the joint count the mesh's
    /// joint indices address.
    ///
    /// `node` is the node the mesh is attached to, whose skin the pose comes
    /// from. A node with no skin gets an empty pose so a caller can walk a
    /// document without asking first.
    ///
    /// The returned pose has one matrix per joint of the skin, in the order the
    /// mesh's joint indices address them. A skin that states fewer
    /// `inverseBindMatrices` than it has joints binds the joints beyond them
    /// at the identity, so each of those poses by its own world matrix.
    pub fn skin_pose(&self, node: NodeId) -> SkinPose {
        let Some(skin) = self.node(node).and_then(|node| node.skin()) else {
            return SkinPose::default();
        };
        let skin = self.decode_skin(&skin);
        let skin = LitModelSkin::new(skin.joints, skin.inverse_bind);
        // A skinned vertex is produced in world space by the joint matrices
        // alone: the mesh node's transform is inert under this scheme. The
        // caller places the mesh by premultiplying the pose, so the entity
        // transform the shader ignores does not also place it.
        let matrices = skin
            .joints()
            .iter()
            .zip(skin.inverse_bind())
            .map(|(&joint, &bind)| self.node_matrices[joint] * bind)
            .collect();
        SkinPose { matrices }
    }

    /// The number of joints a node's skin has.
    ///
    /// `None` when the node names no skin, so a caller can walk a document
    /// without asking first.
    pub fn skin_joint_count(&self, node: NodeId) -> Option<usize> {
        Some(self.node(node)?.skin()?.joints().count())
    }

    /// The morph weights a node's mesh starts at, one per morph target.
    ///
    /// glTF lets a node override the mesh's own weights, so a node states them
    /// first and the mesh's `weights` is the fallback; a mesh that states
    /// neither starts at zero, which is to say undeformed. The vector is as long
    /// as the target list, so it is always the right length for the
    /// [`MorphWeights`](crate::components::MorphWeights) a spawned mesh binds
    /// to — a document that states fewer weights than it has targets is padded,
    /// and one that states more is truncated, because the renderer rejects a
    /// mismatch.
    ///
    /// Returns an empty vector for a node whose mesh has no morph targets.
    pub fn morph_weights(&self, node: NodeId) -> Vec<f32> {
        let Some(node) = self.node(node) else {
            return Vec::new();
        };
        let Some(mesh) = node.mesh() else {
            return Vec::new();
        };
        let node_weights = node.weights();
        let mut weights = node_weights
            .or_else(|| mesh.weights())
            .map(<[f32]>::to_vec)
            .unwrap_or_default();
        // The counts were worked out once as the document loaded, by the same
        // rule the weights channels are sliced by.
        let targets = self.morph_targets.get(node.index()).copied().unwrap_or(0);
        weights.resize(targets, 0.0);
        weights
    }
}
