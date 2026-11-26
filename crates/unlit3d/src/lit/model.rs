//! A bundle for one complete lit model instance.
//!
//! The root [`Transform`] is the model's placement. Node transforms, draw
//! transforms, skin poses, and morph weights are model-owned derived state:
//! callers edit the root, node states, or playback component, then the model
//! evaluates those inputs into its child entities.
//!
//! The model owns its subtree. Despawning part of it leaves the instance
//! incomplete: evaluation writes to the handles that are still alive and
//! leaves the rest where they were, because a model is spawned as one unit and
//! a piece of it going away is a caller bug the frame cannot repair. Despawn
//! the root to remove the whole model.

#[cfg(test)]
mod tests;

use std::ops::Deref;
use std::rc::Rc;

use super::sampling::sample_channel;
use crate::components::{MorphBinding, MorphWeights, SkinBinding, SkinPose, Transform};
use crate::frame::{FrameBehaviourOrder, OnFrame};
use crate::lit::{LitMeshBundle, LitMeshPrimitive};
use unlit_ecs::{Bundle, DynamicBundle, Entity, OnSpawn, Scope, ScopeMut, World, bundle};

/// The update band for model graph evaluation.
pub const LIT_MODEL_UPDATE_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(-10);

/// Why a [`LitModel`] could not be built.
///
/// Every case the sampler would otherwise have to tolerate is caught here, so a
/// built model indexes its own data without a guard: a channel that names a
/// node, a joint, or a keyframe the model does not carry is refused at the door
/// rather than skipped on every frame.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum LitModelError {
    /// A node names a parent index outside the node list.
    #[error("node {node} names parent {parent}, which is not a node of the model")]
    ParentOutOfRange {
        /// The node that states the parent.
        node: usize,
        /// The parent it names.
        parent: usize,
    },
    /// The parent chain of a node runs in a circle, so it has no root.
    #[error("the parent chain of node {node} runs in a circle")]
    ParentCycle {
        /// One node of the cycle.
        node: usize,
    },
    /// A channel targets a node index outside the node list.
    #[error(
        "channel {channel} of clip {clip} targets node {node}, which is not a node of the model"
    )]
    ChannelNodeOutOfRange {
        /// The clip the channel belongs to.
        clip: usize,
        /// The channel inside that clip.
        channel: usize,
        /// The node it targets.
        node: usize,
    },
    /// A channel states no keyframe at all.
    #[error("channel {channel} of clip {clip} states no keyframe")]
    ChannelWithoutKeys {
        /// The clip the channel belongs to.
        clip: usize,
        /// The channel inside that clip.
        channel: usize,
    },
    /// A channel's key times are not in ascending order.
    #[error("channel {channel} of clip {clip} states a key time at {index} that is not ascending")]
    ChannelTimesNotAscending {
        /// The clip the channel belongs to.
        clip: usize,
        /// The channel inside that clip.
        channel: usize,
        /// The index of the offending key time.
        index: usize,
    },
    /// A channel's key times are not finite numbers.
    #[error("channel {channel} of clip {clip} states a key time at {index} that is not a number")]
    ChannelTimeNotFinite {
        /// The clip the channel belongs to.
        clip: usize,
        /// The channel inside that clip.
        channel: usize,
        /// The index of the offending key time.
        index: usize,
    },
    /// A channel states fewer values than its keyframes and interpolation need.
    #[error(
        "channel {channel} of clip {clip} states {values} values, \
         but its {times} keyframes need {expected}"
    )]
    ChannelValuesShort {
        /// The clip the channel belongs to.
        clip: usize,
        /// The channel inside that clip.
        channel: usize,
        /// How many keyframes the channel states.
        times: usize,
        /// How many values it states.
        values: usize,
        /// How many values its keyframes need.
        expected: usize,
    },
    /// A weights channel animates a target count the node does not carry.
    #[error(
        "channel {channel} of clip {clip} animates {targets} morph targets, \
         but node {node} carries {weights}"
    )]
    MorphTargetsMismatch {
        /// The clip the channel belongs to.
        clip: usize,
        /// The channel inside that clip.
        channel: usize,
        /// The node it targets.
        node: usize,
        /// How many targets the channel animates.
        targets: usize,
        /// How many the node carries.
        weights: usize,
    },
    /// A skin names a joint index outside the node list.
    #[error("the skin of node {node} names joint {joint}, which is not a node of the model")]
    JointOutOfRange {
        /// The skinned node.
        node: usize,
        /// The joint it names.
        joint: usize,
    },
    /// A clip states a duration that is not a finite, non-negative length.
    #[error("clip {clip} states the duration {duration}, which is not a length")]
    ClipDuration {
        /// The clip.
        clip: usize,
        /// The duration it states.
        duration: f32,
    },
}

