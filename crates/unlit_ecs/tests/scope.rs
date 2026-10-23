//! Scopes: which entities a scope can see, and which it may change.

use unlit_ecs::bundle;
use unlit_ecs::{Entity, Scope, ScopeOf, World};

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
fn every_entity_carries_the_scope_it_lives_in() {
    let (world, scene, child, _) = fixture();
    let root = world.root();
    assert_eq!(
        world.parent_scope(root),
        Some(root),
        "the root is its own scope"
    );
    assert_eq!(world.parent_scope(world.shared()), Some(root));
    assert_eq!(world.parent_scope(scene), Some(root));
    assert_eq!(world.parent_scope(child), Some(scene));
    assert_eq!(
        world
            .root_scope()
            .query_one::<&ScopeOf>(child)
            .unwrap()
            .parent(),
        scene
    );
}

#[test]
fn the_root_scope_sees_everything() {
    let (world, scene, child, sibling) = fixture();
    let scope = world.root_scope();
    assert!(scope.contains(scene));
    assert!(scope.contains(child));
    assert!(scope.contains(sibling));
}

#[test]
fn a_scene_sees_its_subtree_and_nothing_beside_it() {
    let (world, scene, child, sibling) = fixture();
    let scope = world.scope(scene).unwrap();
    assert!(scope.contains(scene), "itself");
    assert!(scope.contains(child), "its descendant");
    assert!(!scope.contains(sibling), "not its sibling");
    assert!(!scope.contains(world.root()), "not its ancestor");
}

#[test]
fn the_shared_subtree_reaches_every_view() {
    let (mut world, scene, _, _) = fixture();
    let shared = world.shared();
    let global = world.scope_mut(shared).unwrap().spawn(bundle![Marker(9)]);
    assert!(world.root_scope().contains(global));
    assert!(
        world.scope(scene).unwrap().contains(global),
        "a private scene"
    );
}

#[test]
fn a_query_only_yields_what_the_view_can_see() {
    let (world, scene, child, _) = fixture();
    let all: Vec<Entity> = world
        .root_scope()
        .query::<(&Marker, Entity)>()
        .map(|(_marker, e)| e)
        .collect();
    assert_eq!(all, [child]);
    let mine: Vec<Entity> = world
        .scope(scene)
        .unwrap()
        .query::<(&Marker, Entity)>()
        .map(|(_marker, e)| e)
        .collect();
    assert_eq!(mine, [child]);
}

#[test]
fn a_sibling_scene_cannot_read_the_other_scenes_entities() {
    let (world, _, child, sibling) = fixture();
    let scope = world.scope(sibling).unwrap();
    assert!(!scope.contains(child));
    assert!(scope.query_one::<&Marker>(child).is_none());
    assert!(scope.query_one::<&mut Marker>(child).map(|_| ()).is_none());
    assert_eq!(scope.query::<&Marker>().count(), 0);
}

#[test]
fn two_scopes_share_an_archetype_and_still_do_not_mix() {
    let (mut world, scene, child, sibling) = fixture();
    let other = world.scope_mut(sibling).unwrap().spawn(bundle![Marker(2)]);
    // Both rows are `(Marker, ScopeOf)`, so they share one archetype.
    assert_eq!(
        world.root_scope().location(child).unwrap().archetype(),
        world.root_scope().location(other).unwrap().archetype(),
    );
    let in_scene: Vec<u32> = world
        .scope(scene)
        .unwrap()
        .query::<&Marker>()
        .map(|marker| marker.0)
        .collect();
    assert_eq!(in_scene, [1], "the sibling's row is filtered out");

    // The archetype is shared, so neither scope sees all of it and
    // `Scope::archetype` turns it down. Each row is still in an archetype, and
    // `Scope::archetype_of` answers for the row.
    let id = world.root_scope().location(child).unwrap().archetype();
    let scope = world.scope(scene).unwrap();
    assert!(scope.archetype(id).is_none(), "not every row is visible");
    let archetype = scope.archetype_of(child).expect("the row is visible");
    assert_eq!(
        archetype.types(),
        scope.archetype_of(other).unwrap().types()
    );
    assert!(
        archetype.entities().contains(&other),
        "the other scope's row"
    );
    assert!(scope.archetype_of(sibling).is_none(), "not in this scope");
}

