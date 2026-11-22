//! A bundle for one complete lit model instance.
//!
//! The root [`Transform`] is the model's placement. Node transforms, draw
//! transforms, skin poses, and morph weights are model-owned derived state:
//! callers edit the root, node states, or playback component, then the model
//! evaluates those inputs into its child entities.

#[cfg(test)]
mod tests;

use super::sampling::sample_channel;
use crate::components::{MorphBinding, MorphWeights, SkinBinding, SkinPose, Transform};
use crate::frame::{FrameBehaviourOrder, OnFrame};
use crate::lit::{LitMeshBundle, LitMeshPrimitive};
use unlit_ecs::{Bundle, DynamicBundle, Entity, OnSpawn, ScopeMut, World, bundle};

/// The update band for model graph evaluation.
pub const LIT_MODEL_UPDATE_ORDER: FrameBehaviourOrder = FrameBehaviourOrder(-10);

/// Prepared model data. GPU handles are cloneable, so one asset can serve many instances.
#[derive(Clone, Debug)]
pub struct LitModel {
    /// Nodes in any order; each parent is addressed by its index.
    pub nodes: Vec<LitModelNode>,
    /// Animation clips that can drive the nodes.
    pub animations: Vec<LitModelClip>,
}

impl LitModel {
    /// Construct model data. Indices must refer to `nodes`.
    #[must_use]
    pub fn new(nodes: Vec<LitModelNode>, animations: Vec<LitModelClip>) -> Self {
        Self { nodes, animations }
    }
}

/// One model node.
#[derive(Clone, Debug)]
pub struct LitModelNode {
    /// Parent node, or `None` for a model root.
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

/// Skin joints and inverse bind matrices.
#[derive(Clone, Debug)]
pub struct LitModelSkin {
    /// Node indices of the joints.
    pub joints: Vec<usize>,
    /// One inverse bind matrix per joint.
    pub inverse_bind: Vec<glam::Mat4>,
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

/// Node property driven by an animation channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LitModelProperty {
    /// Node translation.
    Translation,
    /// Node scale.
    Scale,
    /// Node rotation.
    Rotation,
    /// Morph target weights.
    Weights,
}

/// Values decoded for one animation channel.
#[derive(Clone, Debug)]
pub enum LitModelChannelValues {
    /// Translation keys, with cubic values stored in glTF kind-first triples.
    Translations(Vec<[f32; 3]>),
    /// Scale keys, with cubic values stored in glTF kind-first triples.
    Scales(Vec<[f32; 3]>),
    /// Rotation keys as `[x, y, z, w]`, with cubic values kind-first.
    Rotations(Vec<[f32; 4]>),
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
    /// Property targeted by the channel.
    pub property: LitModelProperty,
    /// Interpolation mode.
    pub interpolation: LitModelInterpolation,
    /// Monotonically increasing key times.
    pub times: Vec<f32>,
    /// Decoded key values.
    pub values: LitModelChannelValues,
}

/// One decoded animation clip.
#[derive(Clone, Debug)]
pub struct LitModelClip {
    /// Optional clip name.
    pub name: Option<String>,
    /// Channels evaluated by the clip.
    pub channels: Vec<LitModelChannel>,
    /// Duration in seconds.
    pub duration: f32,
}

/// Per-instance playback state.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct LitModelAnimation {
    /// Selected clip index.
    pub clip: Option<usize>,
    /// Current clip time in seconds.
    pub time: f32,
    /// Playback speed multiplier.
    pub speed: f32,
    /// Whether playback wraps at the clip duration.
    pub looping: bool,
}

impl Default for LitModelAnimation {
    fn default() -> Self {
        Self {
            clip: None,
            time: 0.0,
            speed: 1.0,
            looping: true,
        }
    }
}

/// Runtime handles owned by one model root.
#[derive(Clone, Debug, Default)]
pub struct LitModelInstance {
    /// Spawned node entities, indexed like [`LitModel::nodes`].
    pub nodes: Vec<Entity>,
    /// Draw entities owned and updated by this model instance.
    pub draws: Vec<Vec<Entity>>,
    /// Skin-pose entities owned and updated by this model instance.
    pub poses: Vec<Option<Entity>>,
    /// Morph-weight entities owned and updated by this model instance.
    pub weights: Vec<Option<Entity>>,
    /// Last evaluated world matrix for each node.
    pub matrices: Vec<glam::Mat4>,
    order: Vec<(usize, Option<usize>)>,
    locals: Vec<Transform>,
    morphs: Vec<Vec<f32>>,
}

/// Editable state on a spawned model node.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct LitModelNodeState {
    /// Node index in the model.
    pub node: usize,
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

impl LitModelBundle {
    /// Evaluate the current root transform and playback time without advancing the clock.
    /// Use this when rendering outside the frame dispatcher or driving time explicitly.
    ///
    /// The animation time is sampled as-is, wrapping or clamping only for the
    /// lookup; the stored time is not changed. The ordered [`OnFrame`] callback
    /// is the only path that advances playback.
    pub fn evaluate(scope: &unlit_ecs::Scope<'_>, root: Entity) {
        evaluate_model(scope, root, 0.0, false);
    }
}

