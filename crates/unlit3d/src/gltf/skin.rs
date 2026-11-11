//! A node's skin and morph weights, read out of the document.
//!
//! The pose is the deformation a mesh's joint stream addresses; the weights
//! are the morph targets' blend factors. Both are per-node state, so a caller
//! spawns one of each per node and rewrites them while animating.

use super::material::morph_target_count;
use super::{LitGltf, NodeId};
use crate::components::SkinPose;

impl LitGltf {
    /// The [`SkinPose`] of a node's skin, at the node's rest transform.
    ///
    /// A document carries a skin as a list of joint nodes and one inverse bind
    /// matrix per joint; the pose a renderer wants is a matrix per joint, which
    /// is what this builds:
    ///
    /// ```text
    /// joint matrix = inverse(mesh node world) * joint node world * inverse bind matrix
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
    /// from. A node with no skin, or a skin with no `inverseBindMatrices`, gets
    /// matrices that deform nothing: the identity per joint, which is what a
    /// skin bound at rest means.
    ///
    /// The returned pose has one matrix per joint of the skin, in the order the
    /// mesh's joint indices address them.
    pub fn skin_pose(&self, node: NodeId) -> SkinPose {
        self.skin_pose_with_matrices(node.index(), &self.node_matrices)
    }

    /// The [`SkinPose`] a node's skin deforms by, evaluated against a given
    /// set of node world matrices.
    ///
    /// [`Self::skin_pose`] evaluates the rest pose; [`Self::apply_animation`]
    /// evaluates one where the joints have been animated.
    pub(super) fn skin_pose_with_matrices(&self, node: usize, matrices: &[glam::Mat4]) -> SkinPose {
        let node_data = self.node(node);
        let Some(skin) = node_data.skin() else {
            return SkinPose::default();
        };
        let reader = skin.reader(|buffer| Some(&self.buffers[buffer.index()]));
        let inverse_bind: Option<Vec<glam::Mat4>> = reader.read_inverse_bind_matrices().map(|m| {
            m.map(|matrix| glam::Mat4::from_cols_array_2d(&matrix))
                .collect()
        });
        // A skinned mesh is deformed in its own space, so the mesh node's world
        // matrix has to come back out of the joint's: a joint's inverse bind
        // matrix already encodes the joint's rest transform, and the mesh node
        // moves the whole skinned result.
        let world_to_mesh = matrices[node].inverse();
        let poses: Vec<glam::Mat4> = skin
            .joints()
            .enumerate()
            .map(|(joint, joint_node)| {
                let joint_world = matrices[joint_node.index()];
                let bind = inverse_bind.as_ref().map_or(glam::Mat4::IDENTITY, |ibm| {
                    *ibm.get(joint).expect("one inverse bind matrix per joint")
                });
                world_to_mesh * joint_world * bind
            })
            .collect();
        SkinPose { matrices: poses }
    }

    /// The number of joints a node's skin has.
    ///
    /// `None` when the node names no skin, so a caller can walk a document
    /// without asking first.
    pub fn skin_joint_count(&self, node: NodeId) -> Option<usize> {
        Some(self.node(node.index()).skin()?.joints().count())
    }

    /// The morph weights a node's mesh starts at, one per target that
    /// displaces positions.
    ///
    /// glTF lets a node override the mesh's own weights, so a node states them
    /// first and the mesh's `weights` is the fallback; a mesh that states
    /// neither starts at zero, which is to say undeformed. The vector is as
    /// long as the deforming target list, so it is always the right length for
    /// the [`MorphWeights`](crate::components::MorphWeights) a spawned mesh
    /// binds to — a document that states
    /// fewer weights than it has targets is padded, and one that states more is
    /// truncated, because the renderer rejects a mismatch.
    ///
    /// Returns an empty vector for a node whose mesh has no positional morph
    /// targets.
    pub fn morph_weights(&self, node: NodeId) -> Vec<f32> {
        let Some(mesh) = self.node(node.index()).mesh() else {
            return Vec::new();
        };
        let node_weights = self.node(node.index()).weights();
        let mut weights = node_weights
            .or_else(|| mesh.weights())
            .map(<[f32]>::to_vec)
            .unwrap_or_default();
        let count = mesh
            .primitives()
            .map(|primitive| morph_target_count(&primitive))
            .max()
            .unwrap_or(0);
        weights.resize(count, 0.0);
        weights
    }
}
