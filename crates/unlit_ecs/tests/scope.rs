//! Scopes: which entities a view can see, and which it may change.

use unlit_ecs::{Entity, ScopeOf, ScopedWorld, World};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marker(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Name(&'static str);

/// A world with a scene, a child of that scene, and a sibling scene.
fn fixture() -> (World, Entity, Entity, Entity) {
    let mut world = World::new();
    let scene = world.root_view_mut().spawn((Name("scene"),));
    let child = world.view_mut(scene).unwrap().spawn((Marker(1),));
    let sibling = world.root_view_mut().spawn((Name("other"),));
    (world, scene, child, sibling)
}

#[test]
fn every_entity_carries_the_scope_it_lives_in() {
    let (world, scene, child, _) = fixture();
    let root = world.root();
    assert_eq!(
        world.scope_of(root),
        Some(root),
        "the root is its own scope"
    );
    assert_eq!(world.scope_of(world.shared()), Some(root));
    assert_eq!(world.scope_of(scene), Some(root));
    assert_eq!(world.scope_of(child), Some(scene));
    assert_eq!(world.root_view().get::<ScopeOf>(child).unwrap().0, scene);
}

#[test]
fn the_root_view_sees_everything() {
    let (world, scene, child, sibling) = fixture();
    let view = world.root_view();
    assert!(view.contains(scene));
    assert!(view.contains(child));
    assert!(view.contains(sibling));
}

#[test]
fn a_scene_sees_its_subtree_and_nothing_beside_it() {
    let (world, scene, child, sibling) = fixture();
    let view = world.view(scene).unwrap();
    assert!(view.contains(scene), "itself");
    assert!(view.contains(child), "its descendant");
    assert!(!view.contains(sibling), "not its sibling");
    assert!(!view.contains(world.root()), "not its ancestor");
}

#[test]
fn the_shared_subtree_reaches_every_view() {
    let (mut world, scene, _, _) = fixture();
    let shared = world.shared();
    let global = world.view_mut(shared).unwrap().spawn((Marker(9),));
    assert!(world.root_view().contains(global));
    assert!(
        world.view(scene).unwrap().contains(global),
        "a private scene"
    );
}

#[test]
fn a_query_only_yields_what_the_view_can_see() {
    let (world, scene, child, _) = fixture();
    let all: Vec<Entity> = world
        .root_view()
        .query::<&Marker>()
        .map(|(e, _)| e)
        .collect();
    assert_eq!(all, [child]);
    let mine: Vec<Entity> = world
        .view(scene)
        .unwrap()
        .query::<&Marker>()
        .map(|(e, _)| e)
        .collect();
    assert_eq!(mine, [child]);
}

#[test]
fn a_sibling_scene_cannot_read_the_other_scenes_entities() {
    let (world, _, child, sibling) = fixture();
    let view = world.view(sibling).unwrap();
    assert!(!view.contains(child));
    assert!(view.get::<Marker>(child).is_none());
    assert!(view.with_mut::<Marker, _>(child, |_| ()).is_none());
    assert_eq!(view.query::<&Marker>().count(), 0);
}

#[test]
fn two_scopes_share_an_archetype_and_still_do_not_mix() {
    let (mut world, scene, child, sibling) = fixture();
    let other = world.view_mut(sibling).unwrap().spawn((Marker(2),));
    // Both rows are `(Marker, ScopeOf)`, so they share one archetype.
    assert_eq!(
        world.root_view().location(child).unwrap().archetype(),
        world.root_view().location(other).unwrap().archetype(),
    );
    let in_scene: Vec<u32> = world
        .view(scene)
        .unwrap()
        .query::<&Marker>()
        .map(|(_, m)| m.0)
        .collect();
    assert_eq!(in_scene, [1], "the sibling's row is filtered out");
}

#[test]
fn a_scene_can_be_spawned_before_its_content_and_used_as_a_scope() {
    let mut world = World::new();
    let scene = world.root_view_mut().spawn((Name("late"),));
    let child = world.view_mut(scene).unwrap().spawn((Marker(1),));
    assert_eq!(world.scope_of(child), Some(scene));
    assert_eq!(world.view(scene).unwrap().query::<&Marker>().count(), 1);
}

#[test]
fn link_moves_an_entity_into_another_scope() {
    let (mut world, scene, child, sibling) = fixture();
    // The scene owns the child but not the sibling, so only the root may move
    // the child there.
    assert!(!world.view_mut(scene).unwrap().link(child, sibling));
    assert!(world.root_view_mut().link(child, sibling));
    assert_eq!(world.scope_of(child), Some(sibling));
    assert_eq!(world.view(scene).unwrap().query::<&Marker>().count(), 0);
    assert_eq!(world.view(sibling).unwrap().query::<&Marker>().count(), 1);
}

#[test]
fn link_keeps_the_component_set_so_the_archetype_is_unchanged() {
    let (mut world, scene, child, sibling) = fixture();
    let before = world.root_view().location(child).unwrap().archetype();
    world.view_mut(scene).unwrap().link(child, sibling);
    assert_eq!(
        world.root_view().location(child).unwrap().archetype(),
        before
    );
    assert_eq!(world.root_view().get::<Marker>(child).unwrap().0, 1);
}

#[test]
fn unlink_moves_an_entity_into_the_views_scope() {
    let (mut world, scene, child, _) = fixture();
    let root = world.root();
    assert!(
        !world.view_mut(scene).unwrap().unlink(child),
        "it is already in this scope"
    );
    assert!(
        world.root_view_mut().unlink(child),
        "move it up to the root"
    );
    assert_eq!(world.scope_of(child), Some(root));
    assert_eq!(world.view(scene).unwrap().query::<&Marker>().count(), 0);
    // Moving it back is the root's call now: the scene no longer owns it.
    assert!(!world.view_mut(scene).unwrap().unlink(child));
    assert!(world.root_view_mut().link(child, scene));
    assert_eq!(world.scope_of(child), Some(scene));
}

#[test]
fn link_refuses_to_move_a_scope_into_its_own_descendant() {
    let (mut world, scene, child, _) = fixture();
    assert!(!world.view_mut(scene).unwrap().link(scene, child));
    assert_eq!(world.scope_of(scene), Some(world.root()));
}

#[test]
fn link_refuses_an_entity_the_view_does_not_own() {
    let (mut world, _, _, sibling) = fixture();
    let other = world.root_view_mut().spawn((Name("third"),));
    // `sibling` does not own `other`, so it cannot pull it in.
    assert!(!world.view_mut(sibling).unwrap().link(other, sibling));
    assert_eq!(world.scope_of(other), Some(world.root()));
}

#[test]
fn the_roots_cannot_be_despawned_or_relinked() {
    let mut world = World::new();
    let root = world.root();
    let shared = world.shared();
    assert!(!world.root_view_mut().despawn(root));
    assert!(!world.root_view_mut().despawn(shared));
    assert!(!world.root_view_mut().link(root, shared));
    assert!(world.root_view().contains(root));
}

#[test]
fn despawning_a_scene_takes_its_whole_subtree() {
    let (mut world, scene, child, sibling) = fixture();
    let grandchild = world.view_mut(child).unwrap().spawn((Marker(2),));
    assert!(world.root_view_mut().despawn(scene));
    assert!(!world.root_view().contains(scene));
    assert!(!world.root_view().contains(child));
    assert!(!world.root_view().contains(grandchild));
    assert!(world.root_view().contains(sibling), "the sibling survives");
}

#[test]
fn a_scene_cannot_despawn_its_ancestor() {
    let (mut world, scene, _, _) = fixture();
    let root = world.root();
    let sibling = world.root_view_mut().spawn((Name("sib"),));
    assert!(!world.view_mut(scene).unwrap().despawn(root));
    assert!(!world.view_mut(scene).unwrap().despawn(sibling));
}

#[test]
fn a_view_does_not_see_a_dead_handle() {
    let (mut world, scene, child, _) = fixture();
    world.root_view_mut().despawn(child);
    assert!(!world.view(scene).unwrap().contains(child));
    assert!(world.view(scene).unwrap().get::<Marker>(child).is_none());
}

#[test]
fn counting_is_per_view_and_excludes_the_scaffolding() {
    let (world, scene, _, sibling) = fixture();
    assert_eq!(world.root_view().len(), 3, "two scenes and one child");
    assert_eq!(
        world.view(scene).unwrap().len(),
        2,
        "the scene and its child"
    );
    assert_eq!(world.view(sibling).unwrap().len(), 1);
}

#[test]
fn the_info_structs_report_the_view() {
    let (world, scene, _, _) = fixture();
    let info = world.view(scene).unwrap().info();
    assert_eq!(info.entities, 2);
    assert_eq!(info.root, world.root());
    assert_eq!(info.shared, world.shared());
    let archetypes = world.view(scene).unwrap().archetype_infos();
    let sum: usize = archetypes.iter().map(|info| info.len).sum();
    assert_eq!(sum, info.entities, "the lengths add up to the count");
}

#[test]
fn entity_infos_only_lists_what_the_view_sees() {
    let (world, scene, child, _) = fixture();
    let infos = world.view(scene).unwrap().entity_infos(usize::MAX);
    assert_eq!(infos.len(), 2);
    assert!(infos.iter().any(|info| info.entity == child));
}

#[test]
fn rebuilding_the_children_index_agrees_with_the_components() {
    let (mut world, scene, child, _) = fixture();
    assert_eq!(world.children(scene), [child]);
    world.rebuild_children();
    assert_eq!(world.children(scene), [child]);
    assert_eq!(world.children(world.root()).len(), 2, "two scenes");
}

#[test]
fn a_queued_spawn_lands_in_the_queue_s_scope() {
    let (mut world, scene, _, _) = fixture();
    let queued = world.view(scene).unwrap().queue().spawn((Marker(3),));
    world.apply();
    assert_eq!(world.scope_of(queued), Some(scene));
}

#[test]
fn a_queued_despawn_removes_the_subtree() {
    let (mut world, scene, child, _) = fixture();
    world.view(scene).unwrap().queue().despawn(scene);
    world.apply();
    assert!(!world.root_view().contains(child));
}

#[test]
fn a_bundle_carrying_a_scope_is_refused() {
    let mut world = World::new();
    let root = world.root();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.root_view_mut().spawn((Marker(1), ScopeOf(root)));
    }));
    assert!(
        result.is_err(),
        "the world writes the scope, not the bundle"
    );
}

