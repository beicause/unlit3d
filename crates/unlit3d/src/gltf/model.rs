//! Convert a loaded glTF document into the generic lit model representation.

use super::{AnimationId, GltfResources, LitGltf};
use crate::lit::{LitModel, LitModelClip, LitModelMorph, LitModelNode, LitModelSkin};

impl LitGltf {
    /// Prepare the default scene as a reusable model asset with no playback state.
    /// Only the scene and any joint dependencies become model nodes. Animation
    /// channels are remapped to those nodes; clip indices stay unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no default scene or a drawn primitive is
    /// missing its uploaded mesh or material.
    pub fn to_model(&self, resources: &GltfResources) -> Result<LitModel, super::Error> {
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
                        .map(|primitive| self.draw_primitive(index, &mesh, &primitive, resources))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => Vec::new(),
                };
                let skin = primitives
                    .iter()
                    .any(|primitive| primitive.mesh.skinned)
                    .then(|| node.skin())
                    .flatten()
                    .map(|skin| {
                        let reader = skin.reader(|buffer| {
                            self.buffers
                                .get(buffer.index())
                                .map(|data| data.0.as_slice())
                        });
                        let joints = skin
                            .joints()
                            .map(|joint| {
                                remap[joint.index()]
                                    .expect("joint dependencies were included before remapping")
                            })
                            .collect();
                        let inverse_bind = reader
                            .read_inverse_bind_matrices()
                            .map(|values| {
                                values
                                    .map(|matrix| glam::Mat4::from_cols_array_2d(&matrix))
                                    .collect()
                            })
                            .unwrap_or_default();
                        LitModelSkin {
                            joints,
                            inverse_bind,
                        }
                    });
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
        let animations = (0..self.document.animations().count())
            .map(|index| self.clip_model(AnimationId::new(index), &remap))
            .collect();
        Ok(LitModel::new(nodes, animations))
    }

    fn clip_model(&self, animation: AnimationId, remap: &[Option<usize>]) -> LitModelClip {
        let channels: Vec<_> = self
            .clip(animation)
            .iter()
            .filter_map(|channel| {
                Some(crate::lit::LitModelChannel {
                    node: remap[channel.node]?,
                    ..channel.clone()
                })
            })
            .collect();
        let duration = channels
            .iter()
            .filter_map(|channel| channel.times.last().copied())
            .fold(0.0, f32::max);
        LitModelClip {
            name: self.animation_name(animation).map(str::to_owned),
            duration,
            channels,
        }
    }
}
