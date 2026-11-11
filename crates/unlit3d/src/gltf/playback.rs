//! Driving an animation, and asking what one carries.
//!
//! A document's animations name channels over its nodes; sampling one at a
//! time and writing the result back into the world's components is what makes
//! a spawned hierarchy move.

use std::cell::RefCell;

use super::animation::{ChannelValues, channels, sample_rotation, sample_vec3, sample_weights};
use super::scene::decompose;
use super::{AnimationId, GltfNode, LitGltf};
use crate::components::{MorphWeights, SkinPose, Transform};
use unlit_ecs::Scope;

/// The per-frame working set of an animation, reused across frames.
///
/// Sampling a clip needs a copy of the locals it overwrites, the world
/// matrices recomposed from them, and one flag and one weight buffer per node.
/// A document's node count is fixed, so these grow to their high-water mark
/// once and are then only cleared. The buffer is borrowed for the duration of a
/// call and handed back afterwards, which is also what lets one frame animate
/// two documents in turn.
#[derive(Default)]
struct Scratch {
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

thread_local! {
    #[allow(clippy::missing_const_for_thread_local, reason = "the initializer is already a const block")]
    static SCRATCH: RefCell<Scratch> = const { RefCell::new(Scratch {
        locals: Vec::new(),
        matrices: Vec::new(),
        weights: Vec::new(),
        animated: Vec::new(),
        weighted: Vec::new(),
        dirty: Vec::new(),
        stack: Vec::new(),
    }) };
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
    /// The clip's keyframes are decoded on the first frame that plays it and
    /// kept afterwards, and this call's working set is reused across frames, so
    /// a frame allocates only where it writes something new into the world.
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
        let mut scratch = SCRATCH.with_borrow_mut(std::mem::take);
        self.animate(&mut scratch, world, animation, time, nodes);
        SCRATCH.with_borrow_mut(|slot| *slot = scratch);
    }

    /// [`Self::apply_animation`], working in a buffer the caller owns.
    fn animate(
        &self,
        scratch: &mut Scratch,
        world: &Scope<'_>,
        animation: AnimationId,
        time: f32,
        nodes: &[GltfNode],
    ) {
        let clip = self.clip(animation);
        let count = self.parents.len();

        scratch.locals.clear();
        scratch.locals.extend_from_slice(&self.locals);
        scratch.animated.clear();
        scratch.animated.resize(count, false);
        scratch.weighted.clear();
        scratch.weighted.resize(count, false);
        scratch.weights.resize_with(count, Vec::new);

        for channel in clip {
            let node = channel.node;
            match &channel.values {
                // A channel whose values do not cover its times cannot be
                // sampled, which decoding marks by stating none of them.
                ChannelValues::Translations(values) if !values.is_empty() => {
                    scratch.locals[node].translation =
                        sample_vec3(channel.interpolation, &channel.times, values, time).into();
                    scratch.animated[node] = true;
                }
                ChannelValues::Scales(values) if !values.is_empty() => {
                    scratch.locals[node].scale =
                        sample_vec3(channel.interpolation, &channel.times, values, time).into();
                    scratch.animated[node] = true;
                }
                ChannelValues::Rotations(values) if !values.is_empty() => {
                    scratch.locals[node].rotation =
                        sample_rotation(channel.interpolation, &channel.times, values, time);
                    scratch.animated[node] = true;
                }
                ChannelValues::Weights(values) => {
                    let targets = self.morph_targets[node];
                    let needed = channel.times.len()
                        * targets
                        * super::animation::interpolation_stride(channel.interpolation);
                    if targets == 0 || values.len() < needed {
                        continue;
                    }
                    sample_weights(
                        channel.interpolation,
                        &channel.times,
                        values,
                        targets,
                        time,
                        &mut scratch.weights[node],
                    );
                    scratch.weighted[node] = true;
                }
                _ => {}
            }
        }

        if !scratch.animated.iter().any(|&moved| moved)
            && !scratch.weighted.iter().any(|&weighted| weighted)
        {
            return;
        }

        // A node whose ancestor moved has moved too, even when no channel
        // names it: mark the descendants before recomposing.
        scratch.dirty.clear();
        scratch.dirty.extend_from_slice(&scratch.animated);
        scratch.stack.clear();
        scratch
            .stack
            .extend((0..count).filter(|&node| scratch.dirty[node]));
        while let Some(node) = scratch.stack.pop() {
            for &child in &self.children[node] {
                if !scratch.dirty[child] {
                    scratch.dirty[child] = true;
                    scratch.stack.push(child);
                }
            }
        }

        scratch.matrices.clear();
        scratch.matrices.extend_from_slice(&self.node_matrices);
        for &node in &self.order {
            if !scratch.dirty[node] {
                continue;
            }
            let local = glam::Mat4::from(scratch.locals[node].compute_matrix());
            scratch.matrices[node] = match self.parents[node] {
                Some(parent) => scratch.matrices[parent] * local,
                None => local,
            };
        }

        for handle in nodes {
            let node = handle.node.index();
            if scratch.dirty[node] {
                let transform = decompose(scratch.matrices[node]);
                for &entity in &handle.entities {
                    if let Ok(mut current) = world.query_one::<&mut Transform>(entity) {
                        *current.get() = transform.clone();
                    }
                }
            }
            // A pose is a matrix per joint of the skin, so it is only rebuilt
            // when the mesh node or one of its joints moved.
            if let Some(entity) = handle.pose
                && self.skin_moved(node, &scratch.dirty)
            {
                let pose = self.skin_pose_with_matrices(node, &scratch.matrices);
                if let Ok(mut current) = world.query_one::<&mut SkinPose>(entity) {
                    *current.get() = pose;
                }
            }
            if let (Some(entity), true) = (handle.weights, scratch.weighted[node])
                && let Ok(mut current) = world.query_one::<&mut MorphWeights>(entity)
            {
                current.get().weights.clone_from(&scratch.weights[node]);
            }
        }
    }

    /// The decoded keyframes of an animation, decoding them on first use.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    fn clip(&self, animation: AnimationId) -> &[super::animation::Channel] {
        let index = animation.index();
        // Resolved first, so an out-of-bounds index panics with the same
        // message as every other accessor of an animation.
        let animation = self.animation(index);
        self.clips[index].get_or_init(|| channels(animation, &self.buffers))
    }

    fn animation(&self, animation: usize) -> gltf::animation::Animation<'_> {
        self.document
            .animations()
            .nth(animation)
            .expect("glTF animation index in bounds")
    }
}
