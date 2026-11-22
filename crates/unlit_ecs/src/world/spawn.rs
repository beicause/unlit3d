//! Putting a bundle of components on a row, which is how an entity comes to be.
//!
//! A spawn allocates the handle, registers a column for every component type
//! the world has not seen before, finds or creates the archetype with exactly
//! those types, and appends the row to it. The spawn callbacks run afterwards,
//! from [`World::finish_spawn`].
//!
//! A bundle that declares a singleton key is checked against the keys held
//! before its handle is allocated, and claims the key once its row is in.

use core::any::TypeId;

use crate::archetype::Archetype;
use crate::bundle::{Bundle, BundleComponent, Components, DynamicBundle, SingletonKey};
use crate::column::AnyColumn;
use crate::entity::{Entity, Location};
use crate::error::EntityAccessError;

use super::World;

impl World {
    /// Spawn `bundle` in `scope`.
    ///
    /// The entity is named after the bundle: a type of the caller's own names it
    /// after itself, and a `bundle!` names it after the source position that
    /// wrote the bundle. A dynamic bundle made by hand carries no name, so an
    /// entity spawned from one has none.
    pub(crate) fn spawn_in<B: Bundle>(&mut self, scope: Entity, bundle: B) -> Entity {
        let name = bundle.name();
        self.spawn_bundle_in(scope, name, bundle.into_bundle())
    }

    /// Spawn a bundle under `name`, whatever name the bundle carries.
    pub(crate) fn spawn_named_in<B: Bundle>(
        &mut self,
        scope: Entity,
        name: &'static str,
        bundle: B,
    ) -> Entity {
        self.spawn_bundle_in(scope, name, bundle.into_bundle())
    }

    fn spawn_bundle_in(
        &mut self,
        scope: Entity,
        name: &'static str,
        bundle: DynamicBundle,
    ) -> Entity {
        self.assert_spawnable(scope);
        let singleton = bundle.singleton_key;
        self.assert_singleton_free(singleton);
        // Sorted before the handle is allocated: a bundle that names one
        // component twice panics, and the entity allocated for it would then be
        // neither spawned nor freed.
        let components = bundle.finish();
        let entity = self.entities_write().alloc(name);
        self.insert_row(entity, scope, components);
        self.claim_singleton(entity, singleton);
        self.finish_spawn(entity);
        entity
    }

    /// Spawn `bundle` on `entity`, a handle from [`World::reserve_entity`], in
    /// `scope`.
    ///
    /// Answers [`EntityAccessError::NotReserved`] when the handle is not a live
    /// reservation from [`World::reserve_entity`]: spawning on a stale or
    /// never-reserved handle would leave a row nobody can reach or free.
    pub(crate) fn spawn_at_in<B: Bundle>(
        &mut self,
        entity: Entity,
        scope: Entity,
        bundle: B,
    ) -> Result<(), EntityAccessError> {
        // Checked before the reservation is taken, so a scope that is already
        // gone leaves the handle reserved for a later attempt rather than
        // spending it on a row with nowhere to live.
        self.check_spawnable(scope)?;
        let bundle = bundle.into_bundle();
        // Checked before the reservation is taken, as the scope is above, so a
        // refused key leaves the handle reserved for a later attempt.
        let singleton = bundle.singleton_key;
        self.check_singleton_free(singleton)?;
        // Sorted before the reservation is taken, so a rejected bundle leaves
        // the handle reserved for a later attempt rather than leaking it.
        let components = bundle.finish();
        if !self.entities_write().take_reserved(entity) {
            return Err(EntityAccessError::NotReserved { entity });
        }
        // The handle was named when it was reserved, which is where the caller
        // said what the entity is; the bundle does not rename it.
        self.insert_row(entity, scope, components);
        self.claim_singleton(entity, singleton);
        self.finish_spawn(entity);
        Ok(())
    }

