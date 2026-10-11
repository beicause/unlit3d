//! Host helpers: the sub-world that shares the host's thread.

use unlit_ecs::{Entity, World};

/// Spawn a sub-world that shares the host's thread, nested as a `World`
/// component of the returned entity.
///
/// Sub-worlds are for work that must stay on the host thread — rendering
/// into a window's swap chain, say — but whose state should not mix with
/// the host's. A frame reaches it with `World::get_mut` on the returned
/// entity and drives it itself: dispatch its frame behaviours, then present.
pub fn spawn_sub_world(host: &mut World) -> Entity {
    host.spawn((World::new(),))
}
