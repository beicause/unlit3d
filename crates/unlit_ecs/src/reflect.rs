//! The component registry: which components a bridge can address.
//!
//! A component is any `'static` type, so a world alone cannot name one. A
//! component becomes addressable by submitting a [`ComponentEntry`] to a
//! link-time table; the entry is the only place its name and its codec are
//! written down.
//!
//! The codec speaks [Partial] values rather than text: a caller that has a
//! component in reflection form writes it without ever spelling it out, and one
//! that has it as text fills a partial first. JSON is one such spelling, and
//! [`encode_json`], [`set_json`] and [`push_json`] are that front door; a
//! bridge of another format uses the partials underneath it.
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

use facet::Characteristic;
use facet::Facet;
use facet::Partial;
use facet::{AllocError, ReflectError, ShapeMismatchError};
use facet_format::{DeserializeError, SerializeError};
use facet_json::JsonSerializeError;

#[cfg(test)]
use crate::World;
use crate::{DynamicBundle, Entity, EntityAccessError, Scope};

/// Why the registry could not address a component.
///
/// A lookup fails for its own reason -- an unknown name, a handle the scope
/// cannot reach, a value that will not fit its shape -- so each case keeps the
/// handle, the component name and the underlying error rather than flattening
/// them into one line of text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ComponentError {
    /// No submitted entry answers to this name.
    #[error("unknown component {name:?}; known components: {}", .known.join(", "))]
    Unknown {
        /// The name that was looked up.
        name: String,
        /// The bare names that are submitted.
        known: Vec<&'static str>,
    },

    /// The entity does not carry the component, or this scope cannot reach it.
    #[error("entity {entity:?} has no {component} component: {source}")]
    Missing {
        /// The handle that was looked up.
        entity: Entity,
        /// The component that was asked for.
        component: &'static str,
        /// Why the lookup was refused: a dead handle, one out of the scope's
        /// reach, or one whose archetype lacks the component.
        source: EntityAccessError,
    },

    /// The component is written by the world, so setting it alone would leave
    /// two sources of truth disagreeing.
    #[error("{component} is written by the world, not set directly: link the entity to move it")]
    WorldWritten {
        /// The component that was asked for.
        component: &'static str,
    },

    /// The component is written by the world, so a spawn cannot carry it.
    #[error(
        "{component} is written by the world, not by a spawn: pass the scope to the spawn instead"
    )]
    WorldWrittenOnSpawn {
        /// The component that was asked for.
        component: &'static str,
    },

    /// The reflection could not lay out a value for the component.
    #[error(transparent)]
    Allocate(#[from] AllocError),

    /// A partial value was navigated or built wrongly.
    #[error(transparent)]
    Reflect(#[from] ReflectError),

    /// The built value was not the component it was built as.
    #[error(transparent)]
    Materialize(#[from] ShapeMismatchError),

    /// The component could not be written as JSON.
    #[error("{component} cannot be written as JSON: {}", describe(.source))]
    Encode {
        /// The component that was asked for.
        component: &'static str,
        /// Why the serializer gave up.
        source: SerializeError<JsonSerializeError>,
    },

    /// The text could not be read into the component.
    #[error("{component} cannot be read from JSON: {source}")]
    Decode {
        /// The component that was asked for.
        component: &'static str,
        /// Where in the text the read gave up, and why. Boxed: the deserializer
        /// error is large enough that every result carrying one would grow with
        /// it.
        source: Box<DeserializeError>,
    },
}

/// One component a bridge can address.
///
/// Build one with [ComponentEntry::new] and submit it with
/// `inventory::submit!`. The constructor is `const`, so a submission is a
/// static initializer and costs nothing at run time.
#[derive(Debug)]
pub struct ComponentEntry {
    /// The bare type name a client addresses the component by.
    name: &'static str,
    /// The module the type is defined in, which with the name addresses the
    /// component too and tells two same-named types apart.
    module_path: Option<&'static str>,
    /// The component's value as JSON text, or why it could not be read.
    encode_json: fn(&Scope<'_>, Entity) -> Result<String, ComponentError>,
    /// A partial value over the component's [Default], or over nothing when it
    /// has none.
    default_partial: fn() -> Result<Partial<'static, false>, ComponentError>,
    /// A partial value over the entity's current component, or why it cannot be
    /// read.
    current_partial: fn(&Scope<'_>, Entity) -> Result<Partial<'static, false>, ComponentError>,
    /// Overwrite the component with the value a partial holds.
    set: fn(&Scope<'_>, Entity, Partial<'static, false>) -> Result<(), ComponentError>,
    /// Push the value a partial holds into a builder.
    push: fn(&mut DynamicBundle, Partial<'static, false>) -> Result<(), ComponentError>,
    /// Whether a spawn may carry the component, as opposed to one the world
    /// itself writes.
    spawnable: bool,
}

impl ComponentEntry {
    /// The entry for C, using the reflection C derives.
    ///
    /// Spawning a C from a partial value needs a base to merge it over, which
    /// is C's own [Default] when it has one: a [Default] C is spawned from only
    /// the fields that matter, while a C without one has to name every field.
    /// Which of the two applies is read from the reflection C derives, so a
    /// component that gains a [Default] starts merging over it without the
    /// submission changing.
    pub const fn new<C: Facet<'static> + Clone>() -> Self {
        Self {
            name: C::SHAPE.type_identifier,
            module_path: C::SHAPE.module_path,
            encode_json: encode_json_component::<C>,
            default_partial: default_partial_component::<C>,
            current_partial: current_partial_component::<C>,
            set: set_component::<C>,
            push: push_component::<C>,
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
            encode_json: encode_json_component::<crate::ScopeOf>,
            default_partial: default_partial_component::<crate::ScopeOf>,
            current_partial: current_partial_component::<crate::ScopeOf>,
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

/// The entry a name addresses, or [ComponentError::Unknown] naming the ones
/// that exist.
pub fn entry(name: &str) -> Result<&'static ComponentEntry, ComponentError> {
    find(name).ok_or_else(|| ComponentError::Unknown {
        name: name.to_owned(),
        known: names().collect(),
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

/// Entity's name component as JSON text, or why either is missing.
pub fn encode_json(
    scope: &Scope<'_>,
    entity: Entity,
    name: &str,
) -> Result<String, ComponentError> {
    (entry(name)?.encode_json)(scope, entity)
}

/// Overwrite entity's name component from JSON text.
pub fn set_json(
    scope: &Scope<'_>,
    entity: Entity,
    name: &str,
    patch: &str,
) -> Result<(), ComponentError> {
    let entry = entry(name)?;
    let partial = fill((entry.current_partial)(scope, entity)?, patch, entry.name)?;
    (entry.set)(scope, entity, partial)
}

/// Push a name component decoded from JSON text into builder.
pub fn push_json(
    builder: &mut DynamicBundle,
    name: &str,
    patch: &str,
) -> Result<(), ComponentError> {
    let entry = entry(name)?;
    let partial = fill((entry.default_partial)()?, patch, entry.name)?;
    (entry.push)(builder, partial)
}

/// A partial value over the named component's [Default], ready to be filled and
/// handed to [push].
///
/// A component with a [Default] keeps it in the fields a fill leaves alone,
/// while one without has to name every field. Which of the two applies is read
/// from the reflection, so a component that gains a [Default] starts merging
/// over it without the submission changing.
pub fn default_partial(name: &str) -> Result<Partial<'static, false>, ComponentError> {
    (entry(name)?.default_partial)()
}

/// A partial value over the entity's current component, ready to be filled and
/// handed back to [set].
///
/// The fields a fill leaves alone keep the value the entity holds, which is what
/// setting one property at a time means.
pub fn current_partial(
    scope: &Scope<'_>,
    entity: Entity,
    name: &str,
) -> Result<Partial<'static, false>, ComponentError> {
    (entry(name)?.current_partial)(scope, entity)
}

/// Overwrite entity's name component with the value `partial` holds.
pub fn set(
    scope: &Scope<'_>,
    entity: Entity,
    name: &str,
    partial: Partial<'static, false>,
) -> Result<(), ComponentError> {
    (entry(name)?.set)(scope, entity, partial)
}

/// Push the name component, taken from `partial`, into builder.
pub fn push(
    builder: &mut DynamicBundle,
    name: &str,
    partial: Partial<'static, false>,
) -> Result<(), ComponentError> {
    (entry(name)?.push)(builder, partial)
}

/// The partial a JSON patch produces over `partial`.
fn fill(
    partial: Partial<'static, false>,
    patch: &str,
    component: &'static str,
) -> Result<Partial<'static, false>, ComponentError> {
    facet_json::from_str_into(patch, partial).map_err(|source| ComponentError::Decode {
        component,
        source: Box::new(source),
    })
}

/// A value as JSON text, through the reflection it derives.
fn to_json<C: Facet<'static>>(value: &C) -> Result<String, ComponentError> {
    facet_json::to_string(value).map_err(|source| ComponentError::Encode {
        component: C::SHAPE.type_identifier,
        source,
    })
}

fn encode_json_component<C: Facet<'static>>(
    scope: &Scope<'_>,
    entity: Entity,
) -> Result<String, ComponentError> {
    let component = scope
        .query_one::<&C>(entity)
        .map_err(|error| missing::<C>(entity, error))?;
    to_json(&*component)
}

/// A partial over a fresh C, seeded with C's [Default] when it has one.
fn default_partial_component<C: Facet<'static>>() -> Result<Partial<'static, false>, ComponentError>
{
    let partial = Partial::alloc_owned::<C>()?;
    // A C that is Default merges a fill over its own default, so a spawn may
    // name only the fields that matter. The reflection says whether C has one,
    // which keeps this a runtime question rather than a bound on C.
    if C::SHAPE.is(Characteristic::Default) {
        Ok(partial.set_default()?)
    } else {
        Ok(partial)
    }
}

/// A partial over the entity's current C, seeded with a copy of it.
fn current_partial_component<C: Facet<'static> + Clone>(
    scope: &Scope<'_>,
    entity: Entity,
) -> Result<Partial<'static, false>, ComponentError> {
    let current = scope
        .query_one::<&C>(entity)
        .map(|component| (*component).clone())
        .map_err(|error| missing::<C>(entity, error))?;
    let partial = Partial::alloc_owned::<C>()?;
    Ok(partial.set(current)?)
}

fn set_component<C: Facet<'static>>(
    scope: &Scope<'_>,
    entity: Entity,
    partial: Partial<'static, false>,
) -> Result<(), ComponentError> {
    let mut component = scope
        .query_one::<&mut C>(entity)
        .map_err(|error| missing::<C>(entity, error))?;
    *component = materialize(partial)?;
    Ok(())
}

fn push_component<C: Facet<'static>>(
    builder: &mut DynamicBundle,
    partial: Partial<'static, false>,
) -> Result<(), ComponentError> {
    builder.push_component(materialize::<C>(partial)?);
    Ok(())
}