/// The data one model asset carries, shared by every instance that draws it.
#[derive(Debug)]
pub struct LitModelData {
    /// Nodes in any order; each parent is addressed by its index.
    pub nodes: Vec<LitModelNode>,
    /// Animation clips that can drive the nodes.
    pub animations: Vec<LitModelClip>,
}

/// A prepared model asset: one shared handle to its [`LitModelData`].
///
/// A clone is a handle copy, so one asset serves any number of instances
/// without duplicating its nodes, keyframes, or GPU handles — the same bargain
/// [`GpuMesh`](crate::components::GpuMesh) makes for its buffers. The data is
/// checked once, in [`LitModel::new`], which is the only way to build one.
#[derive(Clone, Debug)]
pub struct LitModel(Rc<LitModelData>);

impl Deref for LitModel {
    type Target = LitModelData;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl LitModel {
    /// Check `nodes` and `animations`, then share them as one asset.
    ///
    /// # Errors
    ///
    /// Returns the first inconsistency it finds: see [`LitModelError`]. The
    /// rules a built model is held to, and the sampler relieves itself of, are
    /// that every parent index addresses a node, that no parent chain circles,
    /// that every skin joints only nodes it names, that every channel targets a
    /// node and states at least one finite, ascending key time with every value
    /// its interpolation and morph target count need, and that a clip's duration
    /// is a finite length.
    pub fn new(
        nodes: Vec<LitModelNode>,
        animations: Vec<LitModelClip>,
    ) -> Result<Self, LitModelError> {
        check_nodes(&nodes)?;
        for (index, clip) in animations.iter().enumerate() {
            check_clip(&nodes, index, clip)?;
        }
        Ok(Self(Rc::new(LitModelData { nodes, animations })))
    }

    /// The index of the clip called `name`, or `None` when the asset carries no
    /// such clip.
    ///
    /// Assets name their clips, so a caller that wants a particular one asks by
    /// name rather than by a position a re-export could shuffle.
    #[must_use]
    pub fn clip_index(&self, name: &str) -> Option<usize> {
        self.animations
            .iter()
            .position(|clip| clip.name.as_deref() == Some(name))
    }
}

/// One model node.
#[derive(Clone, Debug)]
pub struct LitModelNode {
    /// Parent node, or `None` for a model root.
    ///
    /// [`LitModel::new`] refuses an index outside the node list and a chain
    /// that returns to a node it already passed through, so every node here
    /// reaches a root.
    pub parent: Option<usize>,
    /// The node local transform before animation.
    pub transform: Transform,
    /// Draws attached to this node.
    pub primitives: Vec<LitMeshPrimitive>,
    /// Skin data used by this node's draws.
    pub skin: Option<LitModelSkin>,
    /// Morph weights used by this node's draws.
    pub morph: Option<LitModelMorph>,
}

/// Skin joints and the inverse bind matrix of each.
///
/// A joint and its matrix are one pair, so the two lists always have the same
/// length: [`LitModelSkin::new`] is the only way to build one, and it pads or
/// truncates the matrices to match the joints.
#[derive(Clone, Debug)]
pub struct LitModelSkin {
    /// Node indices of the joints, in joint order.
    joints: Vec<usize>,
    /// The inverse bind matrix of each joint, in the same order.
    inverse_bind: Vec<glam::Mat4>,
}

impl LitModelSkin {
    /// Pair `joints` with one inverse bind matrix each.
    ///
    /// A document need not state a matrix per joint, so an `inverse_bind`
    /// shorter than `joints` binds the joints past it at the identity — the
    /// pose a joint was bound in, which deforms nothing — and one longer is
    /// truncated, because the matrices past the last joint name no joint.
    #[must_use]
    pub fn new(joints: Vec<usize>, mut inverse_bind: Vec<glam::Mat4>) -> Self {
        inverse_bind.resize(joints.len(), glam::Mat4::IDENTITY);
        Self {
            joints,
            inverse_bind,
        }
    }