/// Resolve parents once, in the same order for entity creation and matrix evaluation.
/// Cut a cycle at the first edge that returns to a node already on the path.
fn hierarchy(nodes: &[LitModelNode]) -> Vec<(usize, Option<usize>)> {
    let mut result = Vec::with_capacity(nodes.len());
    let mut visited = vec![false; nodes.len()];
    let mut attached = vec![false; nodes.len()];
    let mut path = Vec::new();
    for start in 0..nodes.len() {
        let mut index = start;
        while !visited[index] {
            visited[index] = true;
            let parent = nodes[index].parent.filter(|&p| p < nodes.len());
            let parent = parent.filter(|&p| !visited[p] || attached[p]);
            path.push((index, parent));
            let Some(parent) = parent else { break };
            index = parent;
        }
        while let Some((index, parent)) = path.pop() {
            attached[index] = true;
            result.push((index, parent));
        }
    }
    result
}

fn spawn_model(scope: &mut ScopeMut<'_>, root: Entity, model: &LitModel) {
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
    };
    let mut scope = scope.scope_mut(root);
    for &(index, parent) in &instance.order {
        let node = &model.nodes[index];
        let parent = parent.map_or(root, |p| instance.nodes[p]);
        let entity = scope.scope_mut(parent).spawn_named(
            "lit model node",
            bundle![LitModelNodeState {
                node: index,
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
                        .filter(|_| primitive.mesh.skinned)
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
    evaluate_model(&world.root_scope(), root, frame.delta_time, true);
}

fn evaluate_model(scope: &unlit_ecs::Scope<'_>, root: Entity, delta: f32, advance_time: bool) {
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
    } = instance;
    let count = order.len();
    if count != model.nodes.len() || count != matrices.len() {
        return;
    }
    let clip = animation.clip.and_then(|index| model.animations.get(index));
    let sample_time = if let Some(clip) = clip {
        if advance_time {
            animation.time += delta * animation.speed;
        }
        let sample_time = if clip.duration > 0.0 {
            if animation.looping {
                animation.time.rem_euclid(clip.duration)
            } else {
                animation.time.clamp(0.0, clip.duration)
            }
        } else {
            animation.time
        };
        if advance_time {
            animation.time = sample_time;
        }
        Some(sample_time)
    } else {
        None
    };
    for (i, node) in model.nodes.iter().enumerate() {
        let local = nodes
            .get(i)
            .and_then(|&entity| scope.try_query_one::<&LitModelNodeState>(entity).ok());
        match local {
            Some(mut query) => locals[i].clone_from(&query.get().local),
            None => locals[i].clone_from(&node.transform),
        }
        match &node.morph {
            Some(morph) => morphs[i].clone_from(&morph.weights),
            None => morphs[i].clear(),
        }
    }
    if let Some(clip) = clip {
        for channel in &clip.channels {
            let i = channel.node;
            if i >= count {
                continue;
            }
            if let LitModelChannelValues::Weights { targets, .. } = &channel.values
                && *targets != morphs[i].len()
            {
                continue;
            }
            if let Some(sample_time) = sample_time {
                sample_channel(channel, sample_time, &mut locals[i], &mut morphs[i]);
            }
        }
    }
    let root_matrix = glam::Mat4::from(root_transform.compute_matrix());
    for &(i, parent) in order.iter() {
        let local = glam::Mat4::from(locals[i].compute_matrix());
        matrices[i] = parent.map_or(root_matrix, |p| matrices[p]) * local;
    }
    for (i, node) in model.nodes.iter().enumerate() {
        if let Some(entities) = draws.get(i).filter(|draws| !draws.is_empty()) {
            let transform = Transform::from_matrix(glam::Affine3A::from_mat4(matrices[i]));
            for &entity in entities {
                if let Ok(mut q) = scope.try_query_one::<&mut Transform>(entity) {
                    q.get().clone_from(&transform);
                }
            }
        }
        if let Some(Some(entity)) = poses.get(i)
            && let Some(skin) = &node.skin
            && let Ok(mut q) = scope.try_query_one::<&mut SkinPose>(*entity)
        {
            let pose = &mut q.get().matrices;
            pose.clear();
            pose.extend(skin.joints.iter().enumerate().map(|(j, &joint)| {
                matrices.get(joint).copied().unwrap_or(root_matrix)
                    * skin
                        .inverse_bind
                        .get(j)
                        .copied()
                        .unwrap_or(glam::Mat4::IDENTITY)
            }));
        }
        if let Some(Some(entity)) = weights.get(i)
            && let Ok(mut q) = scope.try_query_one::<&mut MorphWeights>(*entity)
        {
            q.get().weights.clone_from(&morphs[i]);
        }
    }
}
