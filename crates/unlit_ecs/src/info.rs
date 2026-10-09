//! Plain-data summaries of a world's shape.
//!
//! A world stores components behind type-erased columns, so a caller that only
//! has a [`World`] can count entities and archetypes but cannot name a
//! component: the names live in a side table keyed by [`TypeId`]. These
//! structures put a name back on every column and borrow nothing from the world,
//! which is what a bridge that reports a world over JSON needs.
//!
//! They are deliberately shallow. Nothing here reaches into a component's own
//! fields — that is the caller's business, and a caller that wants it derives
//! its own reflection.
//!
//! [`TypeId`]: core::any::TypeId

use crate::archetype::Archetype;
use crate::entity::Entity;
use crate::world::World;

/// A live entity's location and component names.
///
/// Only a live entity has one: [`World::entity_info`] answers `None` for a
/// handle that is not alive, so there is no `alive` field to report.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct EntityInfo {
    /// The entity's handle.
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = crate::EntityProxy))]
    pub entity: Entity,
    /// The archetype the entity lives in, as [`Location::archetype`].
    ///
    /// [`Location::archetype`]: crate::Location::archetype
    pub archetype: u32,
    /// The entity's row in that archetype, as [`Location::row`].
    ///
    /// [`Location::row`]: crate::Location::row
    pub row: usize,
    /// The component names of the entity's archetype, in column order.
    pub components: Vec<&'static str>,
}

/// One archetype: its id, its length, and its component names.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ArchetypeInfo {
    /// The archetype id, the value [`World::archetype`] takes and
    /// [`EntityInfo::archetype`] carries.
    pub index: u32,
    /// The number of entities in it, as [`Archetype::len`].
    pub len: usize,
    /// Its component names, in column order.
    pub components: Vec<&'static str>,
}

/// A world's high-level shape.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct WorldInfo {
    /// The number of live entities, as [`World::len`].
    pub entities: usize,
    /// The number of archetypes, including the empty one, as
    /// [`World::archetype_count`].
    pub archetypes: usize,
    /// The names of the component types that have entered the world, sorted.
    ///
    /// This is the world's own inventory, not a registry of types a bridge can
    /// encode: a type no entity has ever carried is absent, and a type that
    /// entered but has no entity left is still present.
    pub components: Vec<&'static str>,
}

impl World {
    /// A world's high-level shape.
    #[must_use]
    pub fn info(&self) -> WorldInfo {
        let mut components: Vec<&'static str> = self
            .archetypes()
            .flat_map(|archetype| self.component_names(archetype))
            .collect();
        components.sort_unstable();
        components.dedup();
        WorldInfo {
            entities: self.len(),
            archetypes: self.archetype_count(),
            components,
        }
    }

    /// Every archetype's id, length and component names, in id order.
    ///
    /// The empty archetype is included, so the ids line up with
    /// [`ArchetypeInfo::index`] and [`World::archetype`].
    #[must_use]
    pub fn archetype_infos(&self) -> Vec<ArchetypeInfo> {
        self.archetypes()
            .enumerate()
            .map(|(index, archetype)| ArchetypeInfo {
                index: index as u32,
                len: archetype.len(),
                components: self.component_names(archetype),
            })
            .collect()
    }

    /// A live entity's location and component names, or `None` when the handle
    /// is not alive.
    #[must_use]
    pub fn entity_info(&self, entity: Entity) -> Option<EntityInfo> {
        let location = self.location(entity)?;
        let archetype = self.archetype(location.archetype())?;
        Some(EntityInfo {
            entity,
            archetype: location.archetype(),
            row: location.row(),
            components: self.component_names(archetype),
        })
    }

    /// At most `limit` live entities' information, in storage order.
    ///
    /// Storage order groups an archetype's entities together and follows the
    /// archetype ids; it is stable for a world that is not mutated.
    #[must_use]
    pub fn entity_infos(&self, limit: usize) -> Vec<EntityInfo> {
        let mut infos = Vec::new();
        for (index, archetype) in self.archetypes().enumerate() {
            let components = self.component_names(archetype);
            for (row, &entity) in archetype.entities().iter().enumerate() {
                if infos.len() == limit {
                    return infos;
                }
                infos.push(EntityInfo {
                    entity,
                    archetype: index as u32,
                    row,
                    components: components.clone(),
                });
            }
        }
        infos
    }

    /// The names of an archetype's columns, in column order.
    ///
    /// Every type of an archetype has a name: a type is registered when the
    /// bundle that first carried it is spawned, before the archetype exists.
    fn component_names(&self, archetype: &Archetype) -> Vec<&'static str> {
        archetype
            .types()
            .iter()
            .map(|&type_id| self.type_name(type_id).expect("a column's type is named"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::entity::Entity;
    use crate::world::World;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Marker(u32);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Name(&'static str);

    #[test]
    fn a_world_reports_its_counts_and_component_names() {
        let mut world = World::new();
        world.spawn((Marker(1), Name("a")));
        world.spawn((Marker(2),));
        let info = world.info();
        assert_eq!(info.entities, 2);
        assert_eq!(info.archetypes, 3, "empty, Marker, Marker+Name");
        assert_eq!(
            info.components,
            [
                core::any::type_name::<Marker>(),
                core::any::type_name::<Name>(),
            ],
            "sorted and deduplicated",
        );
    }

    #[test]
    fn every_archetype_reports_its_id_length_and_columns() {
        let mut world = World::new();
        world.spawn((Marker(1), Name("a")));
        let infos = world.archetype_infos();
        assert_eq!(infos.len(), world.archetype_count());
        assert_eq!(infos[0].index, 0, "the empty archetype comes first");
        assert!(infos[0].components.is_empty());
        assert_eq!(infos[1].index, 1);
        assert_eq!(infos[1].len, 1);
        // The columns are ordered by TypeId, not by how the bundle was
        // written, so compare as a set.
        let mut names = infos[1].components.clone();
        names.sort_unstable();
        let mut expected = [
            core::any::type_name::<Marker>(),
            core::any::type_name::<Name>(),
        ];
        expected.sort_unstable();
        assert_eq!(names, expected);
    }

    #[test]
    fn an_entity_reports_its_archetype_row_and_components() {
        let mut world = World::new();
        let first = world.spawn((Marker(1), Name("a")));
        let second = world.spawn((Marker(2),));
        let info = world.entity_info(first).unwrap();
        assert_eq!(info.entity, first);
        assert_eq!(info.archetype, 1);
        assert_eq!(info.row, 0);
        assert_eq!(info.components.len(), 2);
        assert_eq!(world.entity_info(second).unwrap().archetype, 2);
        assert!(world.entity_info(Entity::from_raw(0, 99)).is_none());
    }

    #[test]
    fn a_despawned_entity_has_no_info() {
        let mut world = World::new();
        let entity = world.spawn((Marker(1),));
        assert!(world.entity_info(entity).is_some());
        world.despawn(entity);
        assert!(world.entity_info(entity).is_none());
    }

    #[test]
    fn entity_infos_stops_at_the_limit_in_storage_order() {
        let mut world = World::new();
        let first = world.spawn((Marker(1),));
        let second = world.spawn((Marker(2), Name("b")));
        let infos = world.entity_infos(1);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].entity, first);
        let all = world.entity_infos(usize::MAX);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].entity, first);
        assert_eq!(all[1].entity, second);
    }
}
