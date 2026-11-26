//! Convert a loaded glTF document into the generic lit model representation.

use super::playback::duration;
use super::{AnimationId, GltfResources, LitGltf};
use crate::lit::{LitModel, LitModelClip, LitModelMorph, LitModelNode, LitModelSkin};

/// Remap a skin's joints into model node indices.
///
/// Building the included set added every joint, and each joint's ancestry with
/// it, so every cell here is filled; a document whose joint list changed after
/// that pass would leave one empty, and the error names it rather than
/// asserting a fact the caller cannot check.
fn remapped_joints(
    joints: impl Iterator<Item = Option<usize>>,
    node: super::NodeId,
) -> Result<Vec<usize>, super::Error> {
    joints
        .enumerate()
        .map(|(joint, cell)| cell.ok_or(super::Error::JointOutsideModel { node, joint }))
        .collect()
}

impl LitGltf {
    /// Prepare the default scene as a reusable model asset with no playback state.
    /// Only the scene and any joint dependencies become model nodes. Animation
    /// channels are remapped to those nodes; clip indices stay unchanged.
    ///
    /// A skin keeps the joints the document leaves outside the default scene:
    /// an animation that drives a skeleton has to find every joint of it, so
    /// the skeleton comes along whether or not the scene reaches it.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no default scene, a drawn primitive is
    /// missing its uploaded mesh or material, or the document decodes into
    /// model data [`LitModel::new`] refuses — see [`super::Error::Model`].
    pub fn to_model(&self, resources: &GltfResources) -> Result<LitModel, super::Error> {
        self.to_model_with(resources, self.animations())
    }

    /// [`Self::to_model`] over the clips a caller asks for.
    ///
    /// A document may carry hundreds of clips a scene never plays, and turning
    /// one into model data copies its keyframes; this is how a caller that
    /// knows which clip it plays pays for that one alone. The clips are turned
    /// into the model in the order given, so the model's clip indices are
    /// positions in `clips` rather than in the document — a caller that passes
    /// `self.animations()` gets the document's indices, which is what
    /// [`Self::to_model`] does.
    ///
    /// # Errors
    ///
    /// As [`Self::to_model`], plus
    /// [`Error::AnimationOutsideDocument`](super::Error::AnimationOutsideDocument)
    /// when `clips` names an animation the document does not carry.
    pub fn to_model_with(
        &self,
        resources: &GltfResources,
        clips: impl IntoIterator<Item = AnimationId>,
    ) -> Result<LitModel, super::Error> {
        let (nodes, remap) = self.model_nodes(resources)?;
        let animations = clips
            .into_iter()
            .map(|animation| self.clip_model(animation, &remap))
            .collect::<Result<Vec<_>, super::Error>>()?;
        Ok(LitModel::new(nodes, animations)?)
    }

    /// The nodes the default scene draws with, and the document-to-model node
    /// remap they were built against.
    fn model_nodes(
        &self,
        resources: &GltfResources,
    ) -> Result<(Vec<LitModelNode>, Vec<Option<usize>>), super::Error> {
        let scene = self
            .document
            .default_scene()
            .ok_or(super::Error::NoDefaultScene)?;
        let mut selected = vec![false; self.document.nodes().len()];
        let mut stack: Vec<_> = scene.nodes().collect();
        while let Some(node) = stack.pop() {
            if !std::mem::replace(&mut selected[node.index()], true) {
                stack.extend(node.children());
            }
        }
        let mut parents = self.parents.clone();
        for root in scene.nodes() {
            parents[root.index()] = None;
        }
        let mut included = selected.clone();
        // Keep a joint and its ancestry even when a document leaves that
        // skeleton outside the scene. An unrelated scene is not instantiated.
        for node in self.document.nodes().filter(|node| selected[node.index()]) {
            if let Some(skin) = node.skin() {
                for joint in skin.joints() {
                    let mut index = joint.index();
                    while !std::mem::replace(&mut included[index], true) {
                        let Some(parent) = parents[index] else { break };
                        index = parent;
                    }
                }
            }
        }
        let mut remap = vec![None; included.len()];
        for (model_index, index) in (0..included.len()).filter(|&i| included[i]).enumerate() {
            remap[index] = Some(model_index);
        }
        let nodes = self
            .document
            .nodes()
            .filter(|node| included[node.index()])
            .map(|node| {
                let index = node.index();
                let primitives = match node.mesh().filter(|_| selected[index]) {
                    Some(mesh) => mesh
                        .primitives()
                        .map(|primitive| {
                            self.draw_primitive(
                                super::NodeId::new(index),
                                &mesh,
                                &primitive,
                                resources,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    None => Vec::new(),
                };
                let skinned = primitives
                    .iter()
                    .any(|primitive| primitive.mesh.skin.is_some());
                let skin = self.model_skin(&node, index, &remap, skinned)?;
                let morph = primitives
                    .iter()
                    .any(|primitive| primitive.mesh.morph_targets > 0)
                    .then(|| LitModelMorph {
                        weights: self.morph_weights(super::NodeId::new(index)),
                    });
                Ok(LitModelNode {
                    parent: parents[index].and_then(|parent| remap[parent]),
                    transform: self.locals[index].clone(),
                    primitives,
                    skin,
                    morph,
                })
            })
            .collect::<Result<Vec<_>, super::Error>>()?;
        Ok((nodes, remap))
    }

    /// One document animation as model data, over the nodes the model carries.
    ///
    /// The length is the document's, taken before any channel is dropped: a
    /// clip that drives a node this model leaves out was still authored as long
    /// as the document says, so a caller phases over the clip the author made
    /// rather than over the part of it that survived the remap.
    /// [`Self::animation_duration`] answers the same number from the same
    /// channels.
    fn clip_model(
        &self,
        animation: AnimationId,
        remap: &[Option<usize>],
    ) -> Result<LitModelClip, super::Error> {
        let source = self.clip(animation)?;
        let channels: Vec<_> = source
            .iter()
            .filter_map(|channel| {
                Some(crate::lit::LitModelChannel {
                    node: remap[channel.node]?,
                    ..channel.clone()
                })
            })
            .collect();
        Ok(LitModelClip {
            name: self.animation_name(animation)?.map(str::to_owned),
            duration: duration(source),
            channels,
        })
    }

    /// The skin a model node carries, or `None` when no drawn primitive of it
    /// is skinned.
    ///
    /// A node's skin is only carried when a primitive drawn from it is
    /// skinned: an unskinned draw would ignore the pose, so the joints and the
    /// per-frame matrix work that comes with them would be spent for nothing.
    fn model_skin(
        &self,
        node: &gltf::Node<'_>,
        index: usize,
        remap: &[Option<usize>],
        skinned: bool,
    ) -> Result<Option<LitModelSkin>, super::Error> {
        let Some(skin) = node.skin().filter(|_| skinned) else {
            return Ok(None);
        };
        let skin = self.decode_skin(&skin);
        // Every joint was added to the included set above, and its ancestors
        // with it, so the remapped set covers them. A joint it missed would
        // leave a skin indexing a node the model does not carry, which is what
        // the error names.
        let joints = remapped_joints(
            skin.joints.iter().copied().map(|joint| remap[joint]),
            super::NodeId::new(index),
        )?;
        Ok(Some(LitModelSkin::new(joints, skin.inverse_bind)))
    }
}
