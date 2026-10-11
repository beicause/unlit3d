//! Render-world helpers: applying transform snapshots to render entities.
//!
//! A simulation world names the entities it wants moved by their `Entity`
//! bits, publishes a [`TransformFrame`], and the render world's frame
//! behaviour copies each transform onto the entity those bits name — the
//! two worlds meet only through the snapshot.

use glam::{Quat, Vec3};
use unlit_ecs::Entity;
use unlit3d::components::Transform;
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};

use crate::channel::SnapshotReader;

/// The frame-behaviour order the transform applier runs at: before anything
/// that positions a camera relative to moved objects.
pub const APPLY_TRANSFORMS_ORDER: i32 = i32::MIN;

/// One frame's worth of transforms, keyed by render entity bits.
#[derive(Clone, Debug, Default)]
pub struct TransformFrame {
    /// Entity bits with the transform each render entity should take.
    pub transforms: Vec<(u64, Vec3, Quat)>,
}

/// The frame behaviour that applies the newest [`TransformFrame`] from
/// `reader` to the render world's entities.
///
/// Entities whose bits name an entity without a `Transform` are skipped:
/// a pool slot not yet spawned is not an error.
#[must_use]
pub fn apply_transform_frames(reader: SnapshotReader<TransformFrame>) -> OnFrame {
    OnFrame::new(
        FrameBehaviourOrder(APPLY_TRANSFORMS_ORDER),
        move |world, _entity, _frame: &mut Frame| {
            let Some(frame) = reader.latest() else {
                return;
            };
            for (bits, translation, rotation) in &frame.transforms {
                let entity = Entity::from_bits(*bits);
                if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                    transform.translation = *translation;
                    transform.rotation = *rotation;
                }
            }
        },
    )
}
