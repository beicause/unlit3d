//! Querying entities through the public API.

use core::any::TypeId;

use unlit_ecs::{Archetype, Entity, Has, World, bundle};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Position {
    x: f32,
    y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Velocity {
    dx: f32,
    dy: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Visible;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hidden;

fn scene() -> (World, Entity, Entity, Entity) {
    let mut world = World::new();
    // Moving and visible.
    let mover = world.root_scope_mut().spawn(bundle![
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 1.0, dy: 0.0 },
        Visible,
    ]);
    // Moving but hidden.
    let hidden = world.root_scope_mut().spawn(bundle![
        Position { x: 10.0, y: 0.0 },
        Velocity { dx: 2.0, dy: 0.0 },
        Hidden,
    ]);
    // Visible but still.
    let still = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 20.0, y: 0.0 }, Visible]);
    (world, mover, hidden, still)
}

#[test]
fn a_single_component_query_visits_every_holder() {
    let (world, _, _, _) = scene();

    assert_eq!(world.root_scope().query::<&Position>().iter().count(), 3);
    assert_eq!(world.root_scope().query::<&Velocity>().iter().count(), 2);
    assert_eq!(world.root_scope().query::<&Visible>().iter().count(), 2);
}

#[test]
fn the_scaffolding_the_archetypes_share_is_skipped_by_position() {
    let mut world = World::new();
    // Bare entities land in the same archetype as the two scaffolding roots, so
    // a query that visits it has to step over them without a per-row test.
    let first = world.root_scope_mut().spawn(bundle![]);
    let second = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 1.0, y: 2.0 }]);
    let third = world.root_scope_mut().spawn(bundle![]);

    let mut found: Vec<Entity> = world.root_scope().query::<Entity>().iter().collect();
    found.sort();
    let mut expected = vec![first, second, third];
    expected.sort();
    assert_eq!(found, expected);
    assert_eq!(world.root_scope().len(), 3);

    // The same archetype keeps the roots at the front while its content churns.
    assert!(world.root_scope_mut().despawn(first).is_ok());
    assert_eq!(world.root_scope().query::<Entity>().iter().count(), 2);
    assert_eq!(world.root_scope().len(), 2);

    // A query that names a component skips the archetype whole, so the bare
    // entities are neither visited nor reported as carrying it.
    assert_eq!(world.root_scope().query::<&Position>().iter().count(), 1);
    assert_eq!(
        world
            .root_scope()
            .query::<Has<Position>>()
            .iter()
            .filter(|&has| has)
            .count(),
        1,
    );
}

#[test]
fn a_two_component_query_requires_every_component() {
    let (world, mover, hidden, _) = scene();

    let mut found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position, &Velocity)>()
        .iter()
        .map(|(entity, _, _)| entity)
        .collect();
    found.sort();

    let mut expected = vec![mover, hidden];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn a_query_yields_its_items_in_the_order_written() {
    let (world, mover, _, _) = scene();
    let mut position_rows = world.root_scope().query::<(&Position, Entity)>();

    // The component is written first and the entity second, and that is the
    // order they come out in.
    let (position, entity) = position_rows.iter().next().unwrap();
    assert_eq!(entity, mover);
    assert_eq!(position.x, 0.0);
}

#[test]
fn a_query_can_fetch_the_entity_alone() {
    let (world, _, _, _) = scene();

    let entities: Vec<Entity> = world.root_scope().query::<Entity>().iter().collect();
    assert_eq!(entities.len(), 3);
}

#[test]
fn mutable_components_are_written_through_a_query() {
    let (world, mover, hidden, still) = scene();

    for (position, velocity) in world
        .root_scope()
        .query::<(&mut Position, &Velocity)>()
        .iter()
    {
        position.x += velocity.dx;
        position.y += velocity.dy;
    }

    assert_eq!(
        *world
            .root_scope()
            .query_one::<&Position>(mover)
            .unwrap()
            .get(),
        Position { x: 1.0, y: 0.0 }
    );
    assert_eq!(
        *world
            .root_scope()
            .query_one::<&Position>(hidden)
            .unwrap()
            .get(),
        Position { x: 12.0, y: 0.0 }
    );
    assert_eq!(
        *world
            .root_scope()
            .query_one::<&Position>(still)
            .unwrap()
            .get(),
        Position { x: 20.0, y: 0.0 },
        "no velocity"
    );
}

