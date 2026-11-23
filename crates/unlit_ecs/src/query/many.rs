//! The holder a scope hands out for a query over a list of entities.

use core::marker::PhantomData;
use core::panic::Location;

use crate::borrow::Claims;
use crate::entity::Entity;
use crate::error::{EntityAccessError, ListAccessError};
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

/// What the list has made of an archetype, as of the first entity of the list
/// that lives in it.
#[derive(Clone, Copy)]
enum Resolved {
    /// No entity of the list has reached this archetype yet.
    Unseen,
    /// The archetype does not satisfy the query, so none of its rows can be
    /// visited.
    Refused,
    /// The archetype resolved to this group.
    Group(usize),
}

/// What a scope hands out for a query over a list of entities.
///
/// Built by [`crate::Scope::query_many`]. The whole list is checked when the
/// holder is built: every entity must be alive, visible from the scope, and
/// match the query. Every entity that fails one of these is reported in a
/// [`ListAccessError`], and no item exists in that case, so a refused list
/// writes nothing.
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
    /// Checks every entry of the list and gathers each refusal, so the caller
    /// sees the problems of the whole list at once.
    pub(crate) fn new(
        scope: &Scope<'w>,
        entities: &[Entity],
        origin: &'static Location<'static>,
    ) -> Result<Self, ListAccessError> {
        let mut claims = Claims::new(scope.world);
        let mut groups: Vec<Group<'w, Q>> = Vec::new();
        let mut resolved = vec![Resolved::Unseen; scope.world.archetypes().as_slice().len()];
        let mut visits = Vec::with_capacity(entities.len());
        let mut failures = Vec::new();

        for &entity in entities {
            match admit(
                scope,
                entity,
                &mut claims,
                &mut groups,
                &mut resolved,
                origin,
            ) {
                Ok(visit) => visits.push(visit),
                Err(failure) => failures.push(failure),
            }
        }
        if !failures.is_empty() {
            return Err(ListAccessError { failures });
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

/// Places one entry of the list, answering with its visit or with why it was
/// refused.
fn admit<'w, Q: Query>(
    scope: &Scope<'w>,
    entity: Entity,
    claims: &mut Claims<'w>,
    groups: &mut Vec<Group<'w, Q>>,
    resolved: &mut [Resolved],
    origin: &'static Location<'static>,
) -> Result<Visit, EntityAccessError> {
    let location = scope.location(entity)?;
    let archetype_id = location.archetype();
    let archetype = scope.world.archetypes().get(archetype_id)?;
    let row = location.row();
    if !scope.row_visible(archetype, row) {
        return Err(EntityAccessError::NotVisible {
            entity,
            scope: scope.scope,
        });
    }

    let slot = &mut resolved[archetype_id as usize];
    let group = match *slot {
        Resolved::Group(group) => group,
        Resolved::Refused => return Err(EntityAccessError::NotMatched { entity }),
        Resolved::Unseen => {
            let mut builder = PlanBuilder::new(archetype, Vec::new());
            match Q::prepare(&mut builder) {
                Some(plan) => {
                    claims.claim_archetype(archetype_id, archetype, builder.asks(), origin);
                    groups.push(Group {
                        plan,
                        named: vec![0; archetype.len().div_ceil(64)],
                    });
                    *slot = Resolved::Group(groups.len() - 1);
                    groups.len() - 1
                }
                None => {
                    *slot = Resolved::Refused;
                    return Err(EntityAccessError::NotMatched { entity });
                }
            }
        }
    };

    let (word, bit) = (row / 64, 1u64 << (row % 64));
    let named = &mut groups[group].named[word];
    if *named & bit != 0 {
        return Err(EntityAccessError::Repeated { entity });
    }
    *named |= bit;
    Ok(Visit { group, row })
}