#[test]
fn a_scene_can_be_spawned_before_its_content_and_used_as_a_scope() {
    let mut world = World::new();
    let scene = world.root_scope_mut().spawn(bundle![Name("late")]);
    let child = world.scope_mut(scene).unwrap().spawn(bundle![Marker(1)]);
    assert_eq!(world.parent_scope(child), Some(scene));
    assert_eq!(world.scope(scene).unwrap().query::<&Marker>().count(), 1);
}

#[test]
fn link_moves_an_entity_into_another_scope() {
    let (mut world, scene, child, sibling) = fixture();
    // The scene owns the child but not the sibling, so only the root may move
    // the child there.
    assert!(!world.scope_mut(scene).unwrap().link(child, sibling));
    assert!(world.root_scope_mut().link(child, sibling));
    assert_eq!(world.parent_scope(child), Some(sibling));
    assert_eq!(world.scope(scene).unwrap().query::<&Marker>().count(), 0);
    assert_eq!(world.scope(sibling).unwrap().query::<&Marker>().count(), 1);
}

#[test]
fn link_keeps_the_component_set_so_the_archetype_is_unchanged() {
    let (mut world, scene, child, sibling) = fixture();
    let before = world.root_scope().location(child).unwrap().archetype();
    world.scope_mut(scene).unwrap().link(child, sibling);
    assert_eq!(
        world.root_scope().location(child).unwrap().archetype(),
        before
    );
    assert_eq!(world.root_scope().query_one::<&Marker>(child).unwrap().0, 1);
}

#[test]
fn unlink_moves_an_entity_into_the_views_scope() {
    let (mut world, scene, child, _) = fixture();
    let root = world.root();
    assert!(
        !world.scope_mut(scene).unwrap().unlink(child),
        "it is already in this scope"
    );
    assert!(
        world.root_scope_mut().unlink(child),
        "move it up to the root"
    );
    assert_eq!(world.parent_scope(child), Some(root));
    assert_eq!(world.scope(scene).unwrap().query::<&Marker>().count(), 0);
    // Moving it back is the root's call now: the scene no longer owns it.
    assert!(!world.scope_mut(scene).unwrap().unlink(child));
    assert!(world.root_scope_mut().link(child, scene));
    assert_eq!(world.parent_scope(child), Some(scene));
}

#[test]
fn link_refuses_to_move_a_scope_into_its_own_descendant() {
    let (mut world, scene, child, _) = fixture();
    assert!(!world.scope_mut(scene).unwrap().link(scene, child));
    assert_eq!(world.parent_scope(scene), Some(world.root()));
}

#[test]
fn link_refuses_an_entity_the_view_does_not_own() {
    let (mut world, _, _, sibling) = fixture();
    let other = world.root_scope_mut().spawn(bundle![Name("third")]);
    // `sibling` does not own `other`, so it cannot pull it in.
    assert!(!world.scope_mut(sibling).unwrap().link(other, sibling));
    assert_eq!(world.parent_scope(other), Some(world.root()));
}

#[test]
fn the_roots_cannot_be_despawned_or_relinked() {
    let mut world = World::new();
    let root = world.root();
    let shared = world.shared();
    assert!(!world.root_scope_mut().despawn(root));
    assert!(!world.root_scope_mut().despawn(shared));
    assert!(!world.root_scope_mut().link(root, shared));
    assert!(world.root_scope().contains(root));
}

#[test]
fn despawning_a_scene_takes_its_whole_subtree() {
    let (mut world, scene, child, sibling) = fixture();
    let grandchild = world.scope_mut(child).unwrap().spawn(bundle![Marker(2)]);
    assert!(world.root_scope_mut().despawn(scene));
    assert!(!world.root_scope().contains(scene));
    assert!(!world.root_scope().contains(child));
    assert!(!world.root_scope().contains(grandchild));
    assert!(world.root_scope().contains(sibling), "the sibling survives");
}

