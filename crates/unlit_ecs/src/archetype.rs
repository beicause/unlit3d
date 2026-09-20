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

use crate::bundle::ErasedValue;
use crate::entity::Entity;
use crate::mode::{AnyColumn, Column, Mode};

/// One archetype: the entities that share a component set.
pub struct Archetype<M: Mode> {
    types: Box<[TypeId]>,
    columns: Box<[Box<M::ErasedColumn>]>,
    entities: Vec<Entity>,
}

impl<M: Mode> Archetype<M> {
    /// Build an archetype from its component types and their columns.
    ///
    /// `types` must be sorted and unique, and `columns` must line up with
    /// it.
    pub(crate) fn new(types: Box<[TypeId]>, columns: Box<[Box<M::ErasedColumn>]>) -> Self {
        debug_assert_eq!(types.len(), columns.len());
        debug_assert!(types.windows(2).all(|pair| pair[0] < pair[1]));
        Self {
            types,
            columns,
            entities: Vec::new(),
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
    pub(crate) fn column_index(&self, type_id: TypeId) -> Option<usize> {
        self.types.binary_search(&type_id).ok()
    }

    /// The storage cell of component `C` at `row`.
    pub(crate) fn cell<C: 'static>(&self, row: usize) -> Option<&M::Cell<C>> {
        let index = self.column_index(TypeId::of::<C>())?;
        let column = self.columns[index]
            .as_any()
            .downcast_ref::<Column<M, C>>()?;
        column.cell(row)
    }

    /// The entity at `row`.
    pub(crate) fn entity_at(&self, row: usize) -> Entity {
        self.entities[row]
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

    /// Append one entity row.
    pub(crate) fn push_entity(&mut self, entity: Entity) {
        self.entities.push(entity);
    }

    /// Drop the row at `row`, moving the last row into the hole.
    ///
    /// Returns the entity that was moved into the hole, so its location can be
    /// updated, or `None` when the row was the last one.
    pub(crate) fn swap_remove(&mut self, row: usize) -> Option<Entity> {
        let last = self.entities.len() - 1;
        for column in self.columns.iter_mut() {
            drop(column.swap_remove(row));
        }
        self.entities.swap_remove(row);
        (row != last).then(|| self.entities[row])
    }

    /// Move the row at `row` into `target`, attaching `extra` when given,
    /// then drop the row.
    ///
    /// A component that `target` has no column for is returned: that is how
    /// [`World::remove`](crate::LocalWorld::remove) takes its value back. The
    /// moved-into-the-hole entity is returned too, so its location can be
    /// updated.
    pub(crate) fn move_row(
        &mut self,
        row: usize,
        target: &mut Archetype<M>,
        extra: Option<ErasedValue>,
    ) -> (Option<Box<dyn Any>>, Option<Entity>) {
        let last = self.entities.len() - 1;
        let entity = self.entities[row];
        if let Some((type_id, value)) = extra {
            target.push_erased(type_id, value);
        }
        let mut removed = None;
        for index in 0..self.types.len() {
            let type_id = self.types[index];
            let value = self.columns[index].swap_remove(row);
            match target.column_index(type_id) {
                Some(target_index) => target.columns[target_index].push_boxed(value),
                None => {
                    debug_assert!(
                        removed.is_none(),
                        "only one component can be missing from the target archetype"
                    );
                    removed = Some(value);
                }
            }
        }
        target.push_entity(entity);
        self.entities.swap_remove(row);
        (removed, (row != last).then(|| self.entities[row]))
    }
}

/// Every archetype of a world.
pub struct Archetypes<M: Mode> {
    archetypes: Vec<Archetype<M>>,
    by_types: HashMap<Box<[TypeId]>, u32>,
}

impl<M: Mode> Archetypes<M> {
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
    pub(crate) fn register(&mut self, archetype: Archetype<M>) -> u32 {
        let id = self.archetypes.len() as u32;
        self.by_types.insert(archetype.types().into(), id);
        self.archetypes.push(archetype);
        id
    }

    /// The archetype with this id.
    pub(crate) fn get(&self, id: u32) -> &Archetype<M> {
        &self.archetypes[id as usize]
    }

    /// The archetype with this id.
    pub(crate) fn get_mut(&mut self, id: u32) -> &mut Archetype<M> {
        &mut self.archetypes[id as usize]
    }

    /// The number of archetypes.
    pub(crate) fn len(&self) -> usize {
        self.archetypes.len()
    }

    /// Mutably borrow two different archetypes at once.
    pub(crate) fn split_mut(&mut self, a: u32, b: u32) -> (&mut Archetype<M>, &mut Archetype<M>) {
        assert_ne!(a, b, "a structural change always targets another archetype");
        let (low, high) = if a < b { (a, b) } else { (b, a) };
        let (first, second) = self.archetypes.split_at_mut(high as usize);
        let low_ref = &mut first[low as usize];
        let high_ref = &mut second[0];
        if a < b {
            (low_ref, high_ref)
        } else {
            (high_ref, low_ref)
        }
    }

    /// Every archetype.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Archetype<M>> {
        self.archetypes.iter()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{Cell, ColumnErase, LocalMode};

    #[test]
    fn the_empty_archetype_exists_first() {
        let archetypes: Archetypes<LocalMode> = Archetypes::new();
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
                    Column::<LocalMode, u32>::new().erase()
                } else {
                    Column::<LocalMode, bool>::new().erase()
                }
            })
            .collect();
        let mut archetypes: Archetypes<LocalMode> = Archetypes::new();
        let id = archetypes.register(Archetype::new(types.into(), columns.into_boxed_slice()));
        assert_eq!(archetypes.find(&types), Some(id));
        assert_eq!(archetypes.len(), 2);
        assert_eq!(archetypes.get(id).types(), types);
    }

    #[test]
    fn swap_remove_reports_the_entity_moved_into_the_hole() {
        let mut archetype = Archetype::<LocalMode>::new(Box::new([]), Box::new([]));
        let a = Entity::from_raw(0, 0);
        let b = Entity::from_raw(1, 0);
        let c = Entity::from_raw(2, 0);
        for entity in [a, b, c] {
            archetype.push_entity(entity);
        }
        assert_eq!(archetype.swap_remove(0), Some(c), "the last row moved in");
        assert_eq!(archetype.entities(), [c, b]);
        // Removing the last row moves nothing.
        assert_eq!(archetype.swap_remove(1), None);
        assert_eq!(archetype.entities(), [c]);
    }

    #[test]
    fn component_values_are_stored_and_read_back() {
        let types = [TypeId::of::<u32>()];
        let columns: Vec<Box<dyn AnyColumn>> = vec![Column::<LocalMode, u32>::new().erase()];
        let mut archetype: Archetype<LocalMode> =
            Archetype::new(types.into(), columns.into_boxed_slice());
        archetype.push_erased(TypeId::of::<u32>(), Box::new(7u32));
        archetype.push_entity(Entity::from_raw(0, 0));
        let value = archetype.cell::<u32>(0).unwrap().try_read().unwrap();
        assert_eq!(*value, 7);
    }
}
