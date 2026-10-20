//! Archetype storage.
//!
//! An archetype owns all entities that have exactly the same set of component
//! types, and stores one column per type. Rows of the different columns line up
//! with [`Archetype::entities`], so an entity's archetype and row are enough
//! to find every component it has.
//!
//! Archetypes are created on demand and never removed; an empty archetype costs
//! one allocation.

use core::any::{Any, TypeId};

use hashbrown::HashMap;

use crate::column::{AnyColumn, Column, borrow_conflict};
use crate::entity::Entity;
use crate::scope::ScopeOf;
use crate::world::World;

/// Which scopes an archetype's rows live in, as far as a whole-archetype gate
/// can tell.
///
/// A derived summary, not part of the archetype's identity: it is read off the
/// [`ScopeOf`] column, and the gates below are the only thing that uses it.
/// Both gates are conservative in the same direction: they never accept a row
/// that the per-row check would reject.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ScopeSummary {
    /// No rows.
    #[default]
    Empty,
    /// Every row lives in this scope.
    Uniform(Entity),
    /// Rows live in more than one scope.
    Mixed,
}

impl ScopeSummary {
    /// Whether at least one row of the archetype is visible from `caller`.
    ///
    /// This is the gate a query applies before it visits any row. A uniform
    /// archetype is either entirely visible or has exactly one visible row: the
    /// caller's own entity, which lives in `caller`'s parent scope and so sits
    /// in that parent's uniform archetype.
    pub(crate) fn any_visible(self, world: &World, caller: Entity) -> bool {
        match self {
            Self::Empty => false,
            Self::Uniform(scope) => {
                world.visible(caller, scope) || world.parent_scope(caller) == Some(scope)
            }
            Self::Mixed => true,
        }
    }

    /// Whether every row of the archetype is visible from `caller`.
    ///
    /// Vacuously true for an empty archetype, which is why the callers that
    /// hand out rows also require the archetype to be non-empty.
    pub(crate) fn all_visible(self, world: &World, caller: Entity) -> bool {
        match self {
            Self::Empty => true,
            Self::Uniform(scope) => world.visible(caller, scope),
            Self::Mixed => false,
        }
    }

    /// Whether row `row` of `archetype` is visible from `caller`.
    ///
    /// The per-row answer for a summary a whole-archetype gate could not
    /// decide. The caller's own row is always visible: a scope sees the entity
    /// it is rooted at, and that entity lives in the scope's parent.
    pub(crate) fn row_visible(
        self,
        archetype: &Archetype,
        world: &World,
        caller: Entity,
        row: usize,
    ) -> bool {
        match self {
            Self::Empty => false,
            Self::Uniform(scope) => {
                world.visible(caller, scope) || archetype.entities()[row] == caller
            }
            Self::Mixed => {
                archetype.entities()[row] == caller
                    || archetype.cell::<ScopeOf>(row).is_some_and(|cell| {
                        let parent = cell
                            .try_borrow()
                            .unwrap_or_else(|_| borrow_conflict::<ScopeOf>("read"))
                            .0;
                        world.visible(caller, parent)
                    })
            }
        }
    }
}

/// One archetype: the entities that share a component set.
pub struct Archetype {
    types: Box<[TypeId]>,
    columns: Box<[Box<dyn AnyColumn>]>,
    entities: Vec<Entity>,
    /// How many rows live in each scope, one entry per distinct scope.
    ///
    /// The scope summary is derived from this rather than stored, so it cannot
    /// go stale: an archetype whose rows all move out of one scope stops being
    /// `Mixed` the moment the last of them leaves.
    scope_counts: Vec<(Entity, u32)>,
}

impl Archetype {
    /// Build an archetype from its component types and their columns.
    ///
    /// `types` must be sorted and unique, and `columns` must line up with
    /// it. Sorting is what makes one component set one archetype: the types are
    /// the key the pool looks an archetype up by, so two spawns of the same set
    /// have to arrive in the same order.
    pub(crate) fn new(types: Box<[TypeId]>, columns: Box<[Box<dyn AnyColumn>]>) -> Self {
        debug_assert_eq!(types.len(), columns.len());
        debug_assert!(types.windows(2).all(|pair| pair[0] < pair[1]));
        Self {
            types,
            columns,
            entities: Vec::new(),
            scope_counts: Vec::new(),
        }
    }

    /// The component types, sorted.
    pub fn types(&self) -> &[TypeId] {
        &self.types
    }

