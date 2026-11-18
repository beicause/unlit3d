//! The storage a mesh carries so the source can reclaim what it shared.

use super::{Allocation, Rc, RefCell};

/// The shared-storage holdings of one mesh, posted to the source when the
/// mesh dies.
///
/// The source builds the ticket as it allocates: only the owner of a pool or
/// an array can name what a range means to it, so the mesh carries the ticket
/// opaque and hands it back whole.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StorageTicket {
    /// The mesh's element range in the vertex pool.
    pub(super) vertex: Option<Allocation>,
    /// The mesh's range in the index pool.
    pub(super) index: Option<Allocation>,
    /// The mesh's range in the frame-wide morph-displacement pool.
    pub(super) morph_deltas: Option<Allocation>,
    /// The mesh's entry in the metadata array.
    pub(super) metadata_index: u32,
}

/// The shared-storage holdings a mesh carries inside its
/// [`MeshParts`](crate::components::MeshParts).
///
/// Dropping the last [`GpuMesh`](crate::components::GpuMesh) handle drops this,
/// and the drop posts the
/// mesh's ticket to the inbox it shares with the source: the mesh
/// cannot release the ranges itself, because the pools they came from are
/// reached only through the world a drop holds no borrow of. The source's
/// next maintain drains the inbox and hands the ranges back, so a freed
/// metadata slot is reused by what the next frame allocates.
///
/// A mesh is shared by cloning its [`GpuMesh`](crate::components::GpuMesh), which
/// shares the parts; the
/// storage is neither cloned nor handed out, so one mesh's death posts
/// exactly one ticket.
///
/// The default is a detached storage: nothing is allocated against it, so
/// its drop posts an empty ticket to an inbox nothing drains.
#[derive(Debug, Default)]
pub struct MeshStorage {
    /// The queue the drop posts the ticket to, shared with the source that
    /// allocated the mesh's ranges.
    pub(super) inbox: Rc<RefCell<Vec<StorageTicket>>>,
    /// The mesh's holdings, posted to the inbox on drop.
    pub(super) ticket: StorageTicket,
    /// Index of the mesh's entry in the source's mesh-metadata array.
    ///
    /// Every mesh owns an entry, whether or not the pipeline that draws it
    /// reads one.
    pub metadata_index: u32,
}

impl Drop for MeshStorage {
    /// Post the mesh's holdings to the inbox. The release is left for the
    /// source's next reclaim, which runs at the frame's own pace rather than
    /// inside a drop.
    fn drop(&mut self) {
        self.inbox.borrow_mut().push(self.ticket);
    }
}
