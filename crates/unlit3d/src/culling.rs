//! CPU visibility culling.
//!
//! The bounding volumes and frustum planes are the reusable primitives in
//! [crate::bounds]; this module is the decision built on top of them: which
//! entities a frame's camera can actually see, and the placement each of them
//! needs to be drawn.

use glam::Affine3A;
#[cfg(test)]
use unlit_ecs::World;
use unlit_ecs::{Entity, Scope};

use crate::bounds::{Aabb, FrustumPlanes};
use crate::components::{GpuMesh, InstanceVisibility, Transform};

/// Whether a mesh whose local bounds are `aabb`, placed by
/// `world_from_local`, lies entirely outside `frustum` and can be skipped.
///
/// The bounds are re-oriented by the transform, so a rotated or non-uniformly
/// scaled entity is tested as the box it really is rather than as its
/// axis-aligned envelope.
pub fn is_culled(aabb: Aabb, world_from_local: &Affine3A, frustum: &FrustumPlanes) -> bool {
    !frustum.test_obb(&aabb.transformed(world_from_local))
}

/// A mesh that passed culling, with its placement resolved.
///
/// Culling resolves only what every family needs to decide *whether* and
/// *where* to draw: the entity and its world transform. What per-instance state
/// a draw carries is the drawing family's own business, so it is not resolved
/// here — a family reads the components it wants, for the entity it is given,
/// while it packs its own instance record.
///
/// A culled entity never reaches a family at all, so nothing is specialized or
/// registered for a mesh the camera cannot see.
#[derive(Clone, Copy)]
pub(crate) struct VisibleMesh {
    /// The entity the mesh belongs to.
    pub(crate) entity: Entity,
    /// The entity's world transform, resolved from its optional [Transform]
    /// and defaulting to the identity.
    pub(crate) world_from_local: Affine3A,
}

