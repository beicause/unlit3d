//! The component registry: which components a JSON bridge can address.
//!
//! A component is any 'static type, so a world alone cannot name one. A
//! component becomes addressable by submitting a [ComponentEntry] to a
//! link-time table; the entry is the only place its name and its JSON shape are
//! written down.
//!
//! For a component that derives [facet::Facet] the entry carries no field list
//! at all: its own struct fields are the JSON shape, and [ComponentEntry::new]
//! turns that reflection into the codec. A component whose fields cannot be
//! reflected — a GPU handle, a private field — reflects through a proxy instead,
//! and one that cannot be written makes its proxy's conversion fail, so a write
//! reports why rather than inventing a value.
//!
//! Entries are collected from wherever they are written, so a crate that
//! defines its own components registers them itself -- nothing has to be handed
//! a registry. The table is link-time, so a crate whose entries are never
//! reachable from the binary is not linked in either; a binary that wants a
//! crate's entries has to name that crate.

use facet::Facet;
use facet::Partial;
use unlit_ecs::{ArchetypeBuilder, Entity, World};

use crate::components::{
    Camera, GpuMaterial, GpuMesh, GpuRenderPipeline, MorphBinding, MorphWeights, RenderLoadOps,
    SkinBinding, SkinPose, Transform, ZSortedDrawing,
};
use crate::input::InputState;
use crate::unlit::{InstanceColor, InstanceCutoff, UnlitPipelineKey};

/// One component the JSON bridge can address.
///
/// Build one with [ComponentEntry::new] or [ComponentEntry::default] and submit
/// it with `inventory::submit!`. Both constructors are `const`, so a submission
/// is a static initializer and costs nothing at run time.
#[derive(Debug)]
pub struct ComponentEntry {
    /// The bare type name a client addresses the component by.
    name: &'static str,
    /// The module the type is defined in, which with the name addresses the
    /// component too and tells two same-named types apart.
    module_path: Option<&'static str>,
    /// The component's JSON text, or a message saying why it could not be read.
    encode: fn(&World, Entity) -> Result<String, String>,
    /// Overwrite the component from JSON text, or a message saying why not.
    set: fn(&World, Entity, &str) -> Result<(), String>,
    /// Push a component decoded from JSON text into a builder.
    push: fn(&mut ArchetypeBuilder, &str) -> Result<(), String>,
}

impl ComponentEntry {
    /// The entry for C, using the reflection C derives.
    ///
    /// Spawning a C from a partial value needs a base to merge it over; a C
    /// that is [Default] is submitted with [ComponentEntry::default] instead.
    pub const fn new<C: Facet<'static> + Clone>() -> Self {
        Self {
            name: C::SHAPE.type_identifier,
            module_path: C::SHAPE.module_path,
            encode: encode_component::<C>,
            set: set_component::<C>,
            push: push_component::<C>,
        }
    }

    /// The entry for a [Default] C, using the reflection C derives.
    ///
    /// The default is what a partial value given to a spawn is merged over, so
    /// a C submitted this way can be spawned from only the fields that matter.
    pub const fn default<C: Facet<'static> + Default + Clone>() -> Self {
        Self {
            name: C::SHAPE.type_identifier,
            module_path: C::SHAPE.module_path,
            encode: encode_component::<C>,
            set: set_component::<C>,
            push: push_default_component::<C>,
        }
    }

    /// The bare type name a client addresses the component by.
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

inventory::collect!(ComponentEntry);

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
inventory::submit! { ComponentEntry::new::<InputState>() }

/// Every submitted entry.
pub fn entries() -> impl Iterator<Item = &'static ComponentEntry> {
    inventory::iter::<ComponentEntry>.into_iter()
}

/// The bare names of every submitted component.
pub fn names() -> impl Iterator<Item = &'static str> {
    entries().map(ComponentEntry::name)
}

