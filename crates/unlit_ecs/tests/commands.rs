//! Deferred structural changes, from a callback that only has `&World`.

use unlit_ecs::bundle;
use unlit_ecs::{Commands, Entity, EntityAccessError, Has, World};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Spawned;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Marker(u32);

/// The shape a driver uses: a behaviour runs with a shared world, reads what
/// it needs from it, queues what it needs through `Commands`, and the driver
/// applies the queue afterwards.
fn run_behaviour(world: &World, commands: Commands<'_>) -> Entity {
    let template = world.root_scope().query::<&Spawned>().count();
    commands.spawn(bundle![Marker(template as u32), Spawned])
}

#[test]
fn a_queued_spawn_does_nothing_until_apply() {
    let mut world = World::new();
    let behavior_ran = std::cell::Cell::new(false);

    let child = {
        let commands = world.root_scope().queue();
        behavior_ran.set(true);
        run_behaviour(&world, commands)
    };

    assert!(behavior_ran.get());
    assert!(!world.root_scope().contains(child), "queued, not spawned");
    assert_eq!(world.root_scope().len(), 0);

    world.apply().unwrap();
    assert!(world.root_scope().contains(child));
    assert_eq!(
        world.root_scope().query_one::<&Marker>(child).unwrap().0,
        0,
        "the behaviour read the world before the spawn existed"
    );
    assert!(
        world
            .root_scope()
            .query_one::<Has<Spawned>>(child)
            .unwrap_or(false)
    );
}

#[test]
fn a_queued_handle_is_usable_before_apply() {
    let mut world = World::new();

    // The handle comes back immediately, so it can be stored in a component
    // on another entity before the queue is applied.
    let child = world.root_scope().queue().spawn(bundle![Marker(7)]);
    let owner = world.root_scope_mut().spawn(bundle![Marker(0), child]);

    world.apply().unwrap();

    assert!(world.root_scope().contains(child));
    assert_eq!(
        *world.root_scope().query_one::<&Entity>(owner).unwrap(),
        child
    );
}

#[test]
fn a_queued_despawn_only_takes_effect_on_apply() {
    let mut world = World::new();
    let entity = world.root_scope_mut().spawn(bundle![Marker(1)]);

    world.root_scope().queue().despawn(entity);
    assert!(
        world.root_scope().contains(entity),
        "still alive until apply"
    );

    world.apply().unwrap();
    assert!(!world.root_scope().contains(entity));
}

#[test]
fn commands_apply_in_the_order_they_were_queued() {
    let mut world = World::new();
    let first = world.root_scope().queue().spawn(bundle![Marker(1)]);
    let second = world.root_scope().queue().spawn(bundle![Marker(2)]);

    world.apply().unwrap();

    assert!(world.root_scope().contains(first));
    assert!(world.root_scope().contains(second));
    assert_eq!(world.root_scope().query_one::<&Marker>(first).unwrap().0, 1);
    assert_eq!(
        world.root_scope().query_one::<&Marker>(second).unwrap().0,
        2
    );
}

#[test]
fn commands_queued_while_applying_are_applied_too() {
    let mut world = World::new();

    let seed = {
        let commands = world.root_scope().queue();
        let seed = commands.spawn(bundle![Marker(1)]);
        commands.spawn(bundle![Marker(2)]);
        seed
    };
    world.apply().unwrap();
    assert!(world.root_scope().contains(seed));
    assert_eq!(world.root_scope().len(), 2);

    // A later batch is a fresh queue and applies on its own `apply`.
    let third = world.root_scope().queue().spawn(bundle![Marker(3)]);
    world.apply().unwrap();
    assert!(world.root_scope().contains(third));
    assert_eq!(world.root_scope().len(), 3);
}

#[test]
fn applying_an_empty_queue_changes_nothing() {
    let mut world = World::new();
    world.apply().unwrap();
    assert!(world.root_scope().is_empty());

    world.root_scope_mut().spawn(bundle![Marker(1)]);
    let before = world.root_scope().len();
    world.apply().unwrap();
    assert_eq!(world.root_scope().len(), before);
}