#[test]
fn optional_components_are_none_when_missing() {
    let (world, mover, _, still) = scene();

    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query::<(Entity, &Position, Option<&Velocity>)>()
        .iter()
        .map(|(entity, _, velocity)| (entity, velocity.map(|velocity| velocity.dx)))
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_component_alone_yields_none_for_holders_without_it() {
    let (world, mover, hidden, still) = scene();

    // An optional item matches every archetype, so all three entities are
    // visited; the one without a velocity yields `None`.
    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query::<(Entity, Option<&Velocity>)>()
        .iter()
        .map(|(entity, velocity)| (entity, velocity.map(|velocity| velocity.dx)))
        .collect();

    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(hidden, Some(2.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn an_optional_mutable_component_writes_only_where_present() {
    let (world, mover, hidden, still) = scene();

    for velocity in world
        .root_scope()
        .query::<Option<&mut Velocity>>()
        .iter()
        .flatten()
    {
        velocity.dx += 10.0;
    }

    assert_eq!(
        world
            .root_scope()
            .query_one::<&Velocity>(mover)
            .unwrap()
            .get()
            .dx,
        11.0
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Velocity>(hidden)
            .unwrap()
            .get()
            .dx,
        12.0
    );
    assert!(
        world.root_scope().query_one::<&Velocity>(still).is_err(),
        "the still entity has no velocity to write"
    );
}

#[test]
fn a_filter_narrows_which_archetypes_are_visited() {
    let (world, mover, _, still) = scene();

    // The filter visits only the visible archetypes; one of them has a velocity
    // and the other does not.
    let seen: Vec<(Entity, Option<f32>)> = world
        .root_scope()
        .query::<(Entity, Option<&Velocity>)>()
        .by(|archetype: &Archetype| archetype.has::<Visible>())
        .iter()
        .map(|(entity, velocity)| (entity, velocity.map(|velocity| velocity.dx)))
        .collect();

    assert_eq!(seen.len(), 2);
    assert!(seen.contains(&(mover, Some(1.0))));
    assert!(seen.contains(&(still, None)));
}

#[test]
fn a_filter_can_forbid_a_component() {
    let (world, _, _, still) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| !archetype.has::<Velocity>())
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [still]);
}

#[test]
fn a_filter_can_require_two_components() {
    let (world, _, hidden, _) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| archetype.has::<Velocity>() && archetype.has::<Hidden>())
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [hidden]);
}

#[test]
fn a_filter_can_take_the_union_of_two_components() {
    let (world, mover, hidden, _) = scene();

    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| archetype.has::<Velocity>() || archetype.has::<Hidden>())
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found.len(), 2);
    assert!(found.contains(&mover));
    assert!(found.contains(&hidden));
}

#[test]
fn a_filter_can_read_a_component_set_known_only_at_run_time() {
    let (world, mover, _, _) = scene();

    // A component set assembled at run time: only the mover carries all three.
    let wanted = [
        TypeId::of::<Position>(),
        TypeId::of::<Velocity>(),
        TypeId::of::<Visible>(),
    ];
    let found: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .by(|archetype: &Archetype| {
            wanted
                .iter()
                .all(|type_id| archetype.types().contains(type_id))
        })
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(found, [mover]);
}

