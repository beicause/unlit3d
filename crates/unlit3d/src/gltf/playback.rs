//! Driving an animation, and asking what one carries.
//!
//! A document's animations name channels over its nodes; sampling one at a
//! time and writing the result back into the world's components is what makes
//! a spawned hierarchy move.

use super::animation::channels;
use super::{AnimationId, GltfNode, LitGltf};
use crate::components::{MorphWeights, SkinPose, Transform};
use crate::lit::sampling::{sample_rotation, sample_vec3, sample_weights};
use crate::lit::{LitModelChannel, LitModelChannelValues as ChannelValues};
use unlit_ecs::Scope;

/// The working set an animation samples into, owned by the caller.
///
/// Sampling a clip needs a copy of the locals it overwrites, the world
/// matrices recomposed from them, and one flag and one weight buffer per node.
/// A document's node count is fixed, so a player grown against one document
/// holds its high-water mark and is then only cleared. Keeping the player
/// outside the library is what makes [`LitGltf::apply_animation`] take `&self`:
/// two documents, or two threads, animate through players of their own, and a
/// frame that animates nothing allocates nothing.
///
/// A player carries no document state of its own: every buffer is refilled
/// from the document each call samples, so one player serves any number of
/// documents in turn and grows to the largest node count it has met.
#[derive(Default)]
pub struct AnimationPlayer {
    /// The node locals the frame's channels write into.
    locals: Vec<Transform>,
    /// The world matrices recomposed from `locals`.
    matrices: Vec<glam::Mat4>,
    /// One weight buffer per node, holding the weights a channel sampled.
    weights: Vec<Vec<f32>>,
    /// Whether a node's transform was sampled this frame.
    animated: Vec<bool>,
    /// Whether a node's morph weights were sampled this frame.
    weighted: Vec<bool>,
    /// Whether a node has moved, itself or through an ancestor.
    dirty: Vec<bool>,
    /// The nodes whose descendants are still to be marked.
    stack: Vec<usize>,
}

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
        self.clip(animation)
            .iter()
            .filter_map(|channel| channel.times.last())
            .fold(0.0, |duration, last| duration.max(*last))
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
    /// `player` holds the buffers the samples land in, so a caller that
    /// animates every frame keeps one beside the document: see
    /// [`AnimationPlayer`] for what it saves.
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
    /// keeps whatever transform it carries — a placement the caller composed
    /// onto it included — while a node a channel reaches is written at the
    /// document's own transform for it, which is what the clip animates. A
    /// caller that placed a document whose nodes it also animates composes the
    /// placement again onto what this wrote. A node that is neither spawned
    /// nor an ancestor of one costs nothing.
    ///
    /// The clip's keyframes are decoded on the first frame that plays it and
    /// kept afterwards, and `player` is reused across frames, so a frame
    /// allocates only where it writes something new into the world.
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
        player: &mut AnimationPlayer,
    ) {
        let clip = self.clip(animation);
        let count = self.parents.len();

        player.locals.clear();
        player.locals.extend_from_slice(&self.locals);
        player.animated.clear();
        player.animated.resize(count, false);
        player.weighted.clear();
        player.weighted.resize(count, false);
        player.weights.resize_with(count, Vec::new);

        for channel in clip {
            let node = channel.node;
            match &channel.values {
                // A channel whose values do not cover its times cannot be
                // sampled, which decoding marks by stating none of them.
                ChannelValues::Translations(values) if !values.is_empty() => {
                    player.locals[node].translation =
                        sample_vec3(channel.interpolation, &channel.times, values, time).into();
                    player.animated[node] = true;
                }
                ChannelValues::Scales(values) if !values.is_empty() => {
                    player.locals[node].scale =
                        sample_vec3(channel.interpolation, &channel.times, values, time).into();
                    player.animated[node] = true;
                }
                ChannelValues::Rotations(values) if !values.is_empty() => {
                    player.locals[node].rotation =
                        sample_rotation(channel.interpolation, &channel.times, values, time);
                    player.animated[node] = true;
                }
                // Decoding drops a weights channel that states fewer values
                // than its times and target count need, so whatever reaches
                // here is sampleable.
                ChannelValues::Weights { targets, values } => {
                    sample_weights(
                        channel.interpolation,
                        &channel.times,
                        values,
                        *targets,
                        time,
                        &mut player.weights[node],
                    );
                    player.weighted[node] = true;
                }
                _ => {}
            }
        }

        if !player.animated.iter().any(|&moved| moved)
            && !player.weighted.iter().any(|&weighted| weighted)
        {
            return;
        }

        // A node whose ancestor moved has moved too, even when no channel
        // names it: mark the descendants before recomposing.
        player.dirty.clear();
        player.dirty.extend_from_slice(&player.animated);
        player.stack.clear();
        player
            .stack
            .extend((0..count).filter(|&node| player.dirty[node]));
        while let Some(node) = player.stack.pop() {
            for &child in &self.children[node] {
                if !player.dirty[child] {
                    player.dirty[child] = true;
                    player.stack.push(child);
                }
            }
        }

        player.matrices.clear();
        player.matrices.extend_from_slice(&self.node_matrices);
        for &node in &self.order {
            if !player.dirty[node] {
                continue;
            }
            let local = glam::Mat4::from(player.locals[node].compute_matrix());
            player.matrices[node] = match self.parents[node] {
                Some(parent) => player.matrices[parent] * local,
                None => local,
            };
        }

        for handle in nodes {
            let node = handle.node.index();
            if player.dirty[node] {
                let transform =
                    Transform::from_matrix(glam::Affine3A::from_mat4(player.matrices[node]));
                for &entity in &handle.entities {
                    if let Ok(mut current) = world.try_query_one::<&mut Transform>(entity) {
                        *current.get() = transform.clone();
                    }
                }
            }
            // A pose is a matrix per joint of the skin, so it is only rebuilt
            // when one of its joints moved.
            if let Some(entity) = handle.pose
                && self.skin_moved(node, &player.dirty)
            {
                let pose = self.skin_pose_with_matrices(node, &player.matrices);
                if let Ok(mut current) = world.try_query_one::<&mut SkinPose>(entity) {
                    *current.get() = pose;
                }
            }
            if let (Some(entity), true) = (handle.weights, player.weighted[node])
                && let Ok(mut current) = world.try_query_one::<&mut MorphWeights>(entity)
            {
                current.get().weights.clone_from(&player.weights[node]);
            }
        }
    }

    /// The decoded keyframes of an animation, decoding them on first use.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub(super) fn clip(&self, animation: AnimationId) -> &[LitModelChannel] {
        let index = animation.index();
        // Resolved first, so an out-of-bounds index panics with the same
        // message as every other accessor of an animation.
        let animation = self.animation(index);
        self.clips[index].get_or_init(|| {
            channels(
                animation,
                &self.buffers,
                &self.matrix_nodes,
                &self.morph_targets,
            )
        })
    }

    fn animation(&self, animation: usize) -> gltf::animation::Animation<'_> {
        self.document
            .animations()
            .nth(animation)
            .expect("glTF animation index in bounds")
    }
}