#[test]
fn a_scene_cannot_despawn_its_ancestor() {
    let (mut world, scene, _, _) = fixture();
    let root = world.root();
    let sibling = world.root_scope_mut().spawn(bundle![Name("sib")]);
    assert!(!world.scope_mut(scene).unwrap().despawn(root));
    assert!(!world.scope_mut(scene).unwrap().despawn(sibling));
}

#[test]
fn a_view_does_not_see_a_dead_handle() {
    let (mut world, scene, child, _) = fixture();
    world.root_scope_mut().despawn(child);
    assert!(!world.scope(scene).unwrap().contains(child));
    assert!(
        world
            .scope(scene)
            .unwrap()
            .query_one::<&Marker>(child)
            .is_none()
    );
}

#[test]
fn counting_is_per_view_and_excludes_the_scaffolding() {
    let (world, scene, _, sibling) = fixture();
    assert_eq!(world.root_scope().len(), 3, "two scenes and one child");
    assert_eq!(
        world.scope(scene).unwrap().len(),
        2,
        "the scene and its child"
    );
    assert_eq!(world.scope(sibling).unwrap().len(), 1);
}

#[test]
fn the_info_structs_report_the_view() {
    let (world, scene, _, _) = fixture();
    let info = world.scope(scene).unwrap().info();
    assert_eq!(info.entities, 2);
    assert_eq!(info.root, world.root());
    assert_eq!(info.shared, world.shared());
    let archetypes = world.scope(scene).unwrap().archetype_infos();
    let sum: usize = archetypes.iter().map(|info| info.len).sum();
    assert_eq!(sum, info.entities, "the lengths add up to the count");
}

#[test]
fn entity_infos_only_lists_what_the_view_sees() {
    let (world, scene, child, _) = fixture();
    let infos = world.scope(scene).unwrap().entity_infos(usize::MAX);
    assert_eq!(infos.len(), 2);
    assert!(infos.iter().any(|info| info.entity == child));
}

#[test]
fn the_tree_agrees_with_the_scope_components() {
    let (mut world, scene, child, sibling) = fixture();
    let extra = world.scope_mut(scene).unwrap().spawn(bundle![Marker(2)]);
    world.scope_mut(sibling).unwrap().link(child, extra);
    assert_invariants(&world);

    world.root_scope_mut().despawn(extra);
    assert_invariants(&world);
}

#[test]
fn a_queued_spawn_lands_in_the_queue_s_scope() {
    let (mut world, scene, _, _) = fixture();
    let queued = world
        .scope(scene)
        .unwrap()
        .queue()
        .spawn(bundle![Marker(3)]);
    world.apply();
    assert_eq!(world.parent_scope(queued), Some(scene));
}

#[test]
fn a_queued_despawn_removes_the_subtree() {
    let (mut world, scene, child, _) = fixture();
    world.scope(scene).unwrap().queue().despawn(scene);
    world.apply();
    assert!(!world.root_scope().contains(child));
}

#[test]
fn a_view_of_an_unknown_scope_is_none() {
    let world = World::new();
    assert!(world.scope(Entity::from_raw(999, 0)).is_none());
}

#[test]
fn a_view_cannot_descend_into_a_sibling() {
    let (world, _, _, sibling) = fixture();
    let scene = world.root_scope().children(world.root())[0];
    assert!(world.scope(scene).unwrap().scope(sibling).is_none());
    assert!(world.root_scope().scope(sibling).is_some());
}

#[test]
fn a_view_is_copy_and_reports_its_scope() {
    let (world, scene, _, _) = fixture();
    let scope: Scope<'_> = world.scope(scene).unwrap();
    let copy = scope;
    assert_eq!(copy.entity(), scene);
    assert_eq!(scope.entity(), scene);
}

