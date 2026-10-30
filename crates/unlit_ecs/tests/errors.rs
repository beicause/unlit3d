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
    assert!(world.scope_mut(scene).unwrap().despawn(dead).is_ok());

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
    world.root_scope_mut().despawn(entity).unwrap();
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
    assert!(!scope.query_one::<Has<Name>>(child).unwrap().get());
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
    let mut world = World::new();
    // An archetype outlives its rows: spawning into a set of components and
    // despawning the entity again leaves the archetype registered and empty.
    let entity = world.root_scope_mut().spawn(bundle![Marker(1)]);
    let id = world.root_scope().location(entity).unwrap().archetype();
    world.root_scope_mut().despawn(entity).unwrap();
    assert!(world.archetypes().get(id).unwrap().is_empty());
    assert_eq!(
        world.root_scope().archetype(id).unwrap_err(),
        ArchetypeAccessError::Empty { id },
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
    let child = scope.children(scene).unwrap()[0];
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

/// Asking for the children of a handle is an access too, so it is turned down
/// the same way instead of answering an empty list that no one can tell apart
/// from a childless scope.
#[test]
fn the_children_of_a_handle_that_is_not_there_are_reported() {
    let (mut world, scene, child, sibling) = fixture();
    world.root_scope_mut().despawn(child).unwrap();
    assert_eq!(
        world.root_scope().children(child).unwrap_err(),
        EntityAccessError::NotAlive { entity: child },
    );
    assert_eq!(
        world.scope(scene).unwrap().children(sibling).unwrap_err(),
        EntityAccessError::NotVisible {
            entity: sibling,
            scope: scene,
        },
        "a sibling scene is beside it, not inside it"
    );
    assert!(
        world
            .scope(scene)
            .unwrap()
            .children(scene)
            .unwrap()
            .is_empty()
    );
}

/// A structural change says which of the reasons it was turned down for, so a
/// caller can answer by asking the scope that does own the entity.
#[test]
fn a_scope_that_does_not_own_the_entity_says_so() {
    let (mut world, _scene, child, sibling) = fixture();
    assert_eq!(
        world
            .scope_mut(sibling)
            .unwrap()
            .despawn(child)
            .unwrap_err(),
        EntityAccessError::NotOwned {
            entity: child,
            scope: sibling,
        },
        "the sibling is beside it, not above it"
    );
    assert_eq!(
        world.root_scope_mut().link(child, sibling),
        Ok(()),
        "only the root owns both ends"
    );
    assert_eq!(world.parent_scope(child), Ok(sibling));
}

/// A dead handle is reported as dead rather than as another scope's: the two
/// have different replies, and the handle is the one thing the caller holds.
#[test]
fn a_dead_handle_is_reported_before_ownership_is_asked() {
    let (mut world, scene, child, sibling) = fixture();
    world.root_scope_mut().despawn(child).unwrap();
    assert_eq!(
        world
            .scope_mut(sibling)
            .unwrap()
            .despawn(child)
            .unwrap_err(),
        EntityAccessError::NotAlive { entity: child },
        "not 'not yours': there is nothing left to own"
    );
    assert_eq!(
        world
            .scope_mut(sibling)
            .unwrap()
            .link(child, sibling)
            .unwrap_err(),
        EntityAccessError::NotAlive { entity: child },
    );
    assert_eq!(
        world.scope_mut(sibling).unwrap().unlink(child).unwrap_err(),
        EntityAccessError::NotAlive { entity: child },
    );
    assert!(world.scope(scene).is_ok(), "the scene itself is untouched");
}

/// The shared subtree is readable from every scope but it is the root's to
/// rearrange, which is its own error rather than "not yours".
#[test]
fn a_shared_row_answers_that_only_the_root_may_move_it() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    let shared = world.shared();
    let resource = world.scope_mut(shared).unwrap().spawn(bundle![Marker(1)]);
    assert_eq!(
        world
            .scope_mut(scene)
            .unwrap()
            .despawn(resource)
            .unwrap_err(),
        EntityAccessError::NotMovable {
            entity: resource,
            scope: scene,
        },
    );
    assert_eq!(
        world
            .scope_mut(scene)
            .unwrap()
            .unlink(resource)
            .unwrap_err(),
        EntityAccessError::NotMovable {
            entity: resource,
            scope: scene,
        },
    );
    // The scene can still read it, which is the whole point of the subtree.
    assert!(world.scope(scene).unwrap().contains(resource));
    assert!(world.root_scope_mut().despawn(resource).is_ok());
}