    /// The node indices of the joints, in joint order.
    #[must_use]
    pub fn joints(&self) -> &[usize] {
        &self.joints
    }

    /// The inverse bind matrix of each joint, in joint order.
    #[must_use]
    pub fn inverse_bind(&self) -> &[glam::Mat4] {
        &self.inverse_bind
    }
}

/// Morph target weights.
#[derive(Clone, Debug)]
pub struct LitModelMorph {
    /// Initial weight for each morph target.
    pub weights: Vec<f32>,
}

/// Interpolation used by an animation channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LitModelInterpolation {
    /// Hold the previous key.
    Step,
    /// Interpolate between keys.
    Linear,
    /// Cubic Hermite interpolation with glTF tangents.
    CubicSpline,
}

/// The property one animation channel drives, with its decoded key values.
///
/// The variant carries both, because they are one fact: a channel that states
/// rotations and a channel that says it states rotations cannot disagree when
/// there is only one field to state.
#[derive(Clone, Debug, PartialEq)]
pub enum LitModelOutput {
    /// Translation keys, with cubic values stored in glTF kind-first triples.
    Translation(Vec<[f32; 3]>),
    /// Scale keys, with cubic values stored in glTF kind-first triples.
    Scale(Vec<[f32; 3]>),
    /// Rotation keys as `[x, y, z, w]`, with cubic values kind-first.
    Rotation(Vec<[f32; 4]>),
    /// Weight keys, packed by key and target.
    Weights {
        /// Number of targets in each key.
        targets: usize,
        /// Values, including cubic tangents when applicable.
        values: Vec<f32>,
    },
}

/// One decoded animation channel.
#[derive(Clone, Debug)]
pub struct LitModelChannel {
    /// Node index targeted by the channel.
    pub node: usize,
    /// Interpolation mode.
    pub interpolation: LitModelInterpolation,
    /// Ascending key times.
    pub times: Vec<f32>,
    /// The property the channel drives and the key values it states.
    pub values: LitModelOutput,
}

/// One decoded animation clip.
#[derive(Clone, Debug)]
pub struct LitModelClip {
    /// Optional clip name.
    pub name: Option<String>,
    /// Channels evaluated by the clip.
    pub channels: Vec<LitModelChannel>,
    /// The clip's authored length in seconds.
    ///
    /// This is a property of the document rather than of `channels`: a loader
    /// that drops the channels of nodes its model leaves out still reports the
    /// length the clip was authored at, so playback loops over the clip the
    /// author made rather than over the part of it that survived.
    pub duration: f32,
}

/// How a model instance's clip time moves.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub enum LitModelClock {
    /// The frame dispatcher advances the time by the frame's delta.
    Playing {
        /// Seconds of clip time per second of real time.
        speed: f32,
        /// Whether the time wraps at the clip's duration instead of stopping there.
        looping: bool,
    },
    /// The caller writes [`LitModelAnimation::time`] itself.
    ///
    /// The frame dispatcher samples that time every frame and never changes it,
    /// which is what a caller driving playback from its own clock — a fixed
    /// sequence of frames, or a scrubber — wants.
    ///
    /// A caller that writes [`LitModelAnimation::time`] after the model's own
    /// ordered callback has run samples again with
    /// [`LitModelBundle::evaluate`]: the pass is idempotent here, since a
    /// manual clock neither advances the time nor writes it back.
    Manual,
}