#[test]
fn a_view_of_an_unknown_scope_is_none() {
    let world = World::new();
    assert!(world.view(Entity::from_raw(999, 0)).is_none());
}

#[test]
fn a_view_cannot_descend_into_a_sibling() {
    let (world, _, _, sibling) = fixture();
    let scene = world.root_view().children(world.root())[0];
    assert!(world.view(scene).unwrap().view(sibling).is_none());
    assert!(world.root_view().view(sibling).is_some());
}

#[test]
fn a_view_is_copy_and_reports_its_scope() {
    let (world, scene, _, _) = fixture();
    let view: ScopedWorld<'_> = world.view(scene).unwrap();
    let copy = view;
    assert_eq!(copy.scope(), scene);
    assert_eq!(view.scope(), scene);
}

#[test]
fn the_shared_subtree_is_content_that_every_view_sees() {
    let mut world = World::new();
    let shared = world.shared();
    // A resource spawned in the shared subtree reaches every view, and a
    // view counts it: only the two scaffolding entities themselves are
    // excluded, not what a caller puts under them.
    let resource = world.view_mut(shared).unwrap().spawn((Marker(1),));
    let scene = world.root_view_mut().spawn((Name("scene"),));
    world.view_mut(scene).unwrap().spawn((Marker(2),));

    let view = world.view(scene).unwrap();
    assert!(
        view.contains(resource),
        "visible through the shared subtree"
    );
    assert_eq!(view.get::<Marker>(resource).map(|marker| marker.0), Some(1));
    let mut found: Vec<u32> = view
        .query::<&Marker>()
        .map(|(_, marker)| marker.0)
        .collect();
    found.sort_unstable();
    assert_eq!(
        found,
        [1, 2],
        "the resource and the child, and nothing else"
    );
}

#[test]
fn a_shared_resource_is_counted_but_the_scaffolding_is_not() {
    let mut world = World::new();
    let shared = world.shared();
    world.view_mut(shared).unwrap().spawn((Marker(1),));
    let scene = world.root_view_mut().spawn((Name("scene"),));
    world.view_mut(scene).unwrap().spawn((Marker(2),));

    assert_eq!(
        world.root_view().len(),
        3,
        "the scene, its child, the resource"
    );
    let view = world.view(scene).unwrap();
    // The resource reaches the view through the shared subtree, so it is
    // counted here too: what the scaffolding excludes is only the two root
    // entities themselves.
    assert_eq!(view.len(), 3, "the scene, its child, the shared resource");
    // The archetype lengths are what the count is summed from, so they have to
    // agree with it.
    let sum: usize = view.archetype_infos().iter().map(|info| info.len).sum();
    assert_eq!(sum, view.len());
}
