//! The component registry: which components a JSON bridge can address.
//!
//! A component is any `'static` type, so a world alone cannot name one. A
//! component becomes addressable by submitting a [`ComponentEntry`] to a
//! link-time table; the entry is the only place its name and its JSON shape are
//! written down.
//!
//! For a component that derives [`facet::Facet`] the entry carries no field list
//! at all: its own struct fields are the JSON shape, and [`ComponentEntry::new`]
//! turns that reflection into the codec. A component whose fields cannot be
//! reflected -- a GPU handle, a private field -- reflects through a proxy instead,
//! and one that cannot be written makes its proxy's conversion fail, so a write
//! reports why rather than inventing a value.
//!
//! Entries are collected from wherever they are written, so a crate that defines
//! its own components registers them itself -- nothing has to be handed a
//! registry. The table is link-time, so a crate whose entries are never
//! reachable from the binary is not linked in either; a binary that wants a
//! crate's entries has to name that crate.

use facet::Facet;
use facet::Partial;

#[cfg(test)]
use crate::World;
use crate::{DynamicBundle, Entity, EntityAccessError, Scope};

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
    encode: fn(&Scope<'_>, Entity) -> Result<String, String>,
    /// Overwrite the component from JSON text, or a message saying why not.
    set: fn(&Scope<'_>, Entity, &str) -> Result<(), String>,
    /// Push a component decoded from JSON text into a builder.
    push: fn(&mut DynamicBundle, &str) -> Result<(), String>,
    /// Whether a spawn may carry the component, as opposed to one the world
    /// itself writes.
    spawnable: bool,
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
            spawnable: true,
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
            spawnable: true,
        }
    }

    /// The entry for a component the world writes itself, which a spawn may
    /// not carry.
    ///
    /// [`ScopeOf`](crate::ScopeOf) is the one such component: an entity's scope
    /// comes from the scope it is spawned in, so a bundle naming it would be a
    /// second source for the same fact. The entry is still submitted, because a
    /// client reads it like any other component.
    pub const fn scope() -> Self {
        Self {
            name: <crate::ScopeOf as Facet<'static>>::SHAPE.type_identifier,
            module_path: <crate::ScopeOf as Facet<'static>>::SHAPE.module_path,
            encode: encode_component::<crate::ScopeOf>,
            set: set_world_written::<crate::ScopeOf>,
            push: push_world_written::<crate::ScopeOf>,
            spawnable: false,
        }
    }

    /// The bare type name a client addresses the component by.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Whether a spawn may carry the component.
    ///
    /// `false` for a component the world writes itself; the push reports why.
    pub const fn spawnable(&self) -> bool {
        self.spawnable
    }
}

inventory::collect!(ComponentEntry);

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
pub fn encode(scope: &Scope<'_>, entity: Entity, name: &str) -> Result<String, String> {
    (entry(name)?.encode)(scope, entity)
}

/// Overwrite entity's name component from JSON text.
pub fn set(scope: &Scope<'_>, entity: Entity, name: &str, patch: &str) -> Result<(), String> {
    (entry(name)?.set)(scope, entity, patch)
}

/// Push a name component decoded from JSON text into builder.
pub fn push(builder: &mut DynamicBundle, name: &str, patch: &str) -> Result<(), String> {
    (entry(name)?.push)(builder, patch)
}

/// A value as JSON text, through the reflection it derives.
fn to_json<C: Facet<'static>>(value: &C) -> Result<String, String> {
    facet_json::to_string(value).map_err(|error| error.to_string())
}

fn encode_component<C: Facet<'static>>(
    scope: &Scope<'_>,
    entity: Entity,
) -> Result<String, String> {
    let component = scope
        .query_one::<&C>(entity)
        .map_err(|error| missing::<C>(entity, error))?;
    to_json(&*component)
}

fn set_component<C: Facet<'static> + Clone>(
    scope: &Scope<'_>,
    entity: Entity,
    patch: &str,
) -> Result<(), String> {
    // Seed the new value with the one the entity holds, then let the patch
    // overwrite the fields it names: a patch that names some fields keeps the
    // rest, which is what setting one property at a time means.
    let current = scope
        .query_one::<&C>(entity)
        .map(|component| (*component).clone())
        .map_err(|error| missing::<C>(entity, error))?;
    let value: C = apply(current, patch)?;
    scope
        .query_one::<&mut C>(entity)
        .map(|mut component| *component = value)
        .map_err(|error| missing::<C>(entity, error))
}

/// The value a patch produces over `base`.
fn apply<C: Facet<'static> + Clone>(base: C, patch: &str) -> Result<C, String> {
    let partial = Partial::alloc_owned::<C>().map_err(|error| error.to_string())?;
    let partial = partial.set(base).map_err(|error| error.to_string())?;
    let partial = facet_json::from_str_into(patch, partial).map_err(|error| error.to_string())?;
    materialize(partial)
}

fn push_component<C: Facet<'static> + Clone>(
    builder: &mut DynamicBundle,
    patch: &str,
) -> Result<(), String> {
    let partial = Partial::alloc_owned::<C>().map_err(|error| error.to_string())?;
    let partial = facet_json::from_str_into(patch, partial).map_err(|error| error.to_string())?;
    builder.push_component(materialize::<C>(partial)?);
    Ok(())
}