/// Per-instance playback state.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct LitModelAnimation {
    /// Selected clip index.
    pub clip: Option<usize>,
    /// Current clip time in seconds.
    pub time: f32,
    /// How the time moves.
    pub clock: LitModelClock,
}

impl Default for LitModelAnimation {
    fn default() -> Self {
        Self {
            clip: None,
            time: 0.0,
            clock: LitModelClock::Playing {
                speed: 1.0,
                looping: true,
            },
        }
    }
}

/// The handles one model root's evaluation writes through.
///
/// Every field is derived: the model spawns these entities and rewrites them as
/// it evaluates, so the type is read-only from the outside. A caller that needs
/// a node's transform edits that node's [`LitModelNodeState`], not the entity
/// this names.
#[derive(Clone, Debug, Default)]
#[cfg_attr(
    feature = "reflect",
    derive(facet::Facet),
    facet(opaque, proxy = crate::reflect::LitModelInstanceProxy)
)]
pub struct LitModelInstance {
    nodes: Vec<Entity>,
    draws: Vec<Vec<Entity>>,
    poses: Vec<Option<Entity>>,
    weights: Vec<Option<Entity>>,
    /// The world matrix each node was last written through, kept to skip the
    /// writes a node whose pose has not changed would repeat.
    matrices: Vec<glam::Mat4>,
    order: Vec<(usize, Option<usize>)>,
    locals: Vec<Transform>,
    morphs: Vec<Vec<f32>>,
    /// Which nodes' matrices ended the last pass different from the pass
    /// before. Scratch, overwritten for every node on each evaluation.
    moved: Vec<bool>,
    /// Which nodes' morph weights still owe their entity a write: set whenever a
    /// resample rebuilds the node's weights, cleared when the write lands. Starts
    /// all true, so the defaults reach their entities on the first evaluation.
    morph_stale: Vec<bool>,
    /// The base local transform each node was last read with from its
    /// [`LitModelNodeState`]. A change here calls for a resample.
    bases: Vec<Transform>,
    /// The clip and sample time the locals and morphs were last resampled at,
    /// `None` before the first pass. While these and the bases are unchanged the
    /// locals stand as they are, so a paused clock skips the keyframe search.
    sampled: Option<(Option<usize>, Option<f32>)>,
}

impl LitModelInstance {
    /// The node entities, indexed like [`LitModelData::nodes`].
    #[must_use]
    pub fn nodes(&self) -> &[Entity] {
        &self.nodes
    }

    /// The draw entities per node, in the order the node's primitives were spawned.
    #[must_use]
    pub fn draws(&self) -> &[Vec<Entity>] {
        &self.draws
    }

    /// The skin-pose entity per node, where the node carries a skin.
    #[must_use]
    pub fn poses(&self) -> &[Option<Entity>] {
        &self.poses
    }

    /// The morph-weight entity per node, where the node carries morph targets.
    #[must_use]
    pub fn weights(&self) -> &[Option<Entity>] {
        &self.weights
    }

    /// The world matrix each node was last evaluated at.
    #[must_use]
    pub fn matrices(&self) -> &[glam::Mat4] {
        &self.matrices
    }
}

/// Editable state on a spawned model node.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct LitModelNodeState {
    /// Base local transform. A channel overrides only the property it samples.
    pub local: Transform,
}

/// A complete model spawn unit. The root transform is the model placement.
#[derive(Clone, Debug)]
pub struct LitModelBundle {
    /// Placement of the model root in world space.
    pub transform: Transform,
    /// Prepared model asset. Its topology is fixed when the bundle is spawned.
    pub model: LitModel,
    /// Playback state.
    pub animation: LitModelAnimation,
}

impl LitModelBundle {
    /// Create an instance at the identity placement with no clip selected.
    #[must_use]
    pub fn new(model: LitModel) -> Self {
        Self {
            transform: Transform::default(),
            model,
            animation: LitModelAnimation::default(),
        }
    }

