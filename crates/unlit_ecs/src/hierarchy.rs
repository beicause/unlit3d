//! The entity hierarchy.
//!
//! The hierarchy is two ordinary components:
//!
//! - [`ChildOf`] on a child points at its parent. An entity has at most one,
//!   so a child has one parent.
//! - [`Children`] on a parent lists its children in order.
//!
//! Both are [`AddableComponent`]s: unlike an ordinary component they may be
//! added and removed while the world runs, because the hierarchy changes. They
//! are kept consistent by [`World::set_parent`], [`World::remove_child`] and
//! [`World::clear_children`], which are the only supported way to change the
//! hierarchy — do not spawn a non-empty [`Children`] or a [`ChildOf`].

use crate::component::AddableComponent;
use crate::entity::Entity;
use crate::mode::Mode;
use crate::world::World;

/// The children of an entity, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Children(Vec<Entity>);

impl Children {
    /// No children.
    pub fn new() -> Self {
        Self::default()
    }

    /// The children, in order.
    pub fn as_slice(&self) -> &[Entity] {
        &self.0
    }

    /// Iterate the children.
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no children.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `child` is one of the children.
    pub fn contains(&self, child: Entity) -> bool {
        self.0.contains(&child)
    }

    pub(crate) fn push(&mut self, child: Entity) {
        if !self.contains(child) {
            self.0.push(child);
        }
    }

    pub(crate) fn remove(&mut self, child: Entity) {
        self.0.retain(|other| *other != child);
    }

    pub(crate) fn remove_all(&mut self) {
        self.0.clear();
    }
}

impl<'a> IntoIterator for &'a Children {
    type Item = Entity;
    type IntoIter = core::iter::Copied<core::slice::Iter<'a, Entity>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().copied()
    }
}

impl FromIterator<Entity> for Children {
    fn from_iter<T: IntoIterator<Item = Entity>>(iter: T) -> Self {
        let mut children = Self::new();
        for child in iter {
            children.push(child);
        }
        children
    }
}

impl AddableComponent for Children {}

/// The parent of an entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildOf(pub Entity);

impl ChildOf {
    /// The parent entity.
    pub fn parent(self) -> Entity {
        self.0
    }
}

impl AddableComponent for ChildOf {}

impl<M: Mode> World<M> {
    /// The parent of `entity`, if it has one.
    pub fn parent(&self, entity: Entity) -> Option<Entity> {
        self.get::<ChildOf>(entity).map(|child_of| child_of.0)
    }

    /// The children of `entity`, in order.
    pub fn child_entities(&self, entity: Entity) -> Vec<Entity> {
        self.get::<Children>(entity)
            .map(|children| children.as_slice().to_vec())
            .unwrap_or_default()
    }

    /// The number of children of `entity`.
    pub fn child_count(&self, entity: Entity) -> usize {
        self.get::<Children>(entity)
            .map(|children| children.len())
            .unwrap_or(0)
    }

    /// Make `child` a child of `parent`, detaching it from its old parent.
    ///
    /// Setting a parent that would create a cycle does nothing. Reparenting to
    /// the same parent moves the child to the end of the list.
    ///
    /// `````
    /// # use unlit_ecs::LocalWorld;
    /// let mut world = LocalWorld::new();
    /// let parent = world.spawn(("root",));
    /// let child = world.spawn(("leaf",));
    /// world.set_parent(child, parent);
    /// assert_eq!(world.parent(child), Some(parent));
    /// assert_eq!(world.child_entities(parent), [child]);
    /// `````
    pub fn set_parent(&mut self, child: Entity, parent: Entity) {
        if child == parent {
            return;
        }
        self.expect_alive(child);
        self.expect_alive(parent);
        if self.is_ancestor_of(child, parent) {
            return;
        }
        if let Some(old_parent) = self.parent(child)
            && let Some(mut children) = self.get_mut::<Children>(old_parent)
        {
            children.remove(child);
        }
        let already_has_parent = self.has::<ChildOf>(child);
        if already_has_parent {
            if let Some(mut child_of) = self.get_mut::<ChildOf>(child) {
                child_of.0 = parent;
            }
        } else {
            let _ = self.insert_erased(
                child,
                core::any::TypeId::of::<ChildOf>(),
                Box::new(ChildOf(parent)),
            );
        }
        let already_has_children = self.has::<Children>(parent);
        if already_has_children {
            if let Some(mut children) = self.get_mut::<Children>(parent) {
                children.push(child);
            }
        } else {
            let _ = self.insert_erased(
                parent,
                core::any::TypeId::of::<Children>(),
                Box::new(Children::from_iter([child])),
            );
        }
    }

    /// Detach `child` from `parent`.
    pub fn remove_child(&mut self, parent: Entity, child: Entity) {
        if let Some(mut children) = self.get_mut::<Children>(parent) {
            children.remove(child);
        }
        let _ = self.remove_erased(child, core::any::TypeId::of::<ChildOf>());
    }

    /// Detach every child of `entity`.
    pub fn clear_children(&mut self, entity: Entity) {
        for child in self.child_entities(entity) {
            let _ = self.remove_erased(child, core::any::TypeId::of::<ChildOf>());
        }
        if let Some(mut children) = self.get_mut::<Children>(entity) {
            children.remove_all();
        }
    }

    /// Whether `entity` is `other` or a descendant of it.
    pub fn is_descendant_of(&self, entity: Entity, other: Entity) -> bool {
        let mut current = Some(entity);
        while let Some(candidate) = current {
            if candidate == other {
                return true;
            }
            current = self.parent(candidate);
        }
        false
    }