#[test]
fn query_one_reads_a_component_the_filter_would_have_rejected() {
    let (world, mover, hidden, _) = scene();

    // A filter narrows archetypes, which is a whole-archetype decision; a
    // one-entity lookup asks about that entity's archetype only.
    for (entity, has_velocity) in world
        .root_scope()
        .query::<(Entity, Has<Velocity>)>()
        .by(|archetype: &Archetype| archetype.has::<Hidden>())
        .iter()
    {
        assert_eq!(entity, hidden);
        assert!(has_velocity);
    }
    assert!(
        world
            .root_scope()
            .query_one::<Has<Velocity>>(mover)
            .unwrap()
            .get()
    );
}

#[test]
fn a_query_over_entities_with_no_matches_is_empty() {
    /// No entity in the scene was spawned with this component.
    #[derive(Clone, Copy, Debug)]
    struct Unused;

    let (world, _, _, _) = scene();
    assert_eq!(world.root_scope().query::<&Unused>().iter().count(), 0);
}

#[test]
fn iteration_is_deterministic_between_runs() {
    let (world, _, _, _) = scene();

    let first: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    let second: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Position)>()
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(first, second);
}

#[test]
fn a_query_skips_an_empty_archetype() {
    let mut world = World::new();
    // Creating and emptying an archetype must not confuse iteration.
    let entity = world.root_scope_mut().spawn(bundle![
        Position { x: 0.0, y: 0.0 },
        Velocity { dx: 1.0, dy: 1.0 },
    ]);
    world.root_scope_mut().despawn(entity).unwrap();
    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 5.0, y: 5.0 }]);

    let mut empty = world.root_scope().query::<(&Position, &Velocity)>();
    assert!(empty.iter().next().is_none());
    assert_eq!(world.root_scope().query::<&Position>().iter().count(), 1);
}

#[test]
fn has_reports_a_component_without_borrowing_it() {
    let (world, mover, _, still) = scene();
    let mut position_row = world
        .root_scope()
        .query_one::<&mut Position>(mover)
        .unwrap();

    // An exclusive borrow of the component held across the lookup is no
    // obstacle, which is what makes `Has` usable beside an exclusive item.
    let position = position_row.get();
    assert!(
        world
            .root_scope()
            .query_one::<Has<Position>>(mover)
            .unwrap()
            .get()
    );
    // `Has` narrows nothing, so a component the entity lacks answers `false`
    // rather than dropping the row.
    assert_eq!(
        world
            .root_scope()
            .query_one::<Has<Velocity>>(still)
            .map(|mut holder| holder.get()),
        Ok(false)
    );
    position.x = 42.0;
}

#[test]
fn has_narrows_nothing_and_by_narrows_for_it() {
    let (world, _, _, _) = scene();

    // Every row is visited, including the one entity without a `Velocity`.
    assert_eq!(
        world.root_scope().query::<Has<Velocity>>().iter().count(),
        3
    );
    // Narrowing to the carriers is the predicate's job.
    assert_eq!(
        world
            .root_scope()
            .query::<Has<Velocity>>()
            .by(|archetype: &Archetype| archetype.has::<Velocity>())
            .iter()
            .count(),
        2
    );
}

#[test]
fn by_applies_to_the_archetype_being_visited() {
    let (world, _, _, _) = scene();

    // The filter decides an archetype at a time, and is attached to the holder
    // before anything is visited.
    let narrowed = world
        .root_scope()
        .query::<&Position>()
        .by(|archetype: &Archetype| archetype.has::<Velocity>())
        .iter()
        .count();
    let whole = world.root_scope().query::<&Position>().iter().count();
    assert_eq!(narrowed, whole - 1, "the one archetype without a Velocity");
}

#[test]
fn has_beside_a_component_is_not_a_duplicate() {
    let (world, _, _, _) = scene();

    // Two queries over the same component, written as two distinct types, are
    // fine: each borrows once.
    assert_eq!(
        world
            .root_scope()
            .query::<(&Position, Has<Position>)>()
            .iter()
            .count(),
        3
    );
}

