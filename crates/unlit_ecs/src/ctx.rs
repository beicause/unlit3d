//! Scopes for invoking behaviour components.
//!
//! A [`Ctx`] is the world as one entity is allowed to see it: the entity
//! itself, its descendants, and resource entities. Anything else is out of
//! reach, so an entity cannot read or write its parent, its siblings, or an
//! unrelated entity — an entity's state is its own, as an object's would be.
//!
//! `````
//! # use unlit_ecs::{LocalWorld, Resource};
//! let mut world = LocalWorld::new();
//! let root = world.spawn(("root",));
//! let child = world.spawn(("child",));
//! world.set_parent(child, root);
//!
//! let ctx = world.ctx(root);
//! assert!(ctx.can_access(child));
//! assert!(!world.ctx(child).can_access(root));
//! `````

use crate::command::Commands;
use crate::component::Resource;
use crate::entity::Entity;
use crate::mode::{CellRef, CellRefMut, Mode};
use crate::world::World;

/// One entity's view of the world.
///
/// The view is a plain value: it can be copied, stored and passed on, and each
/// copy keeps the same isolation.
pub struct Ctx<'w, M: Mode> {
    world: &'w World<M>,
    entity: Entity,
}

impl<'w, M: Mode> Ctx<'w, M> {
    pub(crate) fn new(world: &'w World<M>, entity: Entity) -> Self {
        Self { world, entity }
    }

    /// The entity this scope belongs to.
    pub fn entity(&self) -> Entity {
        self.entity
    }

    /// The whole world.
    ///
    /// This is an escape hatch: it hands out a `&World`, so a caller can read
    /// or write any entity, not just the ones this scope is allowed to reach.
    /// [`Ctx::get_in`], [`Ctx::get_mut_in`] and [`Ctx::children`] are the
    /// isolated forms; reach for `world` only for what they cannot express,
    /// such as a query over the whole world.
    pub fn world(&self) -> &'w World<M> {
        self.world
    }

    /// Whether this scope may reach `other`: itself, one of its descendants,
    /// or a resource entity.
    pub fn can_access(&self, other: Entity) -> bool {
        other == self.entity
            || self.world.is_descendant_of(other, self.entity)
            || self.world.has::<Resource>(other)
    }

    /// The component of this scope's entity.
    ///
    /// The entity is always in scope, so this only returns `None` when the
    /// entity does not have the component.
    pub fn get<C: 'static>(&self) -> Option<CellRef<'w, M, C>> {
        self.world.get::<C>(self.entity)
    }

    /// The component of this scope's entity, exclusively.
    pub fn get_mut<C: 'static>(&self) -> Option<CellRefMut<'w, M, C>> {
        self.world.get_mut::<C>(self.entity)
    }

    /// Run `f` on the component of this scope's entity.
    pub fn with_mut<C: 'static, R>(&self, f: impl FnOnce(&mut C) -> R) -> Option<R> {
        self.world.with_mut::<C, R>(self.entity, f)
    }

    /// The component of another entity, if it is in scope.
    ///
    /// Returns `None` for an entity that is out of scope, just as it does for
    /// a component the entity does not have.
    pub fn get_in<C: 'static>(&self, other: Entity) -> Option<CellRef<'w, M, C>> {
        self.can_access(other).then(|| self.world.get::<C>(other))?
    }

    /// The component of another entity, exclusively, if it is in scope.
    pub fn get_mut_in<C: 'static>(&self, other: Entity) -> Option<CellRefMut<'w, M, C>> {
        self.can_access(other)
            .then(|| self.world.get_mut::<C>(other))?
    }

    /// Iterate the children of this scope's entity as scopes of their own.
    pub fn children(&self) -> impl Iterator<Item = Ctx<'w, M>> + 'w {
        let world: &'w World<M> = self.world;
        world
            .children(self.entity)
            .map(move |child| Ctx::new(world, child))
    }

    /// A queue of structural changes to apply after the callback returns.
    ///
    /// A behaviour component only ever has a shared world, so this is how it
    /// spawns, despawns or reparents: queue the change here, then let the
    /// driver call [`World::apply`].
    pub fn commands(&self) -> Commands<'w, M> {
        self.world.queue()
    }
}
#[cfg(test)]
mod tests {
    use crate::tests_common::Marker;
    use crate::{LocalWorld, Resource};