    /// Evaluate the node states, root transform and playback time into the
    /// instance's derived entities, without advancing the clock.
    ///
    /// Use this when rendering outside the frame dispatcher or after writing
    /// inputs yourself. The sampling wraps or clamps the time only for its own
    /// lookup; the stored time is left as the caller wrote it. The ordered
    /// [`OnFrame`] callback is the only path that advances playback, and it
    /// advances it only for [`LitModelClock::Playing`].
    pub fn evaluate(scope: &Scope<'_>, root: Entity) {
        evaluate_model(scope, root, Step::Sample);
    }
}

/// Whether an evaluation moves the clip time or only samples it.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Advance by this many seconds first, when the clock is playing.
    Advance(f32),
    /// Sample the stored time as written.
    Sample,
}

/// The parent of each node, with every chain rooted, in root-before-child order.
///
/// [`LitModel::new`] guarantees every parent is in range and no chain circles,
/// so each walk climbs to a root and stops there; a chain that reaches a node an
/// earlier walk already placed stops there too, which is what keeps the order
/// parent-before-child.
fn hierarchy(nodes: &[LitModelNode]) -> Vec<(usize, Option<usize>)> {
    let mut result = Vec::with_capacity(nodes.len());
    let mut placed = vec![false; nodes.len()];
    let mut path = Vec::new();
    for start in 0..nodes.len() {
        let mut index = start;
        while !placed[index] {
            placed[index] = true;
            let parent = nodes[index].parent;
            path.push((index, parent));
            let Some(parent) = parent else { break };
            index = parent;
        }
        while let Some((index, parent)) = path.pop() {
            result.push((index, parent));
        }
    }
    result
}

fn spawn_model(scope: &mut ScopeMut<'_>, root: Entity, model: &LitModelData) {
    let count = model.nodes.len();
    let order = hierarchy(&model.nodes);
    let mut instance = LitModelInstance {
        nodes: vec![root; count],
        draws: vec![Vec::new(); count],
        poses: vec![None; count],
        weights: vec![None; count],
        matrices: vec![glam::Mat4::IDENTITY; count],
        order,
        locals: vec![Transform::default(); count],
        morphs: vec![Vec::new(); count],
        moved: vec![true; count],
        morph_stale: vec![true; count],
        bases: vec![Transform::default(); count],
        sampled: None,
    };
    let mut scope = scope.scope_mut(root);
    for &(index, parent) in &instance.order {
        let node = &model.nodes[index];
        let parent = parent.map_or(root, |p| instance.nodes[p]);
        let entity = scope.scope_mut(parent).spawn_named(
            "lit model node",
            bundle![LitModelNodeState {
                local: node.transform.clone(),
            }],
        );
        instance.nodes[index] = entity;
        let mut scope = scope.scope_mut(entity);
        let pose = node
            .skin
            .as_ref()
            .map(|_| scope.spawn_named("lit model skin pose", bundle![SkinPose::default()]));
        let morph = node.morph.as_ref().map(|_| {
            scope.spawn_named("lit model morph weights", bundle![MorphWeights::default()])
        });
        instance.poses[index] = pose;
        instance.weights[index] = morph;
        instance.draws[index].extend(node.primitives.iter().map(|primitive| {
            scope.spawn_named(
                "lit model primitive",
                LitMeshBundle {
                    transform: Transform::default(),
                    primitive: primitive.clone(),
                    skin: pose
                        .filter(|_| primitive.mesh.skin.is_some())
                        .map(|pose| SkinBinding { pose }),
                    morph: morph
                        .filter(|_| primitive.mesh.morph_targets > 0)
                        .map(|weights| MorphBinding { weights }),
                },
            )
        }));
    }
    *scope
        .readonly()
        .query_one::<&mut LitModelInstance>(root)
        .get() = instance;
}

fn update_model(world: &World, root: Entity, frame: &mut crate::frame::Frame) {
    evaluate_model(&world.root_scope(), root, Step::Advance(frame.delta_time));
}