#[test]
fn query_one_yields_the_row_of_one_entity() {
    let (world, mover, hidden, still) = scene();

    assert_eq!(
        *world
            .root_scope()
            .query_one::<&Position>(mover)
            .unwrap()
            .get(),
        Position { x: 0.0, y: 0.0 }
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<(&Position, Entity)>(hidden)
            .unwrap()
            .get()
            .1,
        hidden
    );
    assert!(world.root_scope().query_one::<&Velocity>(still).is_err());
    assert!(
        world
            .root_scope()
            .query_one::<Has<Visible>>(mover)
            .unwrap()
            .get()
    );
}

#[test]
fn query_one_answers_none_for_an_entity_out_of_reach() {
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle![1u32]);
    let child = world
        .root_scope_mut()
        .scope_mut(parent)
        .unwrap()
        .spawn(bundle![2u32]);

    let child_scope = world.root_scope().scope(child).unwrap();
    assert!(child_scope.query_one::<&u32>(child).is_ok());
    assert!(
        child_scope.query_one::<&u32>(parent).is_err(),
        "a scope does not see upwards"
    );
}

/// An optional item is optional about the component's absence, not about the
/// borrow: a conflict panics instead of yielding `None`, which would hide a
/// real conflict as a missing component.
#[test]
#[should_panic(expected = "already borrowed")]
fn an_optional_component_that_cannot_be_borrowed_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);
    let mut position_row_2 = world
        .root_scope()
        .query_one::<&mut Position>(entity)
        .unwrap();

    let _writing = position_row_2.get();
    for _ in world.root_scope().query::<Option<&Position>>().iter() {}
}

#[test]
#[should_panic(expected = "already borrowed")]
fn an_optional_mutable_component_that_cannot_be_borrowed_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);
    let mut position_row_3 = world.root_scope().query_one::<&Position>(entity).unwrap();

    let _reading = position_row_3.get();
    for _ in world.root_scope().query::<Option<&mut Position>>().iter() {}
}

/// Naming a component twice as a shared item is two shared claims, which do not
/// conflict: the row is handed out twice.
#[test]
fn asking_for_the_same_component_twice_reads_it_twice() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 1.0, y: 2.0 }]);

    for (left, right) in world.root_scope().query::<(&Position, &Position)>().iter() {
        assert_eq!(left.x, right.x);
    }
}

/// A shared and an exclusive item over one component are two claims that cannot
/// share a row, so the holder refuses to be built.
#[test]
#[should_panic(expected = "already borrowed")]
fn mixing_a_shared_and_an_exclusive_fetch_panics() {
    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);

    for _ in world
        .root_scope()
        .query::<(&Position, &mut Position)>()
        .iter()
    {}
}

#[test]
#[should_panic(expected = "already borrowed")]
fn reading_a_component_while_it_is_written_panics() {
    let mut world = World::new();
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Position { x: 0.0, y: 0.0 }]);
    let mut position_row_4 = world
        .root_scope()
        .query_one::<&mut Position>(entity)
        .unwrap();

    let _writing = position_row_4.get();
    let _ = world.root_scope().query_one::<&Position>(entity);
}

/// A query declared with the `query!` macro: the fields are the shape of one
/// row, and the components it claims are exactly the ones it names.
#[deny(unsafe_code)]
mod declared {
    // The macro matches the field types itself, so `Entity` and `Has` need
    // no import for a declaration that uses them.
    use unlit_ecs::{World, bundle, query};

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Health(pub i32);

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Armor(pub i32);

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Mana(pub i32);

