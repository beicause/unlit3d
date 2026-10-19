//! Deferred structural changes, from a callback that only has `&World`.

use unlit_ecs::{Commands, Entity, World};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Spawned;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Marker(u32);

/// The shape a driver uses: a behaviour runs with a shared world, reads what
/// it needs from it, queues what it needs through `Commands`, and the driver
/// applies the queue afterwards.
fn run_behaviour(world: &World, commands: Commands<'_>) -> Entity {
    let template = world.root_view().query::<&Spawned>().count();
    commands.spawn((Marker(template as u32), Spawned))
}

#[test]
fn a_queued_spawn_does_nothing_until_apply() {
    let mut world = World::new();
    let behavior_ran = std::cell::Cell::new(false);

    let child = {
        let commands = world.root_view().queue();
        behavior_ran.set(true);
        run_behaviour(&world, commands)
    };

    assert!(behavior_ran.get());
    assert!(!world.root_view().contains(child), "queued, not spawned");
    assert_eq!(world.root_view().len(), 0);

    world.apply();
    assert!(world.root_view().contains(child));
    assert_eq!(
        world.root_view().get::<Marker>(child).unwrap().0,
        0,
        "the behaviour read the world before the spawn existed"
    );
    assert!(world.root_view().has::<Spawned>(child));
}

#[test]
fn a_queued_handle_is_usable_before_apply() {
    let mut world = World::new();

    // The handle comes back immediately, so it can be stored in a component
    // on another entity before the queue is applied.
    let child = world.root_view().queue().spawn((Marker(7),));
    let owner = world.root_view_mut().spawn((Marker(0), child));

    world.apply();

    assert!(world.root_view().contains(child));
    assert_eq!(*world.root_view().get::<Entity>(owner).unwrap(), child);
}

#[test]
fn a_queued_despawn_only_takes_effect_on_apply() {
    let mut world = World::new();
    let entity = world.root_view_mut().spawn((Marker(1),));

    world.root_view().queue().despawn(entity);
    assert!(
        world.root_view().contains(entity),
        "still alive until apply"
    );

    world.apply();
    assert!(!world.root_view().contains(entity));
}

#[test]
fn commands_apply_in_the_order_they_were_queued() {
    let mut world = World::new();
    let first = world.root_view().queue().spawn((Marker(1),));
    let second = world.root_view().queue().spawn((Marker(2),));

    world.apply();

    assert!(world.root_view().contains(first));
    assert!(world.root_view().contains(second));
    assert_eq!(world.root_view().get::<Marker>(first).unwrap().0, 1);
    assert_eq!(world.root_view().get::<Marker>(second).unwrap().0, 2);
}

#[test]
fn commands_queued_while_applying_are_applied_too() {
    let mut world = World::new();

    let seed = {
        let commands = world.root_view().queue();
        let seed = commands.spawn((Marker(1),));
        commands.spawn((Marker(2),));
        seed
    };
    world.apply();
    assert!(world.root_view().contains(seed));
    assert_eq!(world.root_view().len(), 2);

    // A later batch is a fresh queue and applies on its own `apply`.
    let third = world.root_view().queue().spawn((Marker(3),));
    world.apply();
    assert!(world.root_view().contains(third));
    assert_eq!(world.root_view().len(), 3);
}

#[test]
fn applying_an_empty_queue_changes_nothing() {
    let mut world = World::new();
    world.apply();
    assert!(world.root_view().is_empty());

    world.root_view_mut().spawn((Marker(1),));
    let before = world.root_view().len();
    world.apply();
    assert_eq!(world.root_view().len(), before);
}

#[test]
fn a_custom_command_can_be_queued() {
    use unlit_ecs::Command;

    /// Despawns every entity that has a `Marker` below a threshold.
    struct DespawnBelow(u32);

    impl Command for DespawnBelow {
        fn apply(self: Box<Self>, world: &mut World) {
            let doomed: Vec<Entity> = world
                .root_view()
                .query::<&Marker>()
                .filter(|(_, marker)| marker.0 < self.0)
                .map(|(entity, _)| entity)
                .collect();
            for entity in doomed {
                world.root_view_mut().despawn(entity);
            }
        }
    }

    let mut world = World::new();
    let low = world.root_view_mut().spawn((Marker(1),));
    let high = world.root_view_mut().spawn((Marker(9),));

    world.root_view().queue().push(DespawnBelow(5));
    assert!(world.root_view().contains(low), "not applied yet");

    world.apply();
    assert!(!world.root_view().contains(low));
    assert!(world.root_view().contains(high));
}

#[test]
fn many_commands_apply_without_losing_any() {
    let mut world = World::new();
    let handles: Vec<Entity> = {
        let commands = world.root_view().queue();
        (0..256u32)
            .map(|value| commands.spawn((Marker(value),)))
            .collect()
    };

    world.apply();

    assert_eq!(world.root_view().len(), 256);
    for (expected, entity) in (0..256u32).zip(&handles) {
        assert_eq!(
            world.root_view().get::<Marker>(*entity).unwrap().0,
            expected
        );
    }
}