fn evaluate_model(scope: &Scope<'_>, root: Entity, step: Step) {
    let Ok(mut query) = scope.try_query_one::<(
        &Transform,
        &LitModel,
        &mut LitModelAnimation,
        &mut LitModelInstance,
    )>(root) else {
        return;
    };
    let (root_transform, model, animation, instance) = query.get();
    let LitModelInstance {
        nodes,
        draws,
        poses,
        weights,
        matrices,
        order,
        locals,
        morphs,
        moved,
        morph_stale,
        bases,
        sampled,
    } = instance;
    // An instance is spawned against one asset and then evaluated against
    // whatever model its entity holds, which a caller may swap for another
    // asset. The lists below are indexed by the node indices this instance was
    // built with, so a model of another shape would read past them. A frame
    // that cannot be evaluated is skipped and reported rather than written
    // through a stale instance; the next evaluation of a matching model
    // rebuilds whatever the skipped one left behind.
    if order.len() != model.nodes.len() {
        log::error!(
            "the model instance {root:?} was built for {} nodes but its model states {}; the frame is skipped",
            order.len(),
            model.nodes.len(),
        );
        return;
    }
    let clip = animation.clip.and_then(|index| model.animations.get(index));
    let sample_time = clip.map(|clip| match (animation.clock, step) {
        (LitModelClock::Playing { speed, looping }, Step::Advance(delta)) => {
            animation.time += delta * speed;
            let time = if clip.duration > 0.0 {
                if looping {
                    animation.time.rem_euclid(clip.duration)
                } else {
                    animation.time.clamp(0.0, clip.duration)
                }
            } else {
                animation.time
            };
            animation.time = time;
            time
        }
        // A one-off sampling pass of a playing clip brings the stored time into
        // the clip's range without advancing it, so sampling twice in a frame
        // reads the same pose.
        (LitModelClock::Playing { looping, .. }, Step::Sample) if clip.duration > 0.0 => {
            if looping {
                animation.time.rem_euclid(clip.duration)
            } else {
                animation.time.clamp(0.0, clip.duration)
            }
        }
        // A manual clock's time is the caller's to choose: it is sampled as
        // written, and a time past the last keyframe holds that keyframe rather
        // than wrapping to the start of the clip.
        (LitModelClock::Manual, _) | (LitModelClock::Playing { .. }, Step::Sample) => {
            animation.time
        }
    });
    // Batch reads and writes resolve each archetype's columns once. A handle
    // that is dead, out of view, or lacks the component is left out of its
    // batch, so one lost handle does not stop the rest of the batch.
    let mut indices = Vec::new();
    let mut entities = Vec::new();
    // Reading the bases is the one input that cannot be skipped: it is the
    // entity state that a change to the base transform arrives through.
    let key = (animation.clip, sample_time);
    let mut resample = *sampled != Some(key);
    let first = sampled.is_none();
    reachable::<LitModelNodeState>(
        scope,
        nodes.iter().copied().enumerate(),
        &mut indices,
        &mut entities,
    );
    let mut slots = indices.iter().copied();
    scope
        .query_many::<&LitModelNodeState>(&entities)
        .for_each(|state| {
            let Some(i) = slots.next() else { return };
            if state.local != bases[i] {
                bases[i].clone_from(&state.local);
                resample = true;
            }
        });

    // A resample rebuilds every node from its base before the channels run, so
    // a clip that stops driving a node hands back the base, not its last sample.
    if resample {
        *sampled = Some(key);
        for (i, node) in model.nodes.iter().enumerate() {
            locals[i].clone_from(&bases[i]);
            morphs[i].clear();
            if let Some(morph) = &node.morph {
                morphs[i].extend_from_slice(&morph.weights);
                morph_stale[i] = true;
            }
        }
        if let (Some(clip), Some(sample_time)) = (clip, sample_time) {
            for channel in &clip.channels {
                if matches!(channel.values, LitModelOutput::Weights { .. }) {
                    morph_stale[channel.node] = true;
                }
                sample_channel(
                    channel,
                    sample_time,
                    &mut locals[channel.node],
                    &mut morphs[channel.node],
                );
            }
        }
    }
    let root_matrix = glam::Mat4::from(root_transform.compute_matrix());
    // The matrices start as the previous frame's pose, so a node whose matrix
    // ends the pass unchanged needs no write through its entities. A parent
    // moving marks every descendant through the product, which falls out of the
    // parent-first order the walk follows.
    for &(i, parent) in order.iter() {
        let local = glam::Mat4::from(locals[i].compute_matrix());
        let matrix = parent.map_or(root_matrix, |p| matrices[p]) * local;
        moved[i] = matrix != matrices[i];
        matrices[i] = matrix;
    }
    if first {
        // Without a previous pass the matrices are all identity, so a node at
        // rest would compare equal and owe no write. The first evaluation puts
        // every entity at its evaluated state whatever it holds.
        moved.fill(true);
        morph_stale.fill(true);
    }

    // The draws of every moved node take that node's matrix.
    let moved_draws = (0..draws.len())
        .filter(|&i| moved[i])
        .flat_map(|i| draws[i].iter().map(move |&entity| (i, entity)));
    reachable::<Transform>(scope, moved_draws, &mut indices, &mut entities);
    let mut slots = indices.iter().copied();
    scope
        .query_many::<&mut Transform>(&entities)
        .for_each(|transform| {
            let Some(i) = slots.next() else { return };
            let matrix = glam::Affine3A::from_mat4(matrices[i]);
            transform.clone_from(&Transform::from_matrix(matrix));
        });
    // A skin's pose is built from its joints' matrices, so any joint moving
    // moves the pose, not just the skinned node moving itself.
    let posed = (0..model.nodes.len()).filter_map(|i| {
        let skin = model.nodes[i].skin.as_ref()?;
        let entity = (*poses.get(i)?)?;
        skin.joints()
            .iter()
            .any(|&joint| moved[joint])
            .then_some((i, entity))
    });
    reachable::<SkinPose>(scope, posed, &mut indices, &mut entities);
    let mut slots = indices.iter().copied();
    scope
        .query_many::<&mut SkinPose>(&entities)
        .for_each(|pose| {
            let Some(i) = slots.next() else { return };
            let Some(skin) = &model.nodes[i].skin else {
                return;
            };
            pose.matrices.clear();
            pose.matrices.extend(
                skin.joints()
                    .iter()
                    .zip(skin.inverse_bind())
                    .map(|(&node, &bind)| matrices[node] * bind),
            );
        });
    let morphing = (0..model.nodes.len()).filter_map(|i| {
        let entity = (*weights.get(i)?)?;
        morph_stale[i].then_some((i, entity))
    });
    reachable::<MorphWeights>(scope, morphing, &mut indices, &mut entities);
    let mut slots = indices.iter().copied();
    scope
        .query_many::<&mut MorphWeights>(&entities)
        .for_each(|morph| {
            let Some(i) = slots.next() else { return };
            morph.weights.clone_from(&morphs[i]);
            morph_stale[i] = false;
        });
}