#[test]
fn a_custom_command_can_be_queued() {
    use unlit_ecs::Command;

    /// Despawns every entity that has a `Marker` below a threshold.
    struct DespawnBelow(u32);

    impl Command for DespawnBelow {
        fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
            let doomed: Vec<Entity> = world
                .root_scope()
                .query::<(&Marker, Entity)>()
                .filter(|(marker, _)| marker.0 < self.0)
                .map(|(_, entity)| entity)
                .collect();
            for entity in doomed {
                world.root_scope_mut().despawn(entity).unwrap();
            }
            Ok(())
        }
    }

    let mut world = World::new();
    let low = world.root_scope_mut().spawn(bundle![Marker(1)]);
    let high = world.root_scope_mut().spawn(bundle![Marker(9)]);

    world.root_scope().queue().push(DespawnBelow(5));
    assert!(world.root_scope().contains(low), "not applied yet");

    world.apply().unwrap();
    assert!(!world.root_scope().contains(low));
    assert!(world.root_scope().contains(high));
}

#[test]
fn many_commands_apply_without_losing_any() {
    let mut world = World::new();
    let handles: Vec<Entity> = {
        let commands = world.root_scope().queue();
        (0..256u32)
            .map(|value| commands.spawn(bundle![Marker(value)]))
            .collect()
    };

    world.apply().unwrap();

    assert_eq!(world.root_scope().len(), 256);
    for (expected, entity) in (0..256u32).zip(&handles) {
        assert_eq!(
            world.root_scope().query_one::<&Marker>(*entity).unwrap().0,
            expected
        );
    }
}

#[test]
fn a_spawn_on_a_released_handle_reports_the_lost_reservation() {
    let mut world = World::new();
    let doomed = world.root_scope().queue().spawn(bundle![Marker(1)]);
    let survivor = world.root_scope().queue().spawn(bundle![Marker(2)]);

    // Handing the reservation back before the queue runs leaves the `Spawn`
    // with nothing to spawn on: the error names the handle, and the rest of the
    // queue still applies.
    assert!(world.release_entity(doomed));
    assert_eq!(
        world.apply(),
        Err(EntityAccessError::NotReserved { entity: doomed })
    );
    assert!(world.root_scope().contains(survivor));
    assert!(!world.root_scope().contains(doomed));
}

/// A refused command is reported rather than dropped silently, and it does not
/// stop the rest of the queue: the caller asked for a batch, and the first
/// failure is what it gets back.
#[test]
fn a_refused_command_leaves_the_rest_of_the_queue_applied() {
    let mut world = World::new();
    let shared = world.shared();
    let scene = world.root_scope_mut().spawn(bundle![Marker(1)]);
    let resource = world.scope_mut(shared).unwrap().spawn(bundle![Marker(2)]);

    world.scope(scene).unwrap().queue().despawn(resource);
    let late = world.root_scope().queue().spawn(bundle![Marker(3)]);
    let error = world.apply().unwrap_err();
    assert_eq!(
        error,
        EntityAccessError::NotMovable {
            entity: resource,
            scope: scene,
        },
        "the shared subtree is the root's alone"
    );
    assert!(world.root_scope().contains(resource));
    assert!(
        world.root_scope().contains(late),
        "the batch ran to the end"
    );
}

/// The queue answers one error even when several commands fail, so a caller
/// that wants them all has to queue them one at a time.
#[test]
fn only_the_first_failure_is_reported() {
    let mut world = World::new();
    let shared = world.shared();
    let scene = world.root_scope_mut().spawn(bundle![Marker(1)]);
    let first = world.scope_mut(shared).unwrap().spawn(bundle![Marker(2)]);
    let second = world.scope_mut(shared).unwrap().spawn(bundle![Marker(3)]);

    let queue = world.scope(scene).unwrap().queue();
    queue.despawn(first);
    queue.despawn(second);
    assert_eq!(
        world.apply(),
        Err(EntityAccessError::NotMovable {
            entity: first,
            scope: scene,
        }),
    );
    assert!(world.root_scope().contains(second), "neither was removed");
}
