//! The errors that an access naming an entity or an archetype answers with.

use unlit_ecs::{ArchetypeAccessError, Entity, EntityAccessError, Has, World, bundle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marker(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Name(&'static str);

/// A world with a scene, a child of that scene, and a sibling scene.
fn fixture() -> (World, Entity, Entity, Entity) {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    let child = world.scope_mut(scene).unwrap().spawn(bundle![Marker(1)]);
    let sibling = world.root_scope_mut().spawn(bundle![Name("other")]);
    (world, scene, child, sibling)
}

#[test]
fn a_handle_that_names_no_live_entity_is_reported() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    let dead = world.scope_mut(scene).unwrap().spawn(bundle![Marker(1)]);
    assert!(world.scope_mut(scene).unwrap().despawn(dead));

    let not_alive = EntityAccessError::NotAlive { entity: dead };
    assert_eq!(world.scope_mut(dead).err().unwrap(), not_alive);
    let scope = world.scope(scene).unwrap();
    assert_eq!(scope.location(dead).unwrap_err(), not_alive);
    assert_eq!(scope.query_one::<&Marker>(dead).unwrap_err(), not_alive);
    assert_eq!(scope.archetype_of(dead).unwrap_err(), not_alive);
    assert_eq!(scope.entity_name(dead).unwrap_err(), not_alive);
    assert_eq!(scope.entity_info(dead).unwrap_err(), not_alive);
    assert_eq!(scope.parent_scope(dead).unwrap_err(), not_alive);
    assert_eq!(world.entity_name(dead).unwrap_err(), not_alive);
    assert_eq!(world.parent_scope(dead).unwrap_err(), not_alive);
    assert_eq!(world.scope(dead).unwrap_err(), not_alive);
    assert!(!scope.contains(dead), "the row is gone with the handle");
}

/// A handle whose index was never allocated is turned down the same way.
#[test]
fn a_handle_of_an_index_that_was_never_allocated_is_reported() {
    let world = World::new();
    let never = Entity::from_raw(u32::MAX, 0);
    assert_eq!(
        world.root_scope().location(never).unwrap_err(),
        EntityAccessError::NotAlive { entity: never }
    );
}

/// A handle restored from bits carries the right index and generation but no
/// name, and after a despawn its generation is stale.
#[test]
fn a_handle_restored_from_the_bits_of_a_dead_entity_is_reported() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![Marker(1)]);
    world.root_scope_mut().despawn(entity);
    let restored = Entity::from_bits(entity.to_bits());
    assert_eq!(
        world.root_scope().location(restored).unwrap_err(),
        EntityAccessError::NotAlive { entity: restored }
    );
}

#[test]
fn a_handle_outside_the_scope_is_not_visible() {
    let (world, scene, _, sibling) = fixture();
    let not_visible = EntityAccessError::NotVisible {
        entity: sibling,
        scope: scene,
    };
    let scope = world.scope(scene).unwrap();
    assert_eq!(scope.location(sibling).unwrap_err(), not_visible);
    assert_eq!(scope.query_one::<&Name>(sibling).unwrap_err(), not_visible);
    assert_eq!(scope.archetype_of(sibling).unwrap_err(), not_visible);
    assert_eq!(scope.entity_name(sibling).unwrap_err(), not_visible);
    assert_eq!(scope.entity_info(sibling).unwrap_err(), not_visible);
    assert_eq!(scope.parent_scope(sibling).unwrap_err(), not_visible);
    assert_eq!(scope.scope(sibling).unwrap_err(), not_visible);
    assert_eq!(
        world.scope(scene).unwrap().scope(sibling).err().unwrap(),
        not_visible
    );
    assert!(!scope.contains(sibling), "a sibling scene is out of reach");
}

#[test]
fn a_query_the_entity_does_not_match_is_reported() {
    let (world, scene, child, _) = fixture();
    let scope = world.scope(scene).unwrap();
    assert_eq!(
        scope.query_one::<&Name>(child).unwrap_err(),
        EntityAccessError::NotMatched { entity: child },
        "the child carries a marker, not a name"
    );
    // `Has` asks the archetype rather than the row, so a component the
    // archetype does not carry is answered rather than turned down.
    assert!(!scope.query_one::<Has<Name>>(child).unwrap());
}

#[test]
fn an_archetype_id_that_was_never_registered_is_reported() {
    let world = World::new();
    let no_such = ArchetypeAccessError::NoSuchArchetype { id: u32::MAX };
    assert_eq!(world.archetypes().get(u32::MAX).unwrap_err(), no_such);
    assert_eq!(
        world.root_scope().archetype(u32::MAX).unwrap_err(),
        no_such,
        "a scope turns the same id down the same way"
    );
}

#[test]
fn an_archetype_that_holds_no_row_is_reported() {
    let world = World::new();
    // The empty archetype is registered first, and nothing ever spawns into
    // it: a component-less bundle is the one way in, and a spawn carries the
    // scope the world writes.
    assert!(world.archetypes().get(0).unwrap().is_empty());
    assert_eq!(
        world.root_scope().archetype(0).unwrap_err(),
        ArchetypeAccessError::Empty { id: 0 },
        "the pool has it, but there is no row to hand out"
    );
}

#[test]
fn an_archetype_with_rows_beyond_the_scope_is_reported() {
    let (mut world, scene, _, sibling) = fixture();
    // The same component set spawned under another scope lands in the same
    // archetype, which the scene then sees only in part.
    world.scope_mut(sibling).unwrap().spawn(bundle![Marker(2)]);
    let scope = world.scope(scene).unwrap();
    let child = scope.children(scene)[0];
    let id = scope.location(child).unwrap().archetype();
    assert_eq!(
        scope.archetype(id).unwrap_err(),
        ArchetypeAccessError::NotVisible { id, scope: scene },
        "handing out the row list would report a row the scene cannot reach"
    );
}

#[test]
fn an_archetype_error_passes_through_an_entity_error() {
    let error = EntityAccessError::from(ArchetypeAccessError::Empty { id: 0 });
    assert_eq!(error.to_string(), "archetype 0 holds no row");
    assert!(matches!(error, EntityAccessError::Archetype(_)));
}

#[test]
fn every_message_names_the_handle_it_was_asked_about() {
    let (world, scene, child, sibling) = fixture();
    let scope = world.scope(scene).unwrap();
    let never = Entity::from_raw(u32::MAX, 0);

    assert_eq!(
        scope.location(never).unwrap_err().to_string(),
        format!("{never:?} names no live entity"),
        "a handle that never named an entity says so"
    );
    assert_eq!(
        scope.location(sibling).unwrap_err().to_string(),
        format!("{sibling:?} is not visible from {scene:?}"),
        "so does one that is real but out of reach"
    );
    assert_eq!(
        scope.query_one::<&Name>(child).unwrap_err().to_string(),
        format!("{child:?} does not match the query"),
        "and one the query turned down"
    );
    assert_eq!(
        world.archetypes().get(u32::MAX).unwrap_err().to_string(),
        format!("no archetype has id {}", u32::MAX),
    );
}