/// Keeps the slots whose entity is alive, visible from `scope`, and carries
/// `T`, as the index and entity lists a batch query takes. A handle that fails
/// any of these is left out, so the rest of the batch still goes through.
fn reachable<T: 'static>(
    scope: &Scope<'_>,
    slots: impl IntoIterator<Item = (usize, Entity)>,
    indices: &mut Vec<usize>,
    entities: &mut Vec<Entity>,
) {
    indices.clear();
    entities.clear();
    for (index, entity) in slots {
        if scope
            .archetype_of(entity)
            .is_ok_and(|archetype| archetype.has::<T>())
        {
            indices.push(index);
            entities.push(entity);
        }
    }
}

/// Check every parent index, the absence of cycles, and every skin's joints.
fn check_nodes(nodes: &[LitModelNode]) -> Result<(), LitModelError> {
    for (node, entry) in nodes.iter().enumerate() {
        if let Some(parent) = entry.parent
            && parent >= nodes.len()
        {
            return Err(LitModelError::ParentOutOfRange { node, parent });
        }
    }
    // 0 unseen, 1 on the chain being walked, 2 placed.
    let mut state = vec![0u8; nodes.len()];
    for start in 0..nodes.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut index = start;
        // Walk up until the chain reaches a node already placed or a root.
        // Only a node still marked as being on the chain closes a cycle: a
        // root ends the chain without one.
        loop {
            if state[index] == 2 {
                break;
            }
            if state[index] == 1 {
                return Err(LitModelError::ParentCycle { node: index });
            }
            state[index] = 1;
            path.push(index);
            let Some(parent) = nodes[index].parent else {
                break;
            };
            index = parent;
        }
        for node in path {
            state[node] = 2;
        }
    }
    for (node, entry) in nodes.iter().enumerate() {
        let Some(skin) = &entry.skin else { continue };
        for &joint in skin.joints() {
            if joint >= nodes.len() {
                return Err(LitModelError::JointOutOfRange { node, joint });
            }
        }
    }
    Ok(())
}