fn push_default_component<C: Facet<'static> + Default + Clone>(
    builder: &mut DynamicBundle,
    patch: &str,
) -> Result<(), String> {
    builder.push_component(apply(C::default(), patch)?);
    Ok(())
}

fn set_world_written<C: Facet<'static>>(
    _view: &Scope<'_>,
    _entity: Entity,
    _patch: &str,
) -> Result<(), String> {
    // Writing the component alone would leave the scope index pointing at the
    // old parent, so the two sources of truth would disagree. The world moves
    // an entity with `link` instead.
    Err(format!(
        "{} is written by the world, not set directly: link the entity to move it",
        C::SHAPE.type_identifier,
    ))
}

fn push_world_written<C: Facet<'static>>(
    _builder: &mut DynamicBundle,
    _patch: &str,
) -> Result<(), String> {
    Err(format!(
        "{} is written by the world, not by a spawn: pass the scope to the spawn instead",
        C::SHAPE.type_identifier,
    ))
}

fn materialize<C: Facet<'static>>(partial: Partial<'static, false>) -> Result<C, String> {
    let heap = partial.build().map_err(|error| error.to_string())?;
    heap.materialize::<C>().map_err(|error| error.to_string())
}

/// Says which component was asked for and why the lookup failed: the handle
/// may be dead or out of the scope's reach, and each has its own reply.
fn missing<C: Facet<'static>>(entity: Entity, error: EntityAccessError) -> String {
    format!(
        "entity {entity:?} has no {} component: {error}",
        C::SHAPE.type_identifier
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle;

    /// A component defined here, so the registry is tested on its own rather
    /// than through a crate above it.
    #[derive(Clone, Debug, Default, facet::Facet)]
    struct Position {
        x: f32,
        y: f32,
    }

    #[derive(Clone, Copy, Debug, Default, facet::Facet)]
    struct Name(&'static str);

    inventory::submit! { ComponentEntry::default::<Position>() }

    /// The reflected fields are the codec: a component's own struct is the only
    /// place its field list is written down, so reading and writing cannot
    /// drift.
    #[test]
    fn a_reflected_component_round_trips_through_its_own_fields() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(bundle![Position::default()]);

        set(&world.root_scope(), entity, "Position", r#"{"x":1.0}"#)
            .expect("a partial value is accepted");

        // The fields the patch did not name keep their current value, which is
        // the whole point of setting one property at a time.
        let encoded =
            encode(&world.root_scope(), entity, "Position").expect("the component encodes");
        let value: Position = facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(value.x, 1.0);
        assert_eq!(value.y, 0.0);
    }

    /// The module-qualified name addresses the same component as the bare one,
    /// so a client that spells the whole path is not turned away.
    #[test]
    fn a_qualified_name_addresses_the_same_component() {
        let qualified = format!(
            "{}::{}",
            Position::SHAPE.module_path.expect("a module"),
            Position::SHAPE.type_identifier
        );
        assert!(contains("Position"));
        assert!(contains(&qualified));
        assert!(!contains("unlit_ecs::other::Position"));
    }

    /// Spawning merges the given fields over the component's default, so only
    /// the fields that matter have to be given.
    #[test]
    fn spawning_merges_over_the_default() {
        let mut builder = DynamicBundle::new().named("reflect test");
        push(&mut builder, "Position", r#"{"x":4.0}"#).expect("a partial value is accepted");
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(builder);
        let encoded =
            encode(&world.root_scope(), entity, "Position").expect("the spawned component encodes");
        let value: Position = facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(value.x, 4.0);
        assert_eq!(value.y, 0.0);
    }

    /// A component that is not submitted is reported, and the message names the
    /// ones that are.
    #[test]
    fn an_unknown_component_names_the_known_ones() {
        let error = entry("Nope").expect_err("an unknown component is refused");
        assert!(error.contains("Nope"), "{error}");
        assert!(error.contains("Position"), "{error}");
    }

    /// A component the entity does not carry is reported rather than encoded as
    /// an empty value.
    #[test]
    fn a_component_the_entity_lacks_is_reported() {
        let world = World::new();
        let entity = Entity::from_raw(0, 0);
        let error =
            encode(&world.root_scope(), entity, "Position").expect_err("the entity has nothing");
        assert!(error.contains("has no Position"), "{error}");
    }

    /// The reason the access was refused travels with the message, so a caller
    /// can tell a dead handle from one the scope cannot see or one that lacks
    /// the component: they have different replies.
    #[test]
    fn a_refused_access_names_its_reason() {
        let mut world = World::new();
        let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
        let outside = world.root_scope_mut().spawn(bundle![Position::default()]);
        let dead = world.root_scope_mut().spawn(bundle![Position::default()]);
        world.root_scope_mut().despawn(dead).unwrap();

        let hidden = encode(&world.scope(scene).unwrap(), outside, "Position")
            .expect_err("the scene cannot see it");
        assert!(hidden.contains("is not visible from"), "{hidden}");
        assert!(hidden.contains("Position"), "{hidden}");

        let gone = encode(&world.root_scope(), dead, "Position").expect_err("it is gone");
        assert!(gone.contains("names no live entity"), "{gone}");
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
        assert!(count >= 1, "a submitted component is listed");
    }
}