    /// Panic when `scope` is not a place a new row can land.
    ///
    /// A scope that has already been removed is not: its own row is gone, so
    /// a row attached under it would have no link from the world to reach it,
    /// which is an entity that exists and is nowhere. A caller that only has
    /// this to answer with panics, the way [`World::scope_mut`] does; the
    /// queued form answers [`EntityAccessError::NotAlive`] instead.
    #[track_caller]
    fn assert_spawnable(&self, scope: Entity) {
        if let Err(error) = self.check_spawnable(scope) {
            panic!("{error}");
        }
    }

    /// The reason `scope` is not a place a new row can land.
    fn check_spawnable(&self, scope: Entity) -> Result<(), EntityAccessError> {
        self.tree.parent(scope)?;
        Ok(())
    }

    /// Panic when a live entity already holds `singleton`'s key.
    #[track_caller]
    fn assert_singleton_free(&self, singleton: Option<SingletonKey>) {
        if let Err(error) = self.check_singleton_free(singleton) {
            panic!("{error}");
        }
    }

    /// Refuse a bundle whose singleton key a live entity already holds.
    fn check_singleton_free(
        &self,
        singleton: Option<SingletonKey>,
    ) -> Result<(), EntityAccessError> {
        let Some(key) = singleton else {
            return Ok(());
        };
        if self.singleton_holder(key.type_id).is_some() {
            return Err(EntityAccessError::SingletonTaken { key: key.name });
        }
        Ok(())
    }

    /// Record `entity` as the holder of `singleton`'s key, once its row is in.
    ///
    /// Only called after [`World::check_singleton_free`] has passed for the same
    /// bundle, and before any callback runs, so nothing can claim the key in
    /// between.
    pub(super) fn claim_singleton(&mut self, entity: Entity, singleton: Option<SingletonKey>) {
        let Some(key) = singleton else {
            return;
        };
        let previous = self.singleton_holders.insert(key.type_id, entity);
        debug_assert!(
            previous.is_none(),
            "the key was checked free before the row went in",
        );
        self.singleton_keys.insert(entity, key.type_id);
    }

    /// Insert a row for `entity` in `scope`, made of `components`.
    pub(super) fn insert_row(&mut self, entity: Entity, scope: Entity, components: Components) {
        for component in &components {
            self.ctors
                .entry(component.type_id)
                .or_insert(component.column);
            self.names
                .entry(component.type_id)
                .or_insert(component.name);
        }

        let archetype = self.archetype_for_components(&components);
        let target = self
            .archetypes
            .get_mut(archetype)
            .expect("the archetype was just found or registered");
        for component in components {
            target.push_erased(component.type_id, component.value);
        }
        target.push_entity(entity, scope);
        let row = (target.len() - 1) as u32;
        self.entities_write()
            .set_location(entity, Location { archetype, row });
        self.tree_mut().attach(entity, scope);
    }

    /// The archetype with exactly the types of `components`, creating it when
    /// it is new. Every type must have a registered column constructor.
    ///
    /// A hit allocates nothing: the types are hashed as the components are
    /// walked and compared in place, so the type sequence is only materialised
    /// when a new archetype has to be registered.
    fn archetype_for_components(&mut self, components: &[BundleComponent]) -> u32 {
        let found = self.archetypes.find_with(
            components.iter().map(|component| component.type_id),
            |stored| {
                stored.len() == components.len()
                    && stored
                        .iter()
                        .zip(components)
                        .all(|(type_id, component)| *type_id == component.type_id)
            },
        );
        if let Some(id) = found {
            return id;
        }
        let types: Box<[TypeId]> = components
            .iter()
            .map(|component| component.type_id)
            .collect();
        let names: Box<[&'static str]> =
            components.iter().map(|component| component.name).collect();
        let columns: Box<[Box<dyn AnyColumn>]> = types
            .iter()
            .map(|type_id| {
                let ctor = self.ctors.get(type_id).unwrap_or_else(|| {
                    panic!(
                        "no column constructor is registered for `{}`",
                        self.type_name(*type_id).unwrap_or("an unnamed type"),
                    )
                });
                ctor()
            })
            .collect();
        self.archetypes
            .register(Archetype::new(types, names, columns))
    }
}
