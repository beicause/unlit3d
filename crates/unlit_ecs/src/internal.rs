//! The marker that tells a world's own rows from its content.
//!
//! A world keeps a couple of entities for itself: the root scope, which owns
//! everything, and the shared scope, whose subtree every view reaches. Both are
//! ordinary entities — they carry components, they can be opened as scopes, and
//! a lookup by handle finds them — but neither is content. They are never
//! counted, iterated, moved or despawned.
//!
//! [`Internal`] is what says so. It is a component, so the rows that carry it
//! share an archetype with each other and with nothing else, which is the whole
//! point: a query, a count and an archetype report each decide `Internal` versus
//! content once per archetype instead of testing every row. Holding an
//! archetype of their own also means their rows are free to move when the
//! world's own bookkeeping changes, without a query's starting offset going
//! stale.
//!
//! The marker is deliberately not public. A caller that could mark a row as
//! internal would also need somewhere to remove it, and no such path exists
//! yet; the world is the only thing that marks rows and the only thing that
//! owns them for their whole life. Adding a caller means deciding what removing
//! one does first.

/// Marks an entity the world holds for itself rather than as content.
///
/// Carrying it puts the entity in the internal archetype, which
/// [`crate::Scope`] counting, querying and reporting all skip whole.
pub(crate) struct Internal;
