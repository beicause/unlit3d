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

use core::ops::Deref;

use crate::component::AddableComponent;
use crate::entity::Entity;
use crate::mode::{CellRef, Mode};
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

    /// Take the list of children, leaving the component empty.
    pub(crate) fn take(&mut self) -> Vec<Entity> {
        core::mem::take(&mut self.0)
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
    #[must_use]
    pub fn parent(&self, entity: Entity) -> Option<Entity> {
        self.get::<ChildOf>(entity).map(|child_of| child_of.0)
    }

    /// Borrow the children of `entity`, in order.
    ///
    /// An entity without children borrows an empty slice. The borrow is held for
    /// as long as the returned view is alive, so this does not copy the list.
    pub fn children(&self, entity: Entity) -> ChildrenView<'_, M> {
        ChildrenView {
            children: self.get::<Children>(entity),
            index: 0,
        }
    }

    /// The number of children of `entity`.
    #[must_use]
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
    /// assert_eq!(world.children(parent).collect::<Vec<_>>(), [child]);
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
    ///
    /// `parent` must be the child's actual parent. The `ChildOf` component is
    /// the source of truth: when `parent` does not match it, nothing happens,
    /// so a caller that names the wrong parent cannot leave a stale entry in
    /// the real parent's [`Children`].
    pub fn remove_child(&mut self, parent: Entity, child: Entity) {
        if self.parent(child) != Some(parent) {
            return;
        }
        if let Some(mut children) = self.get_mut::<Children>(parent) {
            children.remove(child);
        }
        let _ = self.remove_erased(child, core::any::TypeId::of::<ChildOf>());
    }

    /// Detach every child of `entity`.
    pub fn clear_children(&mut self, entity: Entity) {
        // Move the list out first: detaching would otherwise invalidate a
        // borrow of it, and this avoids copying it.
        if let Some(children) = self.with_mut::<Children, _>(entity, Children::take) {
            for child in children {
                let _ = self.remove_erased(child, core::any::TypeId::of::<ChildOf>());
            }
        }
    }

    /// Whether `entity` is `other` or a descendant of it.
    #[must_use]
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
    #[must_use]
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
    ///
    /// The entity itself is not yielded; `ancestors` walks the other way.
    pub fn descendants(&self, entity: Entity) -> impl Iterator<Item = Entity> + '_ {
        // One open view per level, so descending costs a pop rather than a
        // collect per entity.
        let mut stack: Vec<ChildrenView<'_, M>> = vec![self.children(entity)];
        core::iter::from_fn(move || {
            while let Some(open) = stack.last_mut() {
                if let Some(child) = open.next() {
                    stack.push(self.children(child));
                    return Some(child);
                }
                stack.pop();
            }
            None
        })
    }
}

/// A borrowed view of one entity's children.
///
/// The view is both a slice (through [`Deref`]) and an iterator, and it holds
/// the component borrow for its whole lifetime, so it neither copies the list
/// nor looks it up again per child.
pub struct ChildrenView<'w, M: Mode> {
    children: Option<CellRef<'w, M, Children>>,
    index: usize,
}

impl<M: Mode> Deref for ChildrenView<'_, M> {
    type Target = [Entity];

    fn deref(&self) -> &[Entity] {
        self.children.as_deref().map_or(&[], Children::as_slice)
    }
}

impl<M: Mode> Iterator for ChildrenView<'_, M> {
    type Item = Entity;

    fn next(&mut self) -> Option<Self::Item> {
        let child = self.deref().get(self.index).copied();
        self.index += 1;
        child
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
        assert_eq!(world.children(parent).collect::<Vec<_>>(), [child]);
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
        assert_eq!(world.children(parent).collect::<Vec<_>>(), [a, b, c]);
    }

    #[test]
    fn reparenting_moves_the_child_between_lists() {
        let mut world = crate::LocalWorld::new();
        let first = world.spawn(("first",));
        let second = world.spawn(("second",));
        let child = world.spawn(("leaf",));
        world.set_parent(child, first);
        world.set_parent(child, second);
        assert!(world.children(first).next().is_none());
        assert_eq!(world.children(second).collect::<Vec<_>>(), [child]);
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
        assert!(world.children(parent).next().is_none());
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
        assert!(world.children(parent).next().is_none());
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
        assert_eq!(world.children(parent).collect::<Vec<_>>(), [child]);
        world.remove::<crate::tests_common::Addable>(child).unwrap();
        assert_eq!(world.parent(child), Some(parent));
    }

    #[test]
    fn remove_child_with_the_wrong_parent_does_nothing() {
        let mut world = crate::LocalWorld::new();
        let real = world.spawn(("real",));
        let wrong = world.spawn(("wrong",));
        let child = world.spawn(("child",));
        world.set_parent(child, real);
        world.remove_child(wrong, child);
        assert_eq!(
            world.parent(child),
            Some(real),
            "the real parent still owns it"
        );
        assert_eq!(world.children(real).collect::<Vec<_>>(), [child]);
    }

    #[test]
    fn a_failed_remove_child_cannot_make_despawn_reach_a_reparented_child() {
        let mut world = crate::LocalWorld::new();
        let first = world.spawn(("first",));
        let wrong = world.spawn(("wrong",));
        let second = world.spawn(("second",));
        let child = world.spawn(("child",));
        world.set_parent(child, first);
        world.remove_child(wrong, child);
        world.set_parent(child, second);
        assert!(world.despawn(first));
        assert!(world.contains(child), "the child belongs to `second`");
        assert_eq!(world.parent(child), Some(second));
    }

    #[test]
    fn a_deep_chain_despawns_without_recursing() {
        // Built through the hierarchy components directly, so the test does not
        // pay `set_parent`'s per-call ancestor walk. The chain is deep enough
        // that a recursive despawn would overflow the thread stack.
        use crate::{ChildOf, Children};

        let mut world = crate::LocalWorld::new();
        let root = world.spawn(("root",));
        let mut parent = root;
        for _ in 0..50_000 {
            let child = world.spawn(("node",));
            world.insert(child, ChildOf(parent)).unwrap();
            if world.has::<Children>(parent) {
                let _ = world.with_mut::<Children, _>(parent, |c| c.push(child));
            } else {
                world.insert(parent, Children::from_iter([child])).unwrap();
            }
            parent = child;
        }
        assert!(world.despawn(root));
        assert!(world.is_empty());
    }
}
