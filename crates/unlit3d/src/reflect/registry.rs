//! The components unlit3d submits to the reflection registry.
//!
//! The registry itself lives in [`unlit_ecs::reflect`]: it is a link-time table
//! over the ECS's own types, and every crate that defines components submits its
//! own entries. This module is what unlit3d adds -- the components whose fields
//! cross a JSON boundary, each submitted with the reflection it derives or
//! through a proxy from [`crate::reflect::proxies`].

use unlit_ecs::reflect::ComponentEntry;

use crate::components::{
    Camera, GpuMaterial, GpuMesh, GpuRenderPipeline, MorphBinding, MorphWeights, RenderLoadOps,
    SkinBinding, SkinPose, Transform, ZSortedDrawing,
};
use crate::input::InputHandle;
use crate::unlit::{InstanceColor, InstanceCutoff, UnlitPipelineKey};

inventory::submit! { ComponentEntry::default::<Transform>() }
inventory::submit! { ComponentEntry::new::<Camera>() }
inventory::submit! { ComponentEntry::new::<RenderLoadOps>() }
inventory::submit! { ComponentEntry::new::<GpuMesh>() }
inventory::submit! { ComponentEntry::new::<GpuMaterial>() }
inventory::submit! { ComponentEntry::new::<GpuRenderPipeline<UnlitPipelineKey>>() }
inventory::submit! { ComponentEntry::default::<ZSortedDrawing>() }
inventory::submit! { ComponentEntry::default::<SkinPose>() }
inventory::submit! { ComponentEntry::default::<MorphWeights>() }
inventory::submit! { ComponentEntry::new::<SkinBinding>() }
inventory::submit! { ComponentEntry::new::<MorphBinding>() }
inventory::submit! { ComponentEntry::default::<InstanceColor>() }
inventory::submit! { ComponentEntry::default::<InstanceCutoff>() }
inventory::submit! { ComponentEntry::new::<InputHandle>() }

#[cfg(test)]
mod tests {
    use super::*;
    use unlit_ecs::reflect::{contains, encode, entry, names, push, set};
    use unlit_ecs::{ArchetypeBuilder, Entity, World};

    /// The reflected fields are the codec: a component's own struct is the only
    /// place its field list is written down, so reading and writing cannot
    /// drift.
    #[test]
    fn a_reflected_component_round_trips_through_its_own_fields() {
        let mut world = World::new();
        let entity = world.spawn((Transform::default(),));

        set(
            &world,
            entity,
            "Transform",
            r#"{"translation":[1.0,2.0,3.0]}"#,
        )
        .expect("a partial transform is accepted");

        // The fields the patch did not name keep their current value, which is
        // the whole point of setting one property at a time.
        let encoded = encode(&world, entity, "Transform").expect("the transform encodes");
        let value: Transform = facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(value.translation, glam::Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(value.scale, glam::Vec3::ONE);
        assert_eq!(value.rotation, glam::Quat::IDENTITY);
    }

    /// The module-qualified name addresses the same component as the bare one,
    /// so a client that spells the whole path is not turned away.
    #[test]
    fn a_qualified_name_addresses_the_same_component() {
        assert!(contains("Transform"));
        assert!(contains("unlit3d::components::Transform"));
        assert!(!contains("unlit3d::other::Transform"));
        let mut world = World::new();
        let entity = world.spawn((Transform::default(),));
        assert!(
            encode(&world, entity, "unlit3d::components::Transform").is_ok(),
            "the qualified name reaches the entry"
        );
    }

    /// Spawning merges the given fields over the component's default, so only
    /// the fields that matter have to be given.
    #[test]
    fn spawning_merges_over_the_default() {
        let mut builder = ArchetypeBuilder::new();
        push(
            &mut builder,
            "Transform",
            r#"{"translation":[4.0,5.0,6.0]}"#,
        )
        .expect("a partial transform is accepted");
        let mut world = World::new();
        let entity = world.spawn(builder);
        let encoded = encode(&world, entity, "Transform").expect("the spawned transform encodes");
        let value: Transform = facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(value.translation, glam::Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(value.scale, glam::Vec3::ONE);
    }

    /// A component that is not submitted is reported, and the message names the
    /// ones that are.
    #[test]
    fn an_unknown_component_names_the_known_ones() {
        let error = entry("Nope").expect_err("an unknown component is refused");
        assert!(error.contains("Nope"), "{error}");
        assert!(error.contains("Transform"), "{error}");
    }

    /// A read-only component reflects, but its proxy refuses to be turned back
    /// into a value, so a write reports why instead of inventing one.
    #[test]
    fn a_read_only_component_reports_its_own_reason() {
        use arrayvec::ArrayVec;
        use std::rc::Rc;
        use unlit_wgpu::resources::{ResourceGraph, Virtual};
        use unlit_wgpu::specialize::VertexLayout;

        use crate::bounds::Aabb;
        use crate::components::MeshParts;

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
            count: 3,
            first: 0,
            base_vertex: 0,
            indexed: false,
            aabb: Aabb::new(glam::Vec3::ZERO, glam::Vec3::splat(0.5)),
            morph_targets: 0,
            skinned: false,
        };
        let entity = world.spawn((mesh,));
        let error = set(&world, entity, "GpuMesh", "{}").expect_err("GpuMesh is read-only");
        assert!(error.contains("read-only"), "{error}");

        let encoded = encode(&world, entity, "GpuMesh").expect("GpuMesh still encodes");
        assert!(encoded.contains("count"), "{encoded}");
    }

    /// A component the entity does not carry is reported rather than encoded as
    /// an empty value.
    #[test]
    fn a_component_the_entity_lacks_is_reported() {
        let world = World::new();
        let entity = Entity::from_raw(0, 0);
        let error = encode(&world, entity, "Transform").expect_err("the entity has nothing");
        assert!(error.contains("has no Transform"), "{error}");
    }

    /// The load ops reflect through a proxy per field, so one may be set while
    /// the others keep what they had.
    #[test]
    fn a_proxied_field_is_set_without_disturbing_the_others() {
        let mut world = World::new();
        let entity = world.spawn((RenderLoadOps::default(),));
        set(
            &world,
            entity,
            "RenderLoadOps",
            r#"{"depth":{"clear":0.5}}"#,
        )
        .expect("a partial load op is accepted");
        let ops = world
            .get::<RenderLoadOps>(entity)
            .expect("the load ops are there");
        assert_eq!(ops.depth, wgpu::LoadOp::Clear(0.5));
        assert_eq!(ops.color, RenderLoadOps::default().color);
    }

    /// Every submitted name is distinct, so addressing one component by name is
    /// unambiguous.
    #[test]
    fn every_submitted_name_is_distinct() {
        let mut names: Vec<&str> = names().collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "component names are unique");
        assert!(
            count >= 14,
            "the built-in components are submitted: {names:?}"
        );
    }
}