    /// Whether `entity` is `other` or an ancestor of it.
    pub fn is_ancestor_of(&self, entity: Entity, other: Entity) -> bool {
        self.is_descendant_of(other, entity)
    }

    /// Iterate the ancestors of `entity`, nearest first.
    pub fn ancestors(&self, entity: Entity) -> impl Iterator<Item = Entity> + '_ {
        let mut current = self.parent(entity);
        core::iter::from_fn(move || {
            let next = current;
            current = next.and_then(|entity| self.parent(entity));
            next
        })
    }

    /// Iterate the descendants of `entity`, depth first and in child order.
    pub fn descendants(&self, entity: Entity) -> impl Iterator<Item = Entity> + '_ {
        let mut stack: Vec<Entity> = self.child_entities(entity);
        stack.reverse();
        core::iter::from_fn(move || {
            let next = stack.pop()?;
            let mut children = self.child_entities(next);
            children.reverse();
            stack.extend(children);
            Some(next)
        })
    }
}
#[cfg(test)]
mod tests {
    use crate::tests_common::Marker;

    #[test]
    fn set_parent_updates_both_sides() {
        let mut world = crate::LocalWorld::new();
        let parent = world.spawn(("root",));
        let child = world.spawn(("leaf",));
        world.set_parent(child, parent);
        assert_eq!(world.parent(child), Some(parent));
        assert_eq!(world.child_entities(parent), [child]);
        assert_eq!(world.child_count(parent), 1);
    }

    #[test]
    fn children_keep_their_order() {
        let mut world = crate::LocalWorld::new();
        let parent = world.spawn(("root",));
        let a = world.spawn(("a",));
        let b = world.spawn(("b",));
        let c = world.spawn(("c",));
        for child in [a, b, c] {
            world.set_parent(child, parent);
        }
        assert_eq!(world.child_entities(parent), [a, b, c]);
    }

    #[test]
    fn reparenting_moves_the_child_between_lists() {
        let mut world = crate::LocalWorld::new();
        let first = world.spawn(("first",));
        let second = world.spawn(("second",));
        let child = world.spawn(("leaf",));
        world.set_parent(child, first);
        world.set_parent(child, second);
        assert!(world.child_entities(first).is_empty());
        assert_eq!(world.child_entities(second), [child]);
        assert_eq!(world.parent(child), Some(second));
    }

    #[test]
    fn a_cycle_is_refused() {
        let mut world = crate::LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("leaf",));
        world.set_parent(child, root);
        // Making the root a child of its own descendant would loop.
        world.set_parent(root, child);
        assert_eq!(world.parent(root), None);
        assert_eq!(world.parent(child), Some(root));
    }

    #[test]
    fn remove_child_detaches_both_sides() {
        let mut world = crate::LocalWorld::new();
        let parent = world.spawn(("root",));
        let child = world.spawn(("leaf",));
        world.set_parent(child, parent);
        world.remove_child(parent, child);
        assert!(world.child_entities(parent).is_empty());
        assert_eq!(world.parent(child), None);
    }

    #[test]
    fn clear_children_detaches_every_child() {
        let mut world = crate::LocalWorld::new();
        let parent = world.spawn(("root",));
        let a = world.spawn(("a",));
        let b = world.spawn(("b",));
        world.set_parent(a, parent);
        world.set_parent(b, parent);
        world.clear_children(parent);
        assert!(world.child_entities(parent).is_empty());
        assert_eq!(world.parent(a), None);
        assert_eq!(world.parent(b), None);
    }

    #[test]
    fn despawn_cascades_to_descendants() {
        let mut world = crate::LocalWorld::new();
        let root = world.spawn(("root",));
        let middle = world.spawn(("middle",));
        let leaf = world.spawn(("leaf",));
        world.set_parent(middle, root);
        world.set_parent(leaf, middle);
        assert!(world.despawn(root));
        assert!(!world.contains(root));
        assert!(!world.contains(middle));
        assert!(!world.contains(leaf));
        assert_eq!(world.len(), 0);
    }

    #[test]
    fn ancestors_and_descendants_walk_the_tree() {
        let mut world = crate::LocalWorld::new();
        let root = world.spawn(("root",));
        let middle = world.spawn(("middle",));
        let leaf = world.spawn(("leaf",));
        world.set_parent(middle, root);
        world.set_parent(leaf, middle);

        assert_eq!(world.ancestors(leaf).collect::<Vec<_>>(), [middle, root]);
        assert_eq!(world.descendants(root).collect::<Vec<_>>(), [middle, leaf]);
        assert!(world.is_descendant_of(leaf, root));
        assert!(world.is_ancestor_of(root, leaf));
        assert!(!world.is_descendant_of(root, leaf));
    }

    #[test]
    fn hierarchy_components_survive_component_moves() {
        let mut world = crate::LocalWorld::new();
        let parent = world.spawn(("root",));
        let child = world.spawn((Marker(1),));
        world.set_parent(child, parent);
        // Adding an addable component moves the child between archetypes; the
        // hierarchy must move with it.
        world
            .insert(child, crate::tests_common::Addable(2))
            .unwrap();
        assert_eq!(world.parent(child), Some(parent));
        assert_eq!(world.child_entities(parent), [child]);
        world.remove::<crate::tests_common::Addable>(child).unwrap();
        assert_eq!(world.parent(child), Some(parent));
    }
}
