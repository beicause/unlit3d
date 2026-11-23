//! The holder a scope hands out for a query over a list of entities.

use core::marker::PhantomData;
use core::panic::Location;

use crate::borrow::Claims;
use crate::entity::Entity;
use crate::error::EntityAccessError;
use crate::query::{PlanBuilder, Query};
use crate::view::Scope;

/// What one archetype of the list resolves to, and which of its rows the list
/// names.
struct Group<'w, Q: Query> {
    plan: Q::Plan<'w>,
    /// One bit per row of the archetype, set once the list has named that row.
    named: Vec<u64>,
}

/// One entity of the list: the group its archetype resolved to, and its row.
#[derive(Clone, Copy)]
struct Visit {
    group: usize,
    row: usize,
}

/// What a scope hands out for a query over a list of entities.
///
/// Built by [`crate::Scope::query_many`]. The whole list is checked when the
/// holder is built: every entity must be alive, visible from the scope, and
/// match the query. The first one that is not turns the whole list down before
/// any item exists, so a failed list writes nothing.
///
/// Each archetype the list touches is resolved once and its components are
/// claimed once for the whole list, however the entities are ordered. Items
/// come out in the order the list names the entities.
///
/// An entity named twice is refused. Two items would reach the same row, and
/// the claims cannot tell that apart from one reader asking twice.
pub struct QueryMany<'w, Q: Query> {
    /// Declared before the claims, so the plans are gone before the claims are
    /// released.
    groups: Vec<Group<'w, Q>>,
    visits: Vec<Visit>,
    /// Held for the life of the holder: dropping it is what releases the claims.
    _claims: Claims<'w>,
    marker: PhantomData<fn() -> Q>,
}

impl<'w, Q: Query> QueryMany<'w, Q> {
    pub(crate) fn new(
        scope: &Scope<'w>,
        entities: &[Entity],
        origin: &'static Location<'static>,
    ) -> Result<Self, EntityAccessError> {
        let world = scope.world;
        let mut claims = Claims::new(world);
        let mut groups: Vec<Group<'w, Q>> = Vec::new();
        // The group each archetype id resolved to, once the list has reached it.
        let mut group_of: Vec<Option<usize>> = vec![None; world.archetypes().as_slice().len()];
        let mut visits = Vec::with_capacity(entities.len());

        for &entity in entities {
            let location = scope.location(entity)?;
            let archetype_id = location.archetype();
            let archetype = world.archetypes().get(archetype_id)?;
            let row = location.row();
            if !scope.row_visible(archetype, row) {
                return Err(EntityAccessError::NotMatched { entity });
            }

            let group = match group_of[archetype_id as usize] {
                Some(group) => group,
                None => {
                    let mut builder = PlanBuilder::new(archetype, Vec::new());
                    let Some(plan) = Q::prepare(&mut builder) else {
                        return Err(EntityAccessError::NotMatched { entity });
                    };
                    claims.claim_archetype(archetype_id, archetype, builder.asks(), origin);
                    groups.push(Group {
                        plan,
                        named: vec![0; archetype.len().div_ceil(64)],
                    });
                    group_of[archetype_id as usize] = Some(groups.len() - 1);
                    groups.len() - 1
                }
            };

            let (word, bit) = (row / 64, 1u64 << (row % 64));
            let named = &mut groups[group].named[word];
            if *named & bit != 0 {
                return Err(EntityAccessError::Repeated { entity });
            }
            *named |= bit;
            visits.push(Visit { group, row });
        }

        Ok(Self {
            groups,
            visits,
            _claims: claims,
            marker: PhantomData,
        })
    }

    /// Visit every entity of the list in the order it was given, handing each
    /// item to `visit`.
    ///
    /// The item is only valid for the call, so an exclusive item cannot be kept
    /// while the next one is made.
    pub fn for_each(&mut self, mut visit: impl for<'a> FnMut(Q::Item<'a>)) {
        for &Visit { group, row } in &self.visits {
            visit(Q::read(&mut self.groups[group].plan, row));
        }
    }
}
