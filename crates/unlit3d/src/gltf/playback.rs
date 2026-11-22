//! Asking what a document's animations carry.
//!
//! A document's animations name channels over its nodes. This module decodes
//! them into the shared model representation and answers the questions a caller
//! asks before choosing one; the sampling itself belongs to the model that
//! plays them, not to the document they came from.

use super::animation::channels;
use super::{AnimationId, LitGltf};
use crate::lit::LitModelChannel;

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