    /// The entities, one per row.
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// Number of entities.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the archetype holds no entity.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// The position of `type_id` in [`Archetype::types`], if present.
    ///
    /// A scan rather than a binary search. An archetype holds the components one
    /// entity was spawned with, which a scene entity keeps to a handful, and
    /// over that range a scan wins: comparing is branch-free and vectorizes once
    /// this is inlined into its caller, while a binary search pays a
    /// mispredicted branch per probe. A scan only loses on an archetype far
    /// wider than a tuple spawn reaches, which a hand-written [`Bundle`] can
    /// still build. The sorted order [`Bundle`] gives the types is what keeps
    /// archetypes identical across spawn orders; the lookup does not rely on it.
    ///
    /// [`Bundle`]: crate::Bundle
    #[inline]
    pub(crate) fn column_index(&self, type_id: TypeId) -> Option<usize> {
        self.types
            .iter()
            .position(|&candidate| candidate == type_id)
    }

    /// The storage cell of component `C` at `row`.
    #[inline]
    pub(crate) fn cell<C: 'static>(&self, row: usize) -> Option<&core::cell::RefCell<C>> {
        self.column::<C>()?.cell(row)
    }

    /// The column of component `C`.
    ///
    /// A query resolves this once per archetype instead of doing the lookup per
    /// row; see [`Query::Fetch`](crate::Query::Fetch).
    #[inline]
    pub(crate) fn column<C: 'static>(&self) -> Option<&Column<C>> {
        let index = self.column_index(TypeId::of::<C>())?;
        self.columns[index].as_any().downcast_ref::<Column<C>>()
    }

    /// Append an already erased component value.
    ///
    /// Every caller supplies a value whose type is one of the archetype's
    /// component types.
    pub(crate) fn push_erased(&mut self, type_id: TypeId, value: Box<dyn Any>) {
        let index = self
            .column_index(type_id)
            .expect("the archetype was built with this component type");
        self.columns[index].push_boxed(value);
    }

    /// Append one entity row that lives in `scope`.
    pub(crate) fn push_entity(&mut self, entity: Entity, scope: Entity) {
        self.entities.push(entity);
        self.count_scope(scope, 1);
    }

    /// Which scopes the rows live in.
    pub(crate) fn scope_summary(&self) -> ScopeSummary {
        match self.scope_counts.as_slice() {
            [] => ScopeSummary::Empty,
            [(scope, _)] => ScopeSummary::Uniform(*scope),
            _ => ScopeSummary::Mixed,
        }
    }

    /// Move the count of one row from `from` to `to`.
    ///
    /// Called when a row's [`ScopeOf`] is rewritten: the component set is
    /// unchanged, so the row stays where it is and only the counts change.
    pub(crate) fn move_scope(&mut self, from: Entity, to: Entity) {
        self.count_scope(from, -1);
        self.count_scope(to, 1);
    }

    /// Add `delta` to the row count of `scope`, dropping the entry at zero.
    fn count_scope(&mut self, scope: Entity, delta: i32) {
        if let Some(index) = self
            .scope_counts
            .iter()
            .position(|(candidate, _)| *candidate == scope)
        {
            let count = &mut self.scope_counts[index].1;
            *count = count.wrapping_add_signed(delta);
            if *count == 0 {
                self.scope_counts.swap_remove(index);
            }
            return;
        }
        debug_assert!(delta > 0, "a scope with no rows cannot lose one");
        self.scope_counts.push((scope, delta as u32));
    }

    /// The scope the row at `row` lives in.
    ///
    /// Every archetype that holds a row has a [`ScopeOf`] column: rows are only
    /// ever pushed by [`World`], which writes the scope in the same step.
    pub(crate) fn scope_at(&self, row: usize) -> Entity {
        self.cell::<ScopeOf>(row)
            .expect("every spawned row carries its scope")
            .try_borrow()
            .unwrap_or_else(|_| borrow_conflict::<ScopeOf>("read"))
            .0
    }

    /// Drop the row at `row`, moving the last row into the hole.
    ///
    /// Returns the entity that was moved into the hole, so its location can be
    /// updated, or `None` when the row was the last one.
    pub(crate) fn swap_remove(&mut self, row: usize) -> Option<Entity> {
        let last = self.entities.len() - 1;
        let scope = self.scope_at(row);
        for column in self.columns.iter_mut() {
            drop(column.swap_remove(row));
        }
        self.entities.swap_remove(row);
        self.count_scope(scope, -1);
        (row != last).then(|| self.entities[row])
    }
}

/// Every archetype of a world.
pub struct Archetypes {
    archetypes: Vec<Archetype>,
    by_types: HashMap<Box<[TypeId]>, u32>,
}

impl Archetypes {
    /// A pool containing only the empty archetype.
    pub(crate) fn new() -> Self {
        let mut pool = Self {
            archetypes: Vec::new(),
            by_types: HashMap::new(),
        };
        pool.register(Archetype::new(Box::new([]), Box::new([])));
        pool
    }

    /// The id of the archetype with exactly `types` (which must be sorted).
    pub(crate) fn find(&self, types: &[TypeId]) -> Option<u32> {
        self.by_types.get(types).copied()
    }