/// Check one clip against the nodes it drives.
fn check_clip(
    nodes: &[LitModelNode],
    clip: usize,
    data: &LitModelClip,
) -> Result<(), LitModelError> {
    if !data.duration.is_finite() || data.duration < 0.0 {
        return Err(LitModelError::ClipDuration {
            clip,
            duration: data.duration,
        });
    }
    for (channel, entry) in data.channels.iter().enumerate() {
        if entry.node >= nodes.len() {
            return Err(LitModelError::ChannelNodeOutOfRange {
                clip,
                channel,
                node: entry.node,
            });
        }
        if entry.times.is_empty() {
            return Err(LitModelError::ChannelWithoutKeys { clip, channel });
        }
        for (index, &time) in entry.times.iter().enumerate() {
            if !time.is_finite() {
                return Err(LitModelError::ChannelTimeNotFinite {
                    clip,
                    channel,
                    index,
                });
            }
            if index > 0 && time < entry.times[index - 1] {
                return Err(LitModelError::ChannelTimesNotAscending {
                    clip,
                    channel,
                    index,
                });
            }
        }
        let stride = super::sampling::interpolation_stride(entry.interpolation);
        let targets = match &entry.values {
            LitModelOutput::Weights { targets, .. } => {
                let weights = nodes[entry.node]
                    .morph
                    .as_ref()
                    .map_or(0, |morph| morph.weights.len());
                if *targets != weights {
                    return Err(LitModelError::MorphTargetsMismatch {
                        clip,
                        channel,
                        node: entry.node,
                        targets: *targets,
                        weights,
                    });
                }
                *targets
            }
            _ => 1,
        };
        let expected = entry.times.len() * stride * targets;
        let stated = match &entry.values {
            LitModelOutput::Translation(values) => values.len(),
            LitModelOutput::Scale(values) => values.len(),
            LitModelOutput::Rotation(values) => values.len(),
            LitModelOutput::Weights { values, .. } => values.len(),
        };
        if stated < expected {
            return Err(LitModelError::ChannelValuesShort {
                clip,
                channel,
                times: entry.times.len(),
                values: stated,
                expected,
            });
        }
    }
    Ok(())
}

impl Bundle for LitModelBundle {
    fn into_bundle(self) -> DynamicBundle {
        let mut bundle = DynamicBundle::new();
        let mut spawn_data = Some(self.model.clone());
        bundle.push_component(self.transform);
        bundle.push_component(self.model);
        bundle.push_component(LitModelInstance::default());
        bundle.push_component(self.animation);
        bundle.push_component(OnSpawn::new(move |scope, root| {
            if let Some(model) = spawn_data.take() {
                spawn_model(scope, root, &model);
                LitModelBundle::evaluate(&scope.readonly(), root);
            }
        }));
        bundle.push_component(OnFrame::with_order(LIT_MODEL_UPDATE_ORDER, update_model));
        bundle
    }
}
