//! Samples playback and writes the derived pose of a model instance.

use unlit_ecs::{Entity, Scope, World};

use super::{
    LitModel, LitModelAnimation, LitModelClock, LitModelInstance, LitModelNodeState, LitModelOutput,
};
use crate::components::{MorphWeights, SkinPose, Transform};
use crate::lit::sampling::sample_channel;
use crate::time::{AnimationTime, FrameDelta};

/// Whether an evaluation moves the clip time or only samples it.
#[derive(Clone, Copy, Debug)]
pub(super) enum Step {
    /// Advance by this frame delta when the clock is playing.
    Advance(FrameDelta),
    /// Sample the stored time as written.
    Sample,
}

pub(super) fn update_model(world: &World, root: Entity, frame: &mut crate::frame::Frame) {
    evaluate_model(&world.root_scope(), root, Step::Advance(frame.delta_time));
}

pub(super) fn evaluate_model(scope: &Scope<'_>, root: Entity, step: Step) {
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
    // A manual clock is the caller's to sample. The frame dispatcher neither
    // advances it nor evaluates the instance, so the pose a frame draws is the
    // one the caller last sampled, whatever order it wrote the time in and
    // wherever the model sits in the frame's own order.
    if animation.clock == LitModelClock::Manual && matches!(step, Step::Advance(_)) {
        return;
    }
    let clip = animation.clip.and_then(|index| model.animations.get(index));
    let sample_time = clip.map(|clip| {
        let mut time = f64::from(animation.time.as_secs());
        if let LitModelClock::Playing { speed, looping } = animation.clock {
            if let Step::Advance(delta) = step
                && speed.is_finite()
            {
                time += f64::from(delta.as_secs_f32()) * f64::from(speed);
            }
            let duration = f64::from(clip.duration);
            if duration.is_finite() && duration > 0.0 {
                time = if looping {
                    time.rem_euclid(duration)
                } else {
                    time.clamp(0.0, duration)
                };
            }
        }
        let time = AnimationTime::from_secs(time.min(f64::from(f32::MAX)) as f32);
        if matches!(step, Step::Advance(_)) {
            animation.time = time;
        }
        time.as_secs()
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