/// Whether a component with this name is submitted.
///
/// The name may be the bare type name or the module-qualified one.
pub fn contains(name: &str) -> bool {
    find(name).is_some()
}

/// The entry a name addresses, or a message naming the ones that exist.
pub fn entry(name: &str) -> Result<&'static ComponentEntry, String> {
    find(name).ok_or_else(|| {
        format!(
            "unknown component {name:?}; known components: {}",
            names().collect::<Vec<_>>().join(", ")
        )
    })
}

fn find(name: &str) -> Option<&'static ComponentEntry> {
    let (module, bare) = match name.rsplit_once("::") {
        Some((module, bare)) => (Some(module), bare),
        None => (None, name),
    };
    entries().find(|entry| {
        entry.name == bare && module.is_none_or(|module| entry.module_path == Some(module))
    })
}

/// Encode entity's name component, or a message if either is missing.
pub fn encode(world: &World, entity: Entity, name: &str) -> Result<String, String> {
    (entry(name)?.encode)(world, entity)
}

/// Overwrite entity's name component from JSON text.
pub fn set(world: &World, entity: Entity, name: &str, patch: &str) -> Result<(), String> {
    (entry(name)?.set)(world, entity, patch)
}

/// Push a name component decoded from JSON text into builder.
pub fn push(builder: &mut ArchetypeBuilder, name: &str, patch: &str) -> Result<(), String> {
    (entry(name)?.push)(builder, patch)
}

/// A value as JSON text, through the reflection it derives.
fn to_json<C: Facet<'static>>(value: &C) -> Result<String, String> {
    facet_json::to_string(value).map_err(|error| error.to_string())
}

fn encode_component<C: Facet<'static>>(world: &World, entity: Entity) -> Result<String, String> {
    let component = world.get::<C>(entity).ok_or_else(|| missing::<C>(entity))?;
    to_json(&*component)
}

fn set_component<C: Facet<'static> + Clone>(
    world: &World,
    entity: Entity,
    patch: &str,
) -> Result<(), String> {
    // Seed the new value with the one the entity holds, then let the patch
    // overwrite the fields it names: a patch that names some fields keeps the
    // rest, which is what setting one property at a time means.
    let current = world
        .get::<C>(entity)
        .map(|component| (*component).clone())
        .ok_or_else(|| missing::<C>(entity))?;
    let value: C = apply(current, patch)?;
    world
        .with_mut::<C, _>(entity, |component| *component = value)
        .ok_or_else(|| missing::<C>(entity))
}

/// The value a patch produces over `base`.
fn apply<C: Facet<'static> + Clone>(base: C, patch: &str) -> Result<C, String> {
    let partial = Partial::alloc_owned::<C>().map_err(|error| error.to_string())?;
    let partial = partial.set(base).map_err(|error| error.to_string())?;
    let partial = facet_json::from_str_into(patch, partial).map_err(|error| error.to_string())?;
    materialize(partial)
}

fn push_component<C: Facet<'static> + Clone>(
    builder: &mut ArchetypeBuilder,
    patch: &str,
) -> Result<(), String> {
    let partial = Partial::alloc_owned::<C>().map_err(|error| error.to_string())?;
    let partial = facet_json::from_str_into(patch, partial).map_err(|error| error.to_string())?;
    builder.push(materialize::<C>(partial)?);
    Ok(())
}

fn push_default_component<C: Facet<'static> + Default + Clone>(
    builder: &mut ArchetypeBuilder,
    patch: &str,
) -> Result<(), String> {
    builder.push(apply(C::default(), patch)?);
    Ok(())
}

fn materialize<C: Facet<'static>>(partial: Partial<'static, false>) -> Result<C, String> {
    let heap = partial.build().map_err(|error| error.to_string())?;
    heap.materialize::<C>().map_err(|error| error.to_string())
}

fn missing<C: Facet<'static>>(entity: Entity) -> String {
    format!(
        "entity {entity:?} has no {} component",
        C::SHAPE.type_identifier
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
