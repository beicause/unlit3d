//! Driving an animation, and asking what one carries.
//!
//! A document's animations name channels over its nodes; sampling one at a
//! time and writing the result back into the world's components is what makes
//! a spawned hierarchy move.

use super::animation::{interpolation_stride, sample_rotation, sample_vec3, sample_weights};
use super::scene::{decompose, node_order};
use super::{AnimationId, GltfNode, LitGltf, NodeId};
use crate::components::{MorphWeights, SkinPose, Transform};
use gltf::animation::Property;
use gltf::animation::util::ReadOutputs;
use unlit_ecs::Scope;

impl LitGltf {
    /// The number of animations the document carries.
    pub fn animation_count(&self) -> usize {
        self.document.animations().count()
    }

    /// The name of an animation, or `None` when it carries none.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub fn animation_name(&self, animation: AnimationId) -> Option<&str> {
        self.animation(animation.index()).name()
    }

    /// The duration of an animation in seconds: the latest input keyframe
    /// across all of its samplers.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub fn animation_duration(&self, animation: AnimationId) -> f32 {
        let animation = self.animation(animation.index());
        let mut duration: f32 = 0.0;
        for channel in animation.channels() {
            let reader = channel.reader(|buffer| Some(&self.buffers[buffer.index()]));
            if let Some(inputs) = reader.read_inputs()
                && let Some(last) = inputs.last()
            {
                duration = duration.max(last);
            }
        }
        duration
    }

    /// Drive an animation to `time` seconds, writing what it moves back into
    /// the world.
    ///
    /// `nodes` is the list [`Self::spawn_node`] /
    /// [`Self::spawn_default_scene`] returned: for every node it names, the
    /// animated world transform is written into the entity's [`Transform`],
    /// a skinned node's [`SkinPose`] is rebuilt against the animated joints,
    /// and a morphed node's [`MorphWeights`] are set to the sampled weights.
    ///
    /// `world` is borrowed shared, not mutable: components are written through
    /// the world's interior mutability, so the same call site can keep reading
    /// the world while it animates.
    ///
    /// A channel's inputs outside its keyframe range clamp to the nearest end,
    /// and translation, scale and rotation interpolate by the sampler's
    /// `interpolation`: `STEP` holds the previous keyframe, `LINEAR` blends
    /// (with a spherical blend for a rotation) and `CUBICSPLINE` evaluates the
    /// Hermite spline through the frame's tangents. A node no channel targets
    /// keeps its rest transform, and a node that is neither spawned nor an
    /// ancestor of one costs nothing.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub fn apply_animation(
        &self,
        world: &Scope<'_>,
        animation: AnimationId,
        time: f32,
        nodes: &[GltfNode],
    ) {
        let animation = self.animation(animation.index());
        let count = self.parents.len();
        let mut locals = self.locals.clone();
        let mut animated = vec![false; count];
        let mut weights: Vec<Option<Vec<f32>>> = vec![None; count];

        for channel in animation.channels() {
            let node = channel.target().node().index();
            let property = channel.target().property();
            let interpolation = channel.sampler().interpolation();
            let reader = channel.reader(|buffer| Some(&self.buffers[buffer.index()]));
            let Some(inputs) = reader.read_inputs() else {
                continue;
            };
            let times: Vec<f32> = inputs.collect();
            // Interpolation needs an interval to work in, and the spec
            // requires at least two keyframes anyway.
            if times.len() < 2 {
                continue;
            }
            let Some(outputs) = reader.read_outputs() else {
                continue;
            };
            let required = times.len() * interpolation_stride(interpolation);
            match (property, outputs) {
                (Property::Translation, ReadOutputs::Translations(values)) => {
                    let values: Vec<[f32; 3]> = values.collect();
                    if values.len() < required {
                        continue;
                    }
                    locals[node].translation =
                        sample_vec3(interpolation, &times, &values, time).into();
                    animated[node] = true;
                }
                (Property::Scale, ReadOutputs::Scales(values)) => {
                    let values: Vec<[f32; 3]> = values.collect();
                    if values.len() < required {
                        continue;
                    }
                    locals[node].scale = sample_vec3(interpolation, &times, &values, time).into();
                    animated[node] = true;
                }
                (Property::Rotation, ReadOutputs::Rotations(rotations)) => {
                    let values: Vec<[f32; 4]> = rotations.into_f32().collect();
                    if values.len() < required {
                        continue;
                    }
                    locals[node].rotation = sample_rotation(interpolation, &times, &values, time);
                    animated[node] = true;
                }
                (Property::MorphTargetWeights, ReadOutputs::MorphTargetWeights(outputs)) => {
                    let values: Vec<f32> = outputs.into_f32().collect();
                    let targets = self.morph_weights(NodeId::new(node)).len();
                    let needed = times.len() * targets * interpolation_stride(interpolation);
                    if targets == 0 || values.len() < needed {
                        continue;
                    }
                    weights[node] = Some(sample_weights(
                        interpolation,
                        &times,
                        &values,
                        targets,
                        time,
                    ));
                }
                _ => {}
            }
        }

        if !animated.iter().any(|&moved| moved) && weights.iter().all(Option::is_none) {
            return;
        }

        // A node whose ancestor moved has moved too, even when no channel
        // names it: mark the descendants before recomposing.
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); count];
        for (node, parent) in self.parents.iter().enumerate() {
            if let Some(parent) = parent {
                children[*parent].push(node);
            }
        }
        let mut dirty = animated.clone();
        let mut stack: Vec<usize> = (0..count).filter(|&node| dirty[node]).collect();
        while let Some(node) = stack.pop() {
            for &child in &children[node] {
                if !dirty[child] {
                    dirty[child] = true;
                    stack.push(child);
                }
            }
        }

        let mut matrices = self.node_matrices.clone();
        for &node in &node_order(&self.document, &self.parents) {
            if !dirty[node] {
                continue;
            }
            let local = glam::Mat4::from(locals[node].compute_matrix());
            matrices[node] = match self.parents[node] {
                Some(parent) => matrices[parent] * local,
                None => local,
            };
        }

        for handle in nodes {
            let node = handle.node.index();
            if dirty[node] {
                let transform = decompose(matrices[node]);
                for &entity in &handle.entities {
                    let _ = world
                        .query_one::<&mut Transform>(entity)
                        .map(|mut current| {
                            let current = current.get();
                            {
                                *current = transform.clone();
                            }
                        });
                }
            }
            if let Some(entity) = handle.pose {
                let pose = self.skin_pose_with_matrices(node, &matrices);
                let _ = world.query_one::<&mut SkinPose>(entity).map(|mut current| {
                    let current = current.get();
                    *current = pose.clone()
                });
            }
            if let (Some(entity), Some(sampled)) = (handle.weights, weights[node].as_ref()) {
                let _ = world
                    .query_one::<&mut MorphWeights>(entity)
                    .map(|mut current| {
                        let current = current.get();
                        {
                            current.weights = sampled.clone();
                        }
                    });
            }
        }
    }

    fn animation(&self, animation: usize) -> gltf::animation::Animation<'_> {
        self.document
            .animations()
            .nth(animation)
            .expect("glTF animation index in bounds")
    }
}
