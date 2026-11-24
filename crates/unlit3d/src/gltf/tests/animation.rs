//! Decoding a clip: the channels a document states, and the ones it cannot.

use super::*;
use crate::lit::{LitModelInterpolation, LitModelOutput};

#[test]
fn an_animation_reports_its_name_and_duration() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 1.5, 4.0],
        &[[0.0; 3]; 3],
        &[[0.0, 0.0, 0.0, 1.0]; 3],
    ))
    .expect("the animated document parses");
    assert_eq!(gltf.animation_count(), 1);
    assert_eq!(gltf.animation_name(AnimationId::new(0)), Some("walk"));
    assert!((gltf.animation_duration(AnimationId::new(0)) - 4.0).abs() < 1e-6);
}

#[test]
fn a_clip_states_the_channels_and_keyframes_its_document_carries() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 0.0]],
    ))
    .expect("the animated document parses");
    let clip = gltf.clip(AnimationId::new(0));
    assert_eq!(clip.len(), 2, "one channel per animated property");
    assert_eq!(clip[0].node, 0);
    assert_eq!(clip[0].times, [0.0, 2.0]);
    assert_eq!(clip[0].interpolation, LitModelInterpolation::Linear);
    assert_eq!(
        clip[0].values,
        LitModelOutput::Translation(vec![[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]])
    );
    assert_eq!(clip[1].node, 1);
    assert_eq!(
        clip[1].values,
        LitModelOutput::Rotation(vec![[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 0.0]])
    );
}

#[test]
fn a_weights_channel_short_of_its_keyframes_is_dropped() {
    // One weight where two keyframes and one target ask for two: the channel
    // cannot be sampled, so decoding drops it rather than stating a channel
    // whose values do not cover the times it names.
    let gltf = LitGltf::from_bytes(&animated_morphed_document(&[0.0, 2.0], &[0.5]))
        .expect("the animated morphed document parses");
    assert!(
        gltf.clip(AnimationId::new(0)).is_empty(),
        "a channel whose values fall short of its keyframes is not part of the clip"
    );
}

#[test]
fn a_matrix_node_ignores_the_channels_it_cannot_animate() {
    // The node states its transform as a matrix, which the specification
    // declares to have no animatable properties: sampling the translation
    // channel would move a node the document places with a matrix. Its morph
    // weights are the mesh's, not the transform's, so they still animate.
    let gltf = LitGltf::from_bytes(&matrix_animated_document(
        glam::Mat4::from_translation(glam::Vec3::new(0.0, 5.0, 0.0)).to_cols_array(),
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[0.0, 1.0],
    ))
    .expect("the matrix document parses");
    let clip = gltf.clip(AnimationId::new(0));
    assert_eq!(
        clip.len(),
        1,
        "the matrix node keeps its weights channel and loses its translation one"
    );
    assert_eq!(clip[0].node, 0);
    assert_eq!(
        clip[0].values,
        LitModelOutput::Weights {
            targets: 1,
            values: vec![0.0, 1.0],
        }
    );
}

#[test]
fn a_single_keyframe_channel_holds_its_one_value() {
    // A document may state a channel with a single input: it has no interval to
    // interpolate over, so it holds the value it names for the whole clip. That
    // is the only thing a constant channel can mean, and dropping it would
    // leave the node at rest instead.
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0],
        &[[2.0, 0.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0]],
    ))
    .expect("the single-keyframe document parses");
    let clip = gltf.clip(AnimationId::new(0));
    assert_eq!(clip.len(), 2, "both constant channels survive");
    assert_eq!(clip[0].times, [0.0]);
    assert_eq!(
        clip[0].values,
        LitModelOutput::Translation(vec![[2.0, 0.0, 0.0]]),
        "the one keyframe states the translation the clip holds"
    );
    assert_eq!(
        clip[1].values,
        LitModelOutput::Rotation(vec![[0.0, 0.0, 0.0, 1.0]]),
        "a constant rotation channel holds its quaternion"
    );
}
