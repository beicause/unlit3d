//! Asking what a document's animations carry.
//!
//! A document's animations name channels over its nodes. This module decodes
//! them into the shared model representation and answers the questions a caller
//! asks before choosing one; the sampling itself belongs to the model that
//! plays them, not to the document they came from.

use super::animation::channels;
use super::{AnimationId, Error, LitGltf};
use crate::lit::LitModelChannel;

impl LitGltf {
    /// The number of animations the document carries.
    pub fn animation_count(&self) -> usize {
        self.document.animations().count()
    }

    /// The id of the first animation with `name`.
    #[must_use]
    pub fn animation_id(&self, name: &str) -> Option<AnimationId> {
        self.document
            .animations()
            .enumerate()
            .find_map(|(index, animation)| {
                (animation.name() == Some(name)).then_some(AnimationId::new(index))
            })
    }

    /// The name of an animation, or `None` when it carries none.
    ///
    /// # Errors
    ///
    /// [`Error::AnimationOutsideDocument`] when the document carries no such
    /// animation.
    pub fn animation_name(&self, animation: AnimationId) -> Result<Option<&str>, Error> {
        Ok(self.animation(animation)?.name())
    }

    /// The time of the last decoded keyframe in seconds, or zero for no keys.
    ///
    /// This includes decoded channels targeting nodes outside the default scene.
    ///
    /// # Errors
    ///
    /// [`Error::AnimationOutsideDocument`] when the document carries no such
    /// animation.
    pub fn animation_duration(&self, animation: AnimationId) -> Result<f32, Error> {
        Ok(duration(self.clip(animation)?))
    }

    /// The decoded keyframes of an animation, decoding them on first use.
    ///
    /// # Errors
    ///
    /// [`Error::AnimationOutsideDocument`] when the document carries no such
    /// animation.
    pub(super) fn clip(&self, animation: AnimationId) -> Result<&[LitModelChannel], Error> {
        let index = animation.index();
        let data = self.animation(animation)?;
        Ok(self.clips[index]
            .get_or_init(|| channels(data, &self.buffers, &self.matrix_nodes, &self.morph_targets)))
    }

    fn animation(&self, animation: AnimationId) -> Result<gltf::animation::Animation<'_>, Error> {
        self.document
            .animations()
            .nth(animation.index())
            .ok_or(Error::AnimationOutsideDocument { animation })
    }
}

/// How long a clip lasts: the latest input keyframe across all of its samplers.
///
/// Zero for a clip whose channels carry no keyframes. The one place the length
/// of a clip is computed, so [`LitGltf::animation_duration`] and the clips a
/// model is built from cannot disagree about it.
pub(super) fn duration(channels: &[LitModelChannel]) -> f32 {
    channels
        .iter()
        .filter_map(|channel| channel.times.last().copied())
        .fold(0.0, f32::max)
}
