//! The world's unit tests.

use super::World;

#[derive(Clone, Copy, Debug)]
struct Marker;

/// Two worlds never share an id, and a handle made in one is turned down by
/// the other rather than resolving to whatever entity shares its bits.
#[test]
fn a_handle_from_another_world_is_turned_down() {
    let first = World::new();
    let second = World::new();
    assert_ne!(first.id(), second.id());

    let entity = first.root();
    let remote = first.remote(entity);
    assert_eq!(first.resolve(remote), Ok(entity), "the maker resolves it");
    assert_eq!(
        second.resolve(remote),
        Err(crate::RemoteEntityError::ForeignWorld {
            entity,
            expected: second.id(),
            found: first.id(),
        }),
    );
    assert_eq!(
        second.resolve(second.remote(entity)),
        Ok(entity),
        "the same entity number is this world's own",
    );
}

/// A world dropped and made again does not hand its id to the new one, so a
/// stale handle is refused rather than resolving into its replacement.
#[test]
fn a_replacement_world_carries_a_fresh_id() {
    let stale = World::new();
    let id = stale.id();
    let remote = stale.remote(stale.root());
    drop(stale);
    let fresh = World::new();
    assert_ne!(fresh.id(), id);
    assert!(fresh.resolve(remote).is_err());
}

/// The lookup behind the lifecycle hooks, which is its only caller: an
/// archetype that does not carry the component simply has no hook to run.
#[test]
fn a_component_the_archetype_does_not_carry_runs_no_hook() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(crate::bundle![Marker]);
    assert!(
        world
            .take_lifecycle::<crate::lifecycle::OnSpawn>(entity)
            .is_none(),
        "the archetype has no OnSpawn column",
    );
    let dead = world.root_scope_mut().spawn(crate::bundle![Marker]);
    world.root_scope_mut().despawn(dead).unwrap();
    assert!(
        world
            .take_lifecycle::<crate::lifecycle::OnSpawn>(dead)
            .is_none(),
        "the handle is dead",
    );
}