#[test]
fn the_shared_subtree_is_content_that_every_view_sees() {
    let mut world = World::new();
    let shared = world.shared();
    // A resource spawned in the shared subtree reaches every scope, and a
    // scope counts it: only the two scaffolding entities themselves are
    // excluded, not what a caller puts under them.
    let resource = world.scope_mut(shared).unwrap().spawn(bundle![Marker(1)]);
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    world.scope_mut(scene).unwrap().spawn(bundle![Marker(2)]);

    let scope = world.scope(scene).unwrap();
    assert!(
        scope.contains(resource),
        "visible through the shared subtree"
    );
    assert_eq!(
        scope.query_one::<&Marker>(resource).map(|marker| marker.0),
        Some(1)
    );
    let mut found: Vec<u32> = scope.query::<&Marker>().map(|marker| marker.0).collect();
    found.sort_unstable();
    assert_eq!(
        found,
        [1, 2],
        "the resource and the child, and nothing else"
    );
}

/// The invariants the tree and the components have to agree on.
fn assert_invariants(world: &World) {
    let root = world.root();
    let scope = world.root_scope();
    // Every row's `ScopeOf` is the parent the tree records.
    for archetype in scope.archetypes() {
        for &entity in archetype.entities() {
            let recorded = scope
                .query_one::<&ScopeOf>(entity)
                .unwrap_or_else(|| panic!("{entity:?} has no scope"))
                .parent();
            assert_eq!(
                world.parent_scope(entity),
                Some(recorded),
                "{entity:?} disagrees with its ScopeOf"
            );
        }
    }
    // The child index is the parent index read the other way.
    for archetype in scope.archetypes() {
        for &entity in archetype.entities() {
            for &child in &world.children(entity) {
                assert_eq!(
                    world.parent_scope(child),
                    Some(entity),
                    "{child:?} is listed under {entity:?}"
                );
            }
        }
    }
    assert_eq!(world.parent_scope(root), Some(root));
    // Counting, querying and the per-archetype lengths are one number.
    for scope in [
        scope,
        world.root_scope(),
        world.scope(world.shared()).unwrap(),
    ] {
        let counted = scope.len();
        assert_eq!(counted, scope.query::<Entity>().count(), "{scope:?}");
        let summed: usize = scope.archetype_infos().iter().map(|info| info.len).sum();
        assert_eq!(summed, counted, "{scope:?}");
        assert_eq!(scope.info().entities, counted, "{scope:?}");
    }
}

#[test]
fn a_queued_despawn_of_something_the_scope_does_not_own_is_dropped() {
    let (mut world, scene, child, sibling) = fixture();
    // `scene` neither owns nor even sees its sibling.
    world.scope(scene).unwrap().queue().despawn(sibling);
    world.apply();
    assert!(
        world.root_scope().contains(sibling),
        "a sibling survives a queued despawn from another scene"
    );
    assert!(world.scope(scene).unwrap().contains(child));
}

#[test]
fn a_queued_despawn_cannot_remove_the_scaffolding() {
    let (mut world, _, _, _) = fixture();
    let root = world.root();
    let shared = world.shared();
    let queue = world.root_scope().queue();
    queue.despawn(root);
    queue.despawn(shared);
    world.apply();
    assert_eq!(
        world.root_scope().entity(),
        root,
        "the root is still a scope"
    );
    assert_eq!(world.parent_scope(shared), Some(root));
    assert!(world.root_scope().contains(root));
    assert_invariants(&world);
}

#[test]
fn unlinking_a_scope_into_itself_is_refused() {
    let (mut world, scene, _, _) = fixture();
    assert!(
        !world.scope_mut(scene).unwrap().unlink(scene),
        "a scope cannot become its own parent"
    );
    assert_eq!(world.parent_scope(scene), Some(world.root()));
    assert_invariants(&world);
}