    /// Register a freshly built archetype and return its id.
    pub(crate) fn register(&mut self, archetype: Archetype) -> u32 {
        let id = self.archetypes.len() as u32;
        self.by_types.insert(archetype.types().into(), id);
        self.archetypes.push(archetype);
        id
    }

    /// The archetype with this id.
    #[inline]
    pub(crate) fn get(&self, id: u32) -> &Archetype {
        &self.archetypes[id as usize]
    }

    /// The archetype with this id.
    pub(crate) fn get_mut(&mut self, id: u32) -> &mut Archetype {
        &mut self.archetypes[id as usize]
    }

    /// The number of archetypes.
    pub(crate) fn len(&self) -> usize {
        self.archetypes.len()
    }

    /// Every archetype, as a slice.
    pub(crate) fn as_slice(&self) -> &[Archetype] {
        &self.archetypes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::column::{AnyColumn, Column};

    #[test]
    fn the_empty_archetype_exists_first() {
        let archetypes: Archetypes = Archetypes::new();
        assert!(archetypes.get(0).is_empty());
        assert!(archetypes.get(0).types().is_empty());
    }

    #[test]
    fn an_archetype_is_looked_up_by_its_component_set() {
        // Archetypes keep their component types sorted, so the fixture is too.
        let mut types = [TypeId::of::<u32>(), TypeId::of::<bool>()];
        types.sort_unstable();
        let columns: Vec<Box<dyn AnyColumn>> = types
            .iter()
            .map(|type_id| {
                if *type_id == TypeId::of::<u32>() {
                    Column::<u32>::new().erase()
                } else {
                    Column::<bool>::new().erase()
                }
            })
            .collect();
        let mut archetypes: Archetypes = Archetypes::new();
        let id = archetypes.register(Archetype::new(types.into(), columns.into_boxed_slice()));
        assert_eq!(archetypes.find(&types), Some(id));
        assert_eq!(archetypes.len(), 2);
        assert_eq!(archetypes.get(id).types(), types);
    }

    /// An archetype holding one `ScopeOf` column, as every spawned row has.
    fn scoped_archetype() -> Archetype {
        Archetype::new(
            Box::new([TypeId::of::<ScopeOf>()]),
            Box::new([Column::<ScopeOf>::new().erase()]),
        )
    }

    #[test]
    fn swap_remove_reports_the_entity_moved_into_the_hole() {
        let mut archetype = scoped_archetype();
        let scope = Entity::from_raw(9, 0);
        let a = Entity::from_raw(0, 0);
        let b = Entity::from_raw(1, 0);
        let c = Entity::from_raw(2, 0);
        for entity in [a, b, c] {
            archetype.push_erased(TypeId::of::<ScopeOf>(), Box::new(ScopeOf(scope)));
            archetype.push_entity(entity, scope);
        }
        assert_eq!(archetype.swap_remove(0), Some(c), "the last row moved in");
        assert_eq!(archetype.entities(), [c, b]);
        // Removing the last row moves nothing.
        assert_eq!(archetype.swap_remove(1), None);
        assert_eq!(archetype.entities(), [c]);
    }

    #[test]
    fn the_scope_summary_follows_the_rows() {
        let mut archetype = scoped_archetype();
        let first = Entity::from_raw(9, 0);
        let second = Entity::from_raw(10, 0);
        assert_eq!(archetype.scope_summary(), ScopeSummary::Empty);
        archetype.push_erased(TypeId::of::<ScopeOf>(), Box::new(ScopeOf(first)));
        archetype.push_entity(Entity::from_raw(0, 0), first);
        archetype.push_erased(TypeId::of::<ScopeOf>(), Box::new(ScopeOf(first)));
        archetype.push_entity(Entity::from_raw(1, 0), first);
        assert_eq!(archetype.scope_summary(), ScopeSummary::Uniform(first));
        archetype.push_erased(TypeId::of::<ScopeOf>(), Box::new(ScopeOf(second)));
        archetype.push_entity(Entity::from_raw(2, 0), second);
        assert_eq!(archetype.scope_summary(), ScopeSummary::Mixed);
        // Removing the only row of a scope drops it from the count, so the
        // archetype is uniform again.
        archetype.swap_remove(2);
        assert_eq!(archetype.scope_summary(), ScopeSummary::Uniform(first));
    }

    #[test]
    fn component_values_are_stored_and_read_back() {
        let types = [TypeId::of::<u32>()];
        let columns: Vec<Box<dyn AnyColumn>> = vec![Column::<u32>::new().erase()];
        let mut archetype: Archetype = Archetype::new(types.into(), columns.into_boxed_slice());
        archetype.push_erased(TypeId::of::<u32>(), Box::new(7u32));
        archetype.push_entity(Entity::from_raw(0, 0), Entity::from_raw(9, 0));
        let value = archetype.cell::<u32>(0).unwrap().try_borrow().unwrap();
        assert_eq!(*value, 7);
    }
}