fn set_world_written<C: Facet<'static>>(
    _scope: &Scope<'_>,
    _entity: Entity,
    _partial: Partial<'static, false>,
) -> Result<(), ComponentError> {
    // Writing the component alone would leave the scope index pointing at the
    // old parent, so the two sources of truth would disagree. The world moves
    // an entity with `link` instead.
    Err(ComponentError::WorldWritten {
        component: C::SHAPE.type_identifier,
    })
}

fn push_world_written<C: Facet<'static>>(
    _builder: &mut DynamicBundle,
    _partial: Partial<'static, false>,
) -> Result<(), ComponentError> {
    Err(ComponentError::WorldWrittenOnSpawn {
        component: C::SHAPE.type_identifier,
    })
}

fn materialize<C: Facet<'static>>(partial: Partial<'static, false>) -> Result<C, ComponentError> {
    let heap = partial.build()?;
    Ok(heap.materialize::<C>()?)
}

/// Says which component was asked for and why the lookup failed: the handle
/// may be dead or out of the scope's reach, and each has its own reply.
fn missing<C: Facet<'static>>(entity: Entity, error: EntityAccessError) -> ComponentError {
    ComponentError::Missing {
        entity,
        component: C::SHAPE.type_identifier,
        source: error,
    }
}

/// The backend's own message, which [SerializeError]'s [Display](core::fmt::Display)
/// leaves out of the `Backend` variant.
fn describe(error: &SerializeError<JsonSerializeError>) -> String {
    match error {
        SerializeError::Backend(backend) => backend.to_string(),
        other => other.to_string(),
    }
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

    /// A component with no [Default], which a spawn therefore cannot merge a
    /// partial value over.
    #[derive(Clone, Debug, facet::Facet)]
    struct Offset {
        distance: f32,
    }

    inventory::submit! { ComponentEntry::new::<Position>() }
    inventory::submit! { ComponentEntry::new::<Offset>() }

    /// The reflected fields are the codec: a component's own struct is the only
    /// place its field list is written down, so reading and writing cannot
    /// drift.
    #[test]
    fn a_reflected_component_round_trips_through_its_own_fields() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(bundle![Position::default()]);

        set_json(&world.root_scope(), entity, "Position", r#"{"x":1.0}"#)
            .expect("a partial value is accepted");

        // The fields the patch did not name keep their current value, which is
        // the whole point of setting one property at a time.
        let encoded =
            encode_json(&world.root_scope(), entity, "Position").expect("the component encodes");
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
        push_json(&mut builder, "Position", r#"{"x":4.0}"#).expect("a partial value is accepted");
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(builder);
        let encoded = encode_json(&world.root_scope(), entity, "Position")
            .expect("the spawned component encodes");
        let value: Position = facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(value.x, 4.0);
        assert_eq!(value.y, 0.0);
    }

    /// A component with no [Default] has nothing to merge a partial value over,
    /// so a spawn has to name every field. Which of the two applies is read
    /// from the reflection, not from the submission.
    #[test]
    fn a_component_without_a_default_must_name_every_field() {
        let mut builder = DynamicBundle::new().named("reflect test");
        push_json(&mut builder, "Offset", r#"{"distance":4.0}"#)
            .expect("a whole value is accepted");
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(builder);
        let encoded = encode_json(&world.root_scope(), entity, "Offset").expect("it encodes");
        let value: Offset = facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(value.distance, 4.0);

        let mut builder = DynamicBundle::new().named("reflect test");
        let error =
            push_json(&mut builder, "Offset", "{}").expect_err("there is no default to merge over");
        assert!(error.to_string().contains("distance"), "{error}");
    }

    /// A caller that already holds a component in reflection form writes it
    /// without spelling it out: the entry takes the partial itself, and text is
    /// only one of the ways to fill one.
    #[test]
    fn a_partial_is_written_without_going_through_text() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(bundle![Position::default()]);

        let partial = current_partial(&world.root_scope(), entity, "Position")
            .expect("the entity carries the component");
        let partial = partial.set_field("y", 2.0_f32).expect("y is a field");
        set(&world.root_scope(), entity, "Position", partial).expect("the partial is written");

        let value = world
            .root_scope()
            .query_one::<&Position>(entity)
            .expect("the component is there");
        assert_eq!(value.x, 0.0);
        assert_eq!(value.y, 2.0);
    }

    /// A component that is not submitted is reported, and the message names the
    /// ones that are.
    #[test]
    fn an_unknown_component_names_the_known_ones() {
        let error = entry("Nope").expect_err("an unknown component is refused");
        assert!(matches!(error, ComponentError::Unknown { .. }), "{error}");
        let message = error.to_string();
        assert!(message.contains("Nope"), "{message}");
        assert!(message.contains("Position"), "{message}");
    }

    /// A component the entity does not carry is reported rather than encoded as
    /// an empty value.
    #[test]
    fn a_component_the_entity_lacks_is_reported() {
        let world = World::new();
        let entity = Entity::from_raw(0, 0);
        let error = encode_json(&world.root_scope(), entity, "Position")
            .expect_err("the entity has nothing");
        assert!(matches!(error, ComponentError::Missing { .. }), "{error}");
        let message = error.to_string();
        assert!(message.contains("has no Position"), "{message}");
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

        let hidden = encode_json(&world.scope(scene).unwrap(), outside, "Position")
            .expect_err("the scene cannot see it");
        assert!(
            matches!(
                hidden,
                ComponentError::Missing {
                    source: EntityAccessError::NotVisible { .. },
                    ..
                }
            ),
            "{hidden}"
        );
        let message = hidden.to_string();
        assert!(message.contains("is not visible from"), "{message}");
        assert!(message.contains("Position"), "{message}");

        let gone = encode_json(&world.root_scope(), dead, "Position").expect_err("it is gone");
        assert!(
            matches!(
                gone,
                ComponentError::Missing {
                    source: EntityAccessError::NotAlive { .. },
                    ..
                }
            ),
            "{gone}"
        );
        assert!(gone.to_string().contains("names no live entity"), "{gone}");
    }

    /// A patch that does not fit the component says which component and why,
    /// rather than only that something went wrong.
    #[test]
    fn a_bad_patch_names_the_component_and_the_field() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(bundle![Position::default()]);

        let error = set_json(&world.root_scope(), entity, "Position", r#"{"x":"one"}"#)
            .expect_err("a number is not a string");
        assert!(matches!(error, ComponentError::Decode { .. }), "{error}");
        let message = error.to_string();
        assert!(message.contains("Position"), "{message}");
        assert!(message.contains("x"), "{message}");
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
