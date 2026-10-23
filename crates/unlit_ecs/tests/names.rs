//! Entity names: the label a handle carries for `Debug` and for MCP.

use unlit_ecs::{ArchetypeBuilder, World, bundle};

#[derive(Clone, Copy, Debug)]
struct Cube;

#[test]
fn a_bundle_names_the_entity_after_its_own_source_position() {
    let mut world = World::new();
    let cube = world.root_scope_mut().spawn(bundle![Cube]);
    let name = cube.name();
    assert!(
        name.contains("names.rs:"),
        "{name} names the file the bundle was written in"
    );
}

#[test]
fn spawn_named_and_the_builder_name_the_entity() {
    let mut world = World::new();
    let named = world
        .root_scope_mut()
        .spawn_named("a name of its own", bundle![Cube]);
    assert_eq!(named.name(), "a name of its own");

    let built = ArchetypeBuilder::new().named("assembled at runtime");
    let assembled = world.root_scope_mut().spawn(built);
    assert_eq!(assembled.name(), "assembled at runtime");
}

#[test]
fn a_reservation_names_the_handle_before_its_bundle_exists() {
    let mut world = World::new();
    let reserved = world.reserve_entity("reserved by hand");
    assert_eq!(reserved.name(), "reserved by hand");
    world.root_scope_mut().spawn_at(reserved, bundle![Cube]);
    assert_eq!(
        reserved.name(),
        "reserved by hand",
        "the bundle does not rename a reserved handle"
    );
    assert_eq!(
        world.root_scope().entity_info(reserved).unwrap().name,
        "reserved by hand",
        "the listing reports the same label"
    );
}

#[test]
fn the_name_is_a_label_and_not_part_of_the_identity() {
    let mut world = World::new();
    let cube = world.root_scope_mut().spawn(bundle![Cube]);
    let restored = unlit_ecs::Entity::from_bits(cube.to_bits());

    assert_eq!(restored, cube, "the bits are the identity");
    assert_eq!(restored.to_bits(), cube.to_bits());
    assert_eq!(restored.name(), "", "a restored handle has no label");
    assert!(!cube.name().is_empty());
}

#[test]
fn a_queued_spawn_keeps_the_name_its_bundle_carries() {
    let world = World::new();
    let queued = world.root_scope().queue().spawn(bundle![Cube]);
    assert!(queued.name().contains("names.rs:"), "{}", queued.name());
}