#[test]
fn a_shared_resource_is_the_root_s_to_despawn() {
    let mut world = World::new();
    let shared = world.shared();
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    let resource = world.scope_mut(shared).unwrap().spawn(bundle![Marker(1)]);
    assert!(
        !world.scope_mut(scene).unwrap().despawn(resource),
        "a scene cannot remove what the shared subtree lends it"
    );
    world.root_scope().queue().despawn(resource);
    world.apply();
    assert!(
        !world.root_scope().contains(resource),
        "the root removes it"
    );
    assert_eq!(world.parent_scope(shared), Some(world.root()));
    assert_invariants(&world);
}

#[test]
fn a_shared_resource_is_the_root_s_to_move() {
    let mut world = World::new();
    let root = world.root();
    let shared = world.shared();
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    let resource = world.scope_mut(shared).unwrap().spawn(bundle![Marker(1)]);
    assert!(
        !world.scope_mut(scene).unwrap().link(resource, scene),
        "a scene cannot move what the shared subtree lends it"
    );
    assert!(!world.scope_mut(scene).unwrap().unlink(resource));
    // A queue of another scope drops the relink when it is applied.
    world.scope(scene).unwrap().queue().link(resource, scene);
    world.apply();
    assert_eq!(
        world.parent_scope(resource),
        Some(shared),
        "a scene's queue cannot move it either"
    );

    assert!(world.root_scope_mut().unlink(resource));
    assert_eq!(world.parent_scope(resource), Some(root));
    world.root_scope().queue().link(resource, scene);
    world.apply();
    assert_eq!(world.parent_scope(resource), Some(scene));
    world.root_scope().queue().unlink(resource);
    world.apply();
    assert_eq!(world.parent_scope(resource), Some(root));

    // The shared scope itself is scaffolding: it neither moves nor goes.
    assert!(!world.root_scope_mut().link(shared, scene));
    assert!(!world.root_scope_mut().unlink(shared));
    assert!(!world.root_scope_mut().despawn(shared));
    assert_eq!(world.parent_scope(shared), Some(root));
    assert_invariants(&world);
}

#[test]
#[should_panic(expected = "is not a reserved handle")]
fn spawning_at_a_dead_handle_panics() {
    let (mut world, scene, child, _) = fixture();
    world.root_scope_mut().despawn(child);
    // A stale handle has no reservation left, so the spawn is refused rather
    // than writing a second row for the same index.
    world
        .scope_mut(scene)
        .unwrap()
        .spawn_at(child, bundle![Marker(9)]);
}

#[test]
#[should_panic(expected = "already borrowed")]
fn an_exclusive_scope_borrow_during_a_query_panics() {
    let (mut world, _, child, sibling) = fixture();
    // Two scopes in one archetype: the query has to read each row's scope.
    let other = world.scope_mut(sibling).unwrap().spawn(bundle![Marker(2)]);
    let held = world.root_scope().query_one::<&mut ScopeOf>(other).unwrap();
    let _: Vec<_> = world.root_scope().query::<&Marker>().collect();
    drop(held);
    assert!(world.root_scope().contains(child));
}

#[test]
fn a_shared_resource_is_counted_but_the_scaffolding_is_not() {
    let mut world = World::new();
    let shared = world.shared();
    world.scope_mut(shared).unwrap().spawn(bundle![Marker(1)]);
    let scene = world.root_scope_mut().spawn(bundle![Name("scene")]);
    world.scope_mut(scene).unwrap().spawn(bundle![Marker(2)]);

    assert_eq!(
        world.root_scope().len(),
        3,
        "the scene, its child, the resource"
    );
    let scope = world.scope(scene).unwrap();
    // The resource reaches the scope through the shared subtree, so it is
    // counted here too: what the scaffolding excludes is only the two root
    // entities themselves.
    assert_eq!(scope.len(), 3, "the scene, its child, the shared resource");
    // The archetype lengths are what the count is summed from, so they have to
    // agree with it.
    let sum: usize = scope.archetype_infos().iter().map(|info| info.len).sum();
    assert_eq!(sum, scope.len());
}
