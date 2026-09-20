//! Deferred structural changes, from a callback that only has `&World`.

use unlit_ecs::{Commands, Entity, LocalWorld, World};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Spawned;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Marker(u32);

/// The shape a driver uses: a behaviour runs with a shared world, reads what
/// it needs from it, queues what it needs through `Commands`, and the driver
/// applies the queue afterwards.
fn run_behaviour(
    world: &World<unlit_ecs::LocalMode>,
    commands: Commands<'_, unlit_ecs::LocalMode>,
) -> Entity {
    let template = world.query::<&Spawned>().count();
    commands.spawn((Marker(template as u32), Spawned))
}

#[test]
fn a_queued_spawn_does_nothing_until_apply() {
    let mut world = LocalWorld::new();
    let behavior_ran = std::cell::Cell::new(false);

    let child = {
        let commands = world.queue();
        behavior_ran.set(true);
        run_behaviour(&world, commands)
    };

    assert!(behavior_ran.get());
    assert!(!world.contains(child), "queued, not spawned");
    assert_eq!(world.len(), 0);

    world.apply();
    assert!(world.contains(child));
    assert_eq!(
        world.get::<Marker>(child).unwrap().0,
        0,
        "the behaviour read the world before the spawn existed"
    );
    assert!(world.has::<Spawned>(child));
}

#[test]
fn a_queued_handle_is_usable_before_apply() {
    let mut world = LocalWorld::new();

    // The handle comes back immediately, so it can be stored in a component
    // on another entity before the queue is applied.
    let child = world.queue().spawn((Marker(7),));
    let owner = world.spawn((Marker(0), child));

    world.apply();

    assert!(world.contains(child));
    assert_eq!(*world.get::<Entity>(owner).unwrap(), child);
}

#[test]
fn a_queued_despawn_only_takes_effect_on_apply() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((Marker(1),));

    world.queue().despawn(entity);
    assert!(world.contains(entity), "still alive until apply");

    world.apply();
    assert!(!world.contains(entity));
}

#[test]
fn commands_apply_in_the_order_they_were_queued() {
    let mut world = LocalWorld::new();
    let first = world.queue().spawn((Marker(1),));
    let second = world.queue().spawn((Marker(2),));

    world.apply();

    assert!(world.contains(first));
    assert!(world.contains(second));
    assert_eq!(world.get::<Marker>(first).unwrap().0, 1);
    assert_eq!(world.get::<Marker>(second).unwrap().0, 2);
}

#[test]
fn commands_queued_while_applying_are_applied_too() {
    let mut world = LocalWorld::new();

    let seed = {
        let commands = world.queue();
        let seed = commands.spawn((Marker(1),));
        commands.spawn((Marker(2),));
        seed
    };
    world.apply();
    assert!(world.contains(seed));
    assert_eq!(world.len(), 2);

    // A later batch is a fresh queue and applies on its own `apply`.
    let third = world.queue().spawn((Marker(3),));
    world.apply();
    assert!(world.contains(third));
    assert_eq!(world.len(), 3);
}

#[test]
fn applying_an_empty_queue_changes_nothing() {
    let mut world = LocalWorld::new();
    world.apply();
    assert!(world.is_empty());

    world.spawn((Marker(1),));
    let before = world.len();
    world.apply();
    assert_eq!(world.len(), before);
}

#[test]
fn a_custom_command_can_be_queued() {
    use unlit_ecs::{Command, Mode, World as WorldType};

    /// Despawns every entity that has a `Marker` below a threshold.
    struct DespawnBelow(u32);

    impl<M: Mode> Command<M> for DespawnBelow {
        fn apply(self: Box<Self>, world: &mut WorldType<M>) {
            let doomed: Vec<Entity> = world
                .query::<&Marker>()
                .filter(|(_, marker)| marker.0 < self.0)
                .map(|(entity, _)| entity)
                .collect();
            for entity in doomed {
                world.despawn(entity);
            }
        }
    }

    let mut world = LocalWorld::new();
    let low = world.spawn((Marker(1),));
    let high = world.spawn((Marker(9),));

    world.queue().push(DespawnBelow(5));
    assert!(world.contains(low), "not applied yet");

    world.apply();
    assert!(!world.contains(low));
    assert!(world.contains(high));
}

#[test]
fn many_commands_apply_without_losing_any() {
    let mut world = LocalWorld::new();
    let handles: Vec<Entity> = {
        let commands = world.queue();
        (0..256u32)
            .map(|value| commands.spawn((Marker(value),)))
            .collect()
    };

    world.apply();

    assert_eq!(world.len(), 256);
    for (expected, entity) in (0..256u32).zip(&handles) {
        assert_eq!(world.get::<Marker>(*entity).unwrap().0, expected);
    }
}
