//! Decode glTF animation channels into the shared model representation.

use crate::lit::sampling::interpolation_stride;
use crate::lit::{LitModelChannel as Channel, LitModelInterpolation, LitModelOutput};
use gltf::animation::Property;
use gltf::animation::util::ReadOutputs;

/// Every channel of `animation` that can be sampled, with its keyframes decoded.
///
/// A channel the document states incompletely is dropped here rather than on
/// every frame that plays the animation: one whose inputs are missing or name a
/// single keyframe has no interval to interpolate over, and the spec requires
/// two anyway, and one whose outputs cover fewer keyframes than its inputs
/// name cannot be sampled at all. A weights channel is checked against the
/// target count of the mesh its node carries, which `morph_targets` states.
///
/// `matrix_nodes` marks the nodes whose transform the document states as a
/// matrix. Such a node has no animatable translation, rotation or scale — the
/// spec says so outright, because a matrix is not a property a channel can
/// address — so a channel naming one is dropped; its morph weights stay
/// animatable, since they belong to the mesh rather than to the transform.
pub(super) fn channels(
    animation: gltf::animation::Animation<'_>,
    buffers: &[gltf::buffer::Data],
    matrix_nodes: &[bool],
    morph_targets: &[usize],
) -> Vec<Channel> {
    let mut channels = Vec::new();
    for channel in animation.channels() {
        let reader = channel.reader(|buffer| Some(&buffers[buffer.index()]));
        let Some(inputs) = reader.read_inputs() else {
            continue;
        };
        let times: Vec<f32> = inputs.collect();
        if times.len() < 2 {
            continue;
        }
        let Some(outputs) = reader.read_outputs() else {
            continue;
        };
        let interpolation = match channel.sampler().interpolation() {
            gltf::animation::Interpolation::Step => LitModelInterpolation::Step,
            gltf::animation::Interpolation::Linear => LitModelInterpolation::Linear,
            gltf::animation::Interpolation::CubicSpline => LitModelInterpolation::CubicSpline,
        };
        let required = times.len() * interpolation_stride(interpolation);
        let node = channel.target().node().index();
        if matrix_nodes[node] {
            match channel.target().property() {
                Property::Translation | Property::Scale | Property::Rotation => continue,
                Property::MorphTargetWeights => {}
            }
        }
        let values = match (channel.target().property(), outputs) {
            (Property::Translation, ReadOutputs::Translations(values)) => {
                match complete(values, required) {
                    Some(values) => LitModelOutput::Translation(values),
                    None => continue,
                }
            }
            (Property::Scale, ReadOutputs::Scales(values)) => match complete(values, required) {
                Some(values) => LitModelOutput::Scale(values),
                None => continue,
            },
            (Property::Rotation, ReadOutputs::Rotations(values)) => {
                match complete(values.into_f32(), required) {
                    Some(values) => LitModelOutput::Rotation(values),
                    None => continue,
                }
            }
            (Property::MorphTargetWeights, ReadOutputs::MorphTargetWeights(values)) => {
                let targets = morph_targets[node];
                let values: Vec<f32> = values.into_f32().collect();
                // A weights channel states one weight per target per
                // keyframe, so it needs the target count as a multiplier on
                // top of what a transform channel needs.
                if targets == 0 || values.len() < required * targets {
                    continue;
                }
                LitModelOutput::Weights { targets, values }
            }
            _ => continue,
        };
        channels.push(Channel {
            node,
            interpolation,
            times,
            values,
        });
    }
    channels
}

/// The values of a channel that states every keyframe the times name, or
/// nothing when it states fewer — such a channel cannot be sampled.
fn complete<T>(values: impl Iterator<Item = T>, required: usize) -> Option<Vec<T>> {
    let values: Vec<T> = values.collect();
    (values.len() >= required).then_some(values)
}