/// The roots hold the tree up, so they are neither moved nor removed, and the
/// reason is their own rather than a missing owner.
#[test]
fn a_root_answers_that_it_is_scaffolding() {
    let mut world = World::new();
    let root = world.root();
    let shared = world.shared();
    assert_eq!(
        world.root_scope_mut().despawn(root).unwrap_err(),
        EntityAccessError::Scaffolding { entity: root },
    );
    assert_eq!(
        world.root_scope_mut().despawn(shared).unwrap_err(),
        EntityAccessError::Scaffolding { entity: shared },
    );
    assert_eq!(
        world.root_scope_mut().link(root, shared).unwrap_err(),
        EntityAccessError::Scaffolding { entity: root },
        "the first end that is scaffolding is the one named"
    );
    assert_eq!(
        world.root_scope_mut().unlink(root).unwrap_err(),
        EntityAccessError::Scaffolding { entity: root },
    );
}

/// Moving a scope under its own descendant would cut the tree in two, so it is
/// refused with the pair that would make the cycle.
#[test]
fn a_link_that_would_cycle_says_which_pair_it_was() {
    let (mut world, scene, child, _) = fixture();
    assert_eq!(
        world
            .scope_mut(scene)
            .unwrap()
            .link(scene, child)
            .unwrap_err(),
        EntityAccessError::WouldCycle {
            child: scene,
            parent: child,
        },
        "the child is below the scene"
    );
    assert_eq!(
        world
            .scope_mut(scene)
            .unwrap()
            .link(child, child)
            .unwrap_err(),
        EntityAccessError::WouldCycle {
            child,
            parent: child,
        },
        "so is a link onto itself"
    );
    assert_eq!(
        world.scope_mut(scene).unwrap().unlink(scene).unwrap_err(),
        EntityAccessError::WouldCycle {
            child: scene,
            parent: scene,
        },
        "being its own parent is how the tree says 'root'"
    );
    assert_eq!(world.parent_scope(child), Ok(scene), "nothing moved");
}

/// Unlinking what is already in the scope is the change the caller asked for,
/// so it answers with the move already made rather than a complaint.
#[test]
fn unlinking_what_is_already_there_is_the_thing_asked_for() {
    let (mut world, _scene, child, _) = fixture();
    let root = world.root();
    assert_eq!(world.root_scope_mut().unlink(child), Ok(()));
    assert_eq!(world.parent_scope(child), Ok(root));
    assert_eq!(
        world.root_scope_mut().unlink(child),
        Ok(()),
        "asking twice changes nothing and is not a complaint"
    );
    assert_eq!(world.parent_scope(child), Ok(root));
}

/// The reasons a structural change is turned down for are told apart by the
/// message as well as the variant: each one names the pair it is about.
#[test]
fn every_structural_message_names_its_reason() {
    let (mut world, scene, child, sibling) = fixture();
    let shared = world.shared();
    let resource = world.scope_mut(shared).unwrap().spawn(bundle![Marker(2)]);
    assert_eq!(
        world.scope_mut(sibling).unwrap().despawn(child),
        Err(EntityAccessError::NotOwned {
            entity: child,
            scope: sibling,
        }),
    );
    let mut scope = world.scope_mut(sibling).unwrap();
    let error = scope.despawn(child).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{child:?} is outside {sibling:?}, so it is not its to move or remove"),
    );
    let mut scope = world.scope_mut(scene).unwrap();
    let error = scope.despawn(resource).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{resource:?} is shared, so only the root may move or remove it"),
    );
    let mut scope = world.root_scope_mut();
    let root = scope.entity();
    let error = scope.despawn(root).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{root:?} is scaffolding: it neither moves nor goes"),
    );
    let mut scope = world.root_scope_mut();
    let error = scope.link(scene, child).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{scene:?} cannot be moved under {child:?}, which it holds"),
    );
}