    query! {
        /// One combatant, as the combat pass reads it.
        #[derive(Debug)]
        pub struct Combatant<'a> {
            pub entity: Entity,
            pub health: &'a mut Health,
            pub armor: &'a Armor,
            pub mana: Option<&'a Mana>,
            pub has_armor: Has<Armor>,
        }
    }

    pub fn scene() -> World {
        let mut world = World::new();
        world
            .root_scope_mut()
            .spawn(bundle![Health(10), Armor(2), Mana(1)]);
        world.root_scope_mut().spawn(bundle![Health(20), Armor(4)]);
        world
    }

    #[test]
    fn a_declared_query_reads_every_field_it_names() {
        let world = scene();
        let mut rows = world.root_scope().query::<Combatant>();

        let mut seen = 0;
        for row in rows.iter() {
            seen += 1;
            assert!(row.has_armor);
            match row.mana {
                Some(mana) => assert_eq!(mana.0, 1),
                None => assert_eq!(row.health.0, 20),
            }
            assert_eq!(row.armor.0 * 5, row.health.0);
            let _ = row.entity;
        }
        assert_eq!(seen, 2);
    }

    #[test]
    fn a_declared_query_writes_the_components_it_names_exclusively() {
        let world = scene();
        for row in world.root_scope().query::<Combatant>().iter() {
            row.health.0 -= row.armor.0;
        }

        let mut healths: Vec<i32> = world
            .root_scope()
            .query::<&Health>()
            .iter()
            .map(|health| health.0)
            .collect();
        healths.sort_unstable();
        assert_eq!(healths, vec![8, 16]);
    }

    #[test]
    fn a_declared_query_is_one_type_whatever_lifetime_names_it() {
        let world = scene();
        assert_eq!(world.root_scope().query::<Combatant>().iter().count(), 2);
        assert_eq!(
            world.root_scope().query::<Combatant<'static>>().iter().count(),
            2,
        );
        assert_eq!(world.root_scope().query::<Combatant<'_>>().iter().count(), 2);
    }

    #[test]
    fn a_declared_query_reports_a_missing_required_component_as_no_match() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(bundle![Health(1)]);
        assert!(
            world.root_scope().query_one::<Combatant>(entity).is_err(),
            "a declared query requires every field that is not optional",
        );
    }
}

/// A query wider than any tuple impl, declared with the macro.
#[deny(unsafe_code)]
mod wide {
    use unlit_ecs::{World, bundle, query};

    macro_rules! components {
        ($($name:ident),+ $(,)?) => {
            $(
                #[derive(Clone, Copy, Debug, PartialEq)]
                pub struct $name(pub u32);
            )+
        };
    }

    components!(C0, C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15, C16, C17);

    query! {
        /// Eighteen components, past the widest tuple impl.
        pub struct Wide<'a> {
            pub c0: &'a C0,
            pub c1: &'a C1,
            pub c2: &'a C2,
            pub c3: &'a C3,
            pub c4: &'a C4,
            pub c5: &'a C5,
            pub c6: &'a C6,
            pub c7: &'a C7,
            pub c8: &'a C8,
            pub c9: &'a C9,
            pub c10: &'a C10,
            pub c11: &'a C11,
            pub c12: &'a C12,
            pub c13: &'a C13,
            pub c14: &'a C14,
            pub c15: &'a C15,
            pub c16: &'a C16,
            pub c17: &'a C17,
        }
    }

    #[test]
    fn a_declared_query_covers_more_fields_than_a_tuple_impl() {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![
            C0(0), C1(1), C2(2), C3(3), C4(4), C5(5), C6(6), C7(7), C8(8), C9(9), C10(10), C11(11),
            C12(12), C13(13), C14(14), C15(15), C16(16), C17(17),
        ]);

        let rows: Vec<u32> = world
            .root_scope()
            .query::<Wide>()
            .iter()
            .map(|row| {
                row.c0.0
                    + row.c1.0
                    + row.c2.0
                    + row.c3.0
                    + row.c4.0
                    + row.c5.0
                    + row.c6.0
                    + row.c7.0
                    + row.c8.0
                    + row.c9.0
                    + row.c10.0
                    + row.c11.0
                    + row.c12.0
                    + row.c13.0
                    + row.c14.0
                    + row.c15.0
                    + row.c16.0
                    + row.c17.0
            })
            .collect();
        assert_eq!(rows, vec![(0..18).sum::<u32>()]);
    }
}

