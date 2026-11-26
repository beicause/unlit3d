//! What a source is drawing into, named so that two of them can be told
//! apart.
//!
//! A frame has more than one of everything a source draws with: passes, render
//! targets, and the frames themselves. A [`SurfaceKey`] names a pipeline
//! specialization, not any of those — two passes over one target share a key,
//! and so do two different targets with the same formats — so a source that
//! keeps state per target cannot key it by the surface without confusing the
//! two.
//!
//! The ids here are what a [`PassInfo`](crate::pass::PassInfo) carries instead:
//! [`PassId`] for the logical pass, [`RenderTargetId`] for the target it draws
//! into, and [`FrameId`] for the frame the pass is being recorded in. Each
//! carries the world it belongs to, so an id read from one world never matches
//! an id read from another.
//!
//! [`SurfaceKey`]: unlit_wgpu::specialize::SurfaceKey

use unlit_ecs::{Entity, World, WorldId};
use unlit_wgpu::resources::{ResHandle, ResId, TextureView};

/// One logical pass, in one world.
///
/// A pass is an entity holding a [`FramePass`](crate::pass::FramePass), and the
/// world it is spawned in is as much a part of its identity as the entity is: a
/// host that holds several render worlds would otherwise confuse two passes
/// that happen to have the same entity id.
///
/// The fields are public so an id can be built where no world is in hand and
/// taken apart where one is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassId {
    /// The world the pass lives in.
    pub world: WorldId,
    /// The pass's entity there.
    pub entity: Entity,
}

/// One render target, in one world.
///
/// A target is identified by its color attachment: the resource node the pass
/// binds as color. That node keeps its id for the target's whole life — a
/// window's swap chain replaces the view in place each frame, and an offscreen
/// target replaces its color and its view in place on a resize — so the id
/// survives the resize and the frames in between, and changes only when the
/// pass starts drawing somewhere else.
///
/// Two passes that draw into one target share an id and do not share a
/// [`PassId`]: that is the difference a source keying per-target state needs,
/// and the reason a surface key can stand in for neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTargetId {
    /// The world the target's resource graph belongs to.
    pub world: WorldId,
    /// The color attachment's resource node.
    pub color: ResId,
}

impl RenderTargetId {
    /// Name the target whose color attachment is `color`.
    #[must_use]
    pub fn of(world: &World, color: &ResHandle<TextureView>) -> Self {
        Self {
            world: world.id(),
            color: color.id(),
        }
    }
}

/// The frame a pass is being recorded in.
///
/// This is the [`FrameClock`](crate::frame::FrameClock)'s index, taken when the
/// renderer started the frame: every pass of one frame sees the same value, and
/// the next frame sees another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(pub u32);

impl core::fmt::Display for FrameId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameId, PassId, RenderTargetId};

    use unlit_ecs::{World, bundle};
    use unlit_wgpu::resources::ResId;

    #[test]
    fn a_pass_of_one_world_is_not_a_pass_of_another() {
        let mut one = World::new();
        let mut two = World::new();
        let pass = one.root_scope_mut().spawn(bundle![]);
        let elsewhere = two.root_scope_mut().spawn(bundle![]);

        let here = PassId {
            world: one.id(),
            entity: pass,
        };
        let there = PassId {
            world: two.id(),
            entity: elsewhere,
        };

        assert_ne!(here, there);
        assert_eq!(
            here,
            PassId {
                world: one.id(),
                entity: pass,
            }
        );
    }

    #[test]
    fn one_worlds_entity_is_not_anothers_target() {
        let one = World::new();
        let two = World::new();

        // Two worlds never hand out the same id, so a target named in one is
        // never mistaken for a target named in the other.
        assert_ne!(one.id(), two.id());
        assert_ne!(
            RenderTargetId {
                world: one.id(),
                color: ResId::from_bits(1),
            },
            RenderTargetId {
                world: two.id(),
                color: ResId::from_bits(1),
            }
        );
    }

    #[test]
    fn a_frame_is_named_by_its_number() {
        assert_eq!(FrameId(7), FrameId(7));
        assert_eq!(FrameId(7), FrameId::default().max(FrameId(7)));
        assert!(FrameId(7) > FrameId(6));
        assert_eq!(FrameId(7).to_string(), "7");
    }
}
