//! Running `OnSpawn` and `OnDespawn` callbacks out of the rows that hold them.
//!
//! The callback is a value in the row's column, not a function pointer known to
//! the world, so running one means moving it out of the cell first: a callback
//! that reaches for its own lifecycle component would otherwise find the cell
//! held open by its own run. The guard puts it back unless the callback left a
//! replacement of its own.

use crate::entity::Entity;
use crate::lifecycle::{Lifecycle, LifecycleCallback};
use crate::view::ScopeMut;

use super::World;

impl World {
    /// Run the spawn callbacks of an entity whose row is already in place.
    pub(super) fn finish_spawn(&mut self, entity: Entity) {
        self.run_lifecycle::<crate::OnSpawn>(entity);
    }

    /// Run the callback of the lifecycle component `C` the entity carries.
    ///
    /// The scope handed to it is the one the entity lives in, that of its
    /// parent: a callback that wants the entity's own subtree descends into the
    /// entity itself.
    pub(super) fn run_lifecycle<C: 'static + Lifecycle>(&mut self, entity: Entity) {
        let Ok(scope) = self.parent_scope(entity) else {
            return;
        };
        // The callback is moved out of the row for the run, so a callback that
        // reaches for its own lifecycle component reads it instead of finding
        // it held open by its own run.
        let Some(callback) = self.take_lifecycle::<C>(entity) else {
            return;
        };
        // `Held` puts the callback back on the way out, including when the
        // callback panics: the row holds a no-op placeholder meanwhile, and an
        // entity that lost its callback for good would never run one again.
        struct Held {
            world: *mut World,
            entity: Entity,
            restore: fn(&mut World, Entity, LifecycleCallback),
            callback: Option<LifecycleCallback>,
        }

        impl Drop for Held {
            fn drop(&mut self) {
                let Some(callback) = self.callback.take() else {
                    return;
                };
                // SAFETY: the guard is built from the `&mut World` the run was
                // handed and holds it until it is dropped here.
                let world = unsafe { &mut *self.world };
                // A callback that despawned its own entity leaves no row to put
                // the callback back in; the row is gone with it.
                if world.location(self.entity).is_ok() {
                    (self.restore)(world, self.entity, callback);
                }
            }
        }

        let mut held = Held {
            world: self,
            entity,
            restore: World::restore_lifecycle::<C>,
            callback: Some(callback),
        };
        // SAFETY: `held` holds the world exclusively as a raw pointer for
        // exactly this borrow, which ends before the guard is dropped.
        let world = unsafe { &mut *held.world };
        let mut scope = ScopeMut::new(world, scope);
        let callback = held
            .callback
            .as_mut()
            .expect("the guard holds the callback until it is dropped");
        callback(&mut scope, entity);
    }

    /// Move the callback of the lifecycle component `C` out of the entity's row.
    ///
    /// `None` means there is nothing to run: the archetype has no such column,
    /// the handle is dead, or the row carries no callback.
    pub(super) fn take_lifecycle<C: 'static + Lifecycle>(
        &mut self,
        entity: Entity,
    ) -> Option<LifecycleCallback> {
        let location = self.location(entity).ok()?;
        let archetype = self.archetypes.get_mut(location.archetype()).ok()?;
        let column = archetype.column_mut::<C>()?;
        let cell = column.cells_mut().get_mut(location.row())?;
        // SAFETY: `&mut World` excludes every query and every other reference to
        // the world's components, so nothing else can be looking at this row.
        unsafe { &mut *cell.get() }.take()
    }

    /// Put back a callback moved out by the world's own run of it, unless the
    /// callback left a replacement of its own on the row.
    fn restore_lifecycle<C: 'static + Lifecycle>(
        &mut self,
        entity: Entity,
        callback: LifecycleCallback,
    ) {
        let location = self
            .location(entity)
            .expect("a lifecycle callback was moved out of a live entity's row");
        let archetype = self
            .archetypes
            .get_mut(location.archetype())
            .expect("a live entity's location names a registered archetype");
        let column = archetype
            .column_mut::<C>()
            .expect("the column the callback came from is still registered");
        let cell = column
            .cells_mut()
            .get_mut(location.row())
            .expect("the row still exists");
        // SAFETY: as above, `&mut World` excludes every other reference.
        unsafe { &mut *cell.get() }.restore(callback)
    }
}