/// A query written by hand against the public surface, with no `unsafe` on
/// the caller's side: the builder hands out claimed columns and refuses the
/// asks that could not coexist.
#[deny(unsafe_code)]
mod handwritten {
    use unlit_ecs::{Col, ColMut, PlanBuilder, Query, World, bundle};

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Position {
        pub x: f32,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Velocity {
        pub dx: f32,
    }

    /// Writes the position it reads the velocity of.
    pub struct Step<'a> {
        pub position: &'a mut Position,
        pub velocity: &'a Velocity,
    }

    pub struct StepPlan<'w> {
        position: ColMut<'w, Position>,
        velocity: Col<'w, Velocity>,
    }

    impl<'a> Query for Step<'a> {
        type Item<'row> = Step<'row>;
        type Plan<'w> = StepPlan<'w>;

        fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
            Some(StepPlan {
                position: builder.write::<Position>()?,
                velocity: builder.read::<Velocity>()?,
            })
        }

        fn read<'row, 'w: 'row>(plan: &'row mut Self::Plan<'w>, row: usize) -> Self::Item<'row> {
            Step {
                position: plan.position.at(row),
                velocity: plan.velocity.at(row),
            }
        }
    }

    #[test]
    fn a_hand_written_query_steps_the_rows_it_matches() {
        let mut world = World::new();
        world
            .root_scope_mut()
            .spawn(bundle![Position { x: 0.0 }, Velocity { dx: 2.0 }]);

        for row in world.root_scope().query::<Step>().iter() {
            row.position.x += row.velocity.dx;
        }
        assert_eq!(
            world
                .root_scope()
                .query::<&Position>()
                .iter()
                .next()
                .unwrap()
                .x,
            2.0,
        );

        // It visits only the archetypes carrying both components, because a
        // required ask answers `None` for one that carries neither.
        let other = world.root_scope_mut().spawn(bundle![Position { x: 1.0 }]);
        assert_eq!(world.root_scope().query::<Step>().iter().count(), 1);
        assert!(world.root_scope().query_one::<Step>(other).is_err());
    }
}

/// A declared query that names one component both ways is refused by the
/// builder, before any rows are handed out.
mod clashing {
    use unlit_ecs::{World, bundle, query};

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Marker(pub u32);

    query! {
        pub struct Clashing<'a> {
            pub read: &'a Marker,
            pub write: &'a mut Marker,
        }
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn a_declared_query_cannot_name_one_component_both_ways() {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![Marker(0)]);
        for _ in world.root_scope().query::<Clashing>().iter() {}
    }
}

/// Items nest: a tuple of tuples asks for the components of the flat tuple and
/// yields the nested shape.
mod nested {
    use unlit_ecs::{World, bundle};

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct A(pub u32);

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct B(pub u32);

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct C(pub u32);

    #[test]
    fn a_nested_tuple_query_reads_the_rows_a_flat_one_does() {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![A(1), B(2), C(3)]);
        world.root_scope_mut().spawn(bundle![A(4), B(5)]);

        let mut rows: Vec<(u32, (u32, u32))> = world
            .root_scope()
            .query::<(&A, (&B, &C))>()
            .iter()
            .map(|(a, (b, c))| (a.0, (b.0, c.0)))
            .collect();
        rows.sort_unstable();
        assert_eq!(rows, [(1, (2, 3))]);
    }

    #[test]
    fn a_nested_tuple_writes_where_a_flat_one_would() {
        let mut world = World::new();
        let entity = world.root_scope_mut().spawn(bundle![A(1), B(2)]);

        for (a, (b,)) in world.root_scope().query::<(&mut A, (&mut B,))>().iter() {
            a.0 += b.0;
            b.0 = 0;
        }

        let scope = world.root_scope();
        assert_eq!(scope.query_one::<&A>(entity).unwrap().get().0, 3);
        assert_eq!(scope.query_one::<&B>(entity).unwrap().get().0, 0);
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn a_nested_tuple_still_refuses_one_component_named_both_ways() {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![A(1)]);
        for _ in world.root_scope().query::<(&A, (&mut A,))>().iter() {}
    }
}
