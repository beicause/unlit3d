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
/// A target is the whole set of attachments a pass draws into, so the id names
/// each of them: the color node, the depth node and the multisample node. A
/// pass that adds depth or multisampling to a color view draws into another
/// target than one that does not, even though both resolve into the same view
/// — depth loads, clears and sample counts belong to the attachment set, not
/// to the view it resolves into.
///
/// Each node keeps its id for the target's whole life: a window's swap chain
/// replaces the view in place each frame, and an offscreen target replaces its
/// color and its views in place on a resize. The id therefore survives the
/// resize and the frames in between, and changes only when the pass starts
/// drawing somewhere else.
///
/// Two passes that draw into one target share an id and do not share a
/// [`PassId`]: that is the difference a source keying per-target state needs,
/// and the reason a surface key can stand in for neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTargetId {
    /// The world the target's resource graph belongs to.
    pub world: WorldId,
    /// The color attachment's resource node, if the pass has one.
    pub color: Option<ResId>,
    /// The depth attachment's resource node, if the pass has one.
    pub depth: Option<ResId>,
    /// The multisample attachment's resource node, if the pass has one.
    pub msaa: Option<ResId>,
}

impl RenderTargetId {
    /// Name the target a pass drawing into these attachments draws into, or
    /// `None` when it draws nowhere.
    ///
    /// The handles are the pass's own attachment views, in the order a pass
    /// states them: color, depth, multisample. Any subset may be present — a
    /// prepass has only depth, an unmultisampled pass has no third — but at
    /// least one has to be, since a pass with no attachment records nothing.
    #[must_use]
    pub fn of(
        world: &World,
        color: Option<&ResHandle<TextureView>>,
        depth: Option<&ResHandle<TextureView>>,
        msaa: Option<&ResHandle<TextureView>>,
    ) -> Option<Self> {
        if color.is_none() && depth.is_none() && msaa.is_none() {
            return None;
        }
        Some(Self {
            world: world.id(),
            color: color.map(ResHandle::id),
            depth: depth.map(ResHandle::id),
            msaa: msaa.map(ResHandle::id),
        })
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
        let named = |world: &World| RenderTargetId {
            world: world.id(),
            color: Some(ResId::from_bits(1)),
            depth: None,
            msaa: None,
        };

        // Two worlds never hand out the same id, so a target named in one is
        // never mistaken for a target named in the other.
        assert_ne!(one.id(), two.id());
        assert_ne!(named(&one), named(&two));
    }

    #[test]
    fn a_target_is_its_whole_attachment_set() {
        let world = World::new();
        let color = Some(ResId::from_bits(1));
        let depth = Some(ResId::from_bits(2));
        let target = |color, depth| RenderTargetId {
            world: world.id(),
            color,
            depth,
            msaa: None,
        };

        // The same color view with and without depth is two targets: what a
        // pass clears and depth-tests through belongs to the set, not to the
        // view it resolves into.
        assert_ne!(target(color, None), target(color, depth));
        assert_eq!(target(color, depth), target(color, depth));
    }

    #[test]
    fn a_pass_that_draws_nowhere_names_no_target() {
        let world = World::new();

        assert_eq!(
            RenderTargetId::of(&world, None, None, None),
            None,
            "a parked pass draws into no target"
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