/// Collect every mesh entity the frustum can see into `out`, replacing its
/// contents.
///
/// An entity's placement is its optional [Transform], defaulting to the
/// identity. The placement resolved here is the one the frame sorts by, and it
/// is handed to the drawing family, so culling and drawing cannot disagree
/// about where the mesh is.
///
/// An entity whose [InstanceVisibility] says it is hidden is dropped here
/// rather than in a family: culling is the one pass every draw goes through, so
/// a hidden entity costs this test and nothing else — no variant is
/// specialized, no instance record is packed, no handle is interned. An entity
/// without the component is drawn, which keeps the component out of every
/// caller that never hides anything.
pub(crate) fn collect_visible(
    scope: &Scope<'_>,
    frustum: &FrustumPlanes,
    out: &mut Vec<VisibleMesh>,
) {
    out.clear();
    for (gpu_mesh, option, visibility, entity) in scope
        .query::<(
            &GpuMesh,
            Option<&Transform>,
            Option<&InstanceVisibility>,
            Entity,
        )>()
        .iter()
    {
        if visibility.is_some_and(|visibility| !visibility.visible) {
            continue;
        }
        let model = match option {
            Some(transform) => transform.compute_matrix(),
            None => Affine3A::IDENTITY,
        };
        if is_culled(gpu_mesh.aabb, &model, frustum) {
            continue;
        }
        out.push(VisibleMesh {
            entity,
            world_from_local: model,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrayvec::ArrayVec;
    use glam::Vec3;
    use std::rc::Rc;
    use unlit_ecs::bundle;
    use unlit_wgpu::resources::{ResourceGraph, Virtual};
    use unlit_wgpu::specialize::VertexLayout;

    use crate::components::MeshParts;

    fn test_perspective() -> glam::Mat4 {
        glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0)
    }

    fn frustum_at(eye: Vec3) -> FrustumPlanes {
        let view = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y);
        FrustumPlanes::from_clip_from_world(test_perspective() * view)
    }

    #[test]
    fn a_mesh_in_front_of_the_camera_is_visible() {
        let frustum = frustum_at(Vec3::new(0.0, 0.0, 5.0));
        let aabb = Aabb::new(Vec3::ZERO, Vec3::splat(0.5));
        assert!(!is_culled(aabb, &Affine3A::IDENTITY, &frustum));
    }

    #[test]
    fn a_mesh_behind_the_camera_is_culled() {
        let frustum = frustum_at(Vec3::new(0.0, 0.0, 5.0));
        let aabb = Aabb::new(Vec3::new(0.0, 0.0, 20.0), Vec3::splat(0.5));
        assert!(is_culled(aabb, &Affine3A::IDENTITY, &frustum));
    }

    #[test]
    fn a_mesh_moved_out_of_view_by_its_transform_is_culled() {
        let frustum = frustum_at(Vec3::new(0.0, 0.0, 5.0));
        let aabb = Aabb::new(Vec3::ZERO, Vec3::splat(0.5));
        let far = Affine3A::from_translation(Vec3::new(-10.0, 0.0, 0.0));
        assert!(is_culled(aabb, &far, &frustum));
    }

    #[test]
    fn collect_visible_keeps_only_the_visible_meshes() {
        let mut world = World::new();
        // The root is a lifetime entry point this test never removes through,
        // so a bare placeholder node is enough.
        let mut graph = ResourceGraph::new();
        let root = graph.insert(Virtual, None);
        let mesh = GpuMesh {
            parts: Rc::new(MeshParts {
                root,
                vertex_buffers: ArrayVec::new(),
                index_buffer: None,
                bind_group_id: None,
                vertex_allocation: None,
                index_allocation: None,
                morph_deltas_allocation: None,
                metadata_index: 0,
            }),
            vertex_layout: VertexLayout::default(),
            count: 0,
            first: 0,
            base_vertex: 0,
            indexed: false,
            aabb: Aabb::new(Vec3::ZERO, Vec3::splat(0.5)),
            morph_targets: 0,
            skinned: false,
        };
        let near = world
            .root_scope_mut()
            .spawn(bundle![mesh.clone(), Transform::default()]);
        let far = world.root_scope_mut().spawn(bundle![
            mesh,
            Transform {
                translation: Vec3::new(0.0, 0.0, 20.0),
                rotation: glam::Quat::IDENTITY,
                scale: Vec3::ONE,
            },
        ]);
        let frustum = frustum_at(Vec3::new(0.0, 0.0, 5.0));
        let mut out = Vec::new();
        collect_visible(&world.root_scope(), &frustum, &mut out);
        let found: Vec<_> = out.iter().map(|mesh| mesh.entity).collect();
        assert_eq!(found, [near]);
        assert_ne!(found, [far]);
    }

    /// Two entities at the same place, differing only in whether they are
    /// visible: the hidden one is dropped before its bounds are ever tested, so
    /// what it would have cost a family is never paid.
    #[test]
    fn a_hidden_entity_is_not_collected() {
        let mut world = World::new();
        let mut graph = ResourceGraph::new();
        let root = graph.insert(Virtual, None);
        let mesh = GpuMesh {
            parts: Rc::new(MeshParts {
                root,
                vertex_buffers: ArrayVec::new(),
                index_buffer: None,
                bind_group_id: None,
                vertex_allocation: None,
                index_allocation: None,
                morph_deltas_allocation: None,
                metadata_index: 0,
            }),
            vertex_layout: VertexLayout::default(),
            count: 0,
            first: 0,
            base_vertex: 0,
            indexed: false,
            aabb: Aabb::new(Vec3::ZERO, Vec3::splat(0.5)),
            morph_targets: 0,
            skinned: false,
        };
        let shown = world.root_scope_mut().spawn(bundle![
            mesh.clone(),
            Transform::default(),
            InstanceVisibility::default(),
        ]);
        let hidden = world.root_scope_mut().spawn(bundle![
            mesh,
            Transform::default(),
            InstanceVisibility { visible: false },
        ]);
        let frustum = frustum_at(Vec3::new(0.0, 0.0, 5.0));
        let mut out = Vec::new();
        collect_visible(&world.root_scope(), &frustum, &mut out);
        let found: Vec<_> = out.iter().map(|mesh| mesh.entity).collect();
        assert_eq!(found, [shown]);
        assert_ne!(found, [hidden]);
    }

    /// The component states visibility explicitly, and its default is drawn:
    /// a caller that adds it to switch hiding on must not hide everything it
    /// already draws.
    #[test]
    fn the_default_visibility_is_drawn() {
        assert!(InstanceVisibility::default().visible);
    }
}