    #[test]
    fn a_scope_reaches_itself_and_descendants() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("child",));
        let grandchild = world.spawn(("grandchild",));
        world.set_parent(child, root);
        world.set_parent(grandchild, child);

        let ctx = world.ctx(root);
        assert!(ctx.can_access(root));
        assert!(ctx.can_access(child));
        assert!(ctx.can_access(grandchild));
    }

    #[test]
    fn a_scope_cannot_reach_its_parent_or_siblings() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("child",));
        let sibling = world.spawn(("sibling",));
        let unrelated = world.spawn(("unrelated",));
        world.set_parent(child, root);
        world.set_parent(sibling, root);

        let ctx = world.ctx(child);
        assert!(!ctx.can_access(root), "the parent is out of reach");
        assert!(!ctx.can_access(sibling), "a sibling is out of reach");
        assert!(
            !ctx.can_access(unrelated),
            "an unrelated entity is out of reach"
        );
    }

    #[test]
    fn a_scope_reaches_a_resource() {
        let mut world = LocalWorld::new();
        let child = world.spawn(("child",));
        let settings = world.spawn((Resource, 60u32));
        assert!(world.ctx(child).can_access(settings));
    }

    #[test]
    fn an_out_of_scope_lookup_returns_none() {
        let mut world = LocalWorld::new();
        let parent = world.spawn((Marker(1),));
        let child = world.spawn((Marker(2),));
        world.set_parent(child, parent);
        let ctx = world.ctx(child);
        assert!(ctx.get_in::<Marker>(child).is_some());
        assert!(
            ctx.get_in::<Marker>(parent).is_none(),
            "the parent is out of scope"
        );
    }

    #[test]
    fn a_scope_reads_and_writes_its_own_components() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        let ctx = world.ctx(entity);
        assert_eq!(ctx.get::<Marker>().unwrap().0, 1);
        ctx.with_mut::<Marker, _>(|marker| marker.0 = 5).unwrap();
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 5);
    }

    #[test]
    fn children_are_scopes_with_the_same_root() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("child",));
        world.set_parent(child, root);
        let ctx = world.ctx(root);
        let children: Vec<_> = ctx.children().collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].entity(), child);
    }

    #[test]
    fn walking_the_descendants_visits_each_in_order() {
        use crate::Ctx;
        let mut world = LocalWorld::new();
        let root = world.spawn((Marker(1),));
        let a = world.spawn((Marker(2),));
        let b = world.spawn((Marker(3),));
        let leaf = world.spawn((Marker(4),));
        world.set_parent(a, root);
        world.set_parent(b, root);
        world.set_parent(leaf, a);

        let mut visited = Vec::new();
        for entity in world.descendants(root) {
            world.call::<Marker, _>(entity, |marker, _ctx: Ctx<'_, _>| visited.push(marker.0));
        }
        assert_eq!(visited, [2, 4, 3], "depth first, children in order");
    }

    #[test]
    fn call_runs_one_entity_only() {
        use crate::Ctx;
        let mut world = LocalWorld::new();
        let root = world.spawn((Marker(1),));
        let child = world.spawn((Marker(2),));
        world.set_parent(child, root);

        // Direction-free: no tree walk, just the named entity.
        let result = world.call::<Marker, _>(child, |marker, _ctx: Ctx<'_, _>| marker.0);
        assert_eq!(result, Some(2));
        assert_eq!(
            world.get::<Marker>(root).unwrap().0,
            1,
            "the root was not called"
        );
    }

    #[test]
    fn call_returns_none_without_the_component() {
        use crate::Ctx;
        let mut world = LocalWorld::new();
        let entity = world.spawn(("no marker",));
        let result = world.call::<Marker, _>(entity, |_marker, _ctx: Ctx<'_, _>| ());
        assert!(result.is_none());
    }

    #[test]
    fn walking_the_descendants_skips_entities_without_the_component() {
        use crate::Ctx;
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn((Marker(2),));
        world.set_parent(child, root);
        let mut count = 0;
        for entity in world.descendants(root) {
            world.call::<Marker, _>(entity, |_marker, _ctx: Ctx<'_, _>| count += 1);
        }
        assert_eq!(count, 1);
    }
}
