//! The pools a pooled mesh shares with every other pooled mesh.

use super::{
    BufferPool, HashMap, ResHandle, ResourceGraph, VertexBufferLayoutDesc, VertexStreamPool,
};

/// Initial element capacity per index or vertex stream.
const INITIAL_MESH_ELEMENTS: u32 = 1024;

/// The vertex and index pools every mesh uploaded through
/// [`MeshSource::allocate_pooled_mesh`](crate::mesh_source::MeshSource::allocate_pooled_mesh)
/// draws out of.
///
/// Each pool is one buffer in the resource graph that many meshes read through,
/// so the node is strong — the source owns it, no mesh does — and a mesh names
/// its slice by an element range rather than by a resource of its own.
pub(crate) struct MeshPools {
    /// The pool every pooled mesh keeps its indices in, as one large buffer
    /// shared by every indexed mesh.
    ///
    /// A mesh names it through
    /// [`MeshParts::index_buffer`](crate::components::MeshParts::index_buffer), which
    /// is why the buffer is a node in the resource graph and has to be replaced
    /// there when the pool grows; see
    /// [`MeshSource::sync_pool_node`](crate::mesh_source::MeshSource::sync_pool_node).
    pub index_pool: BufferPool,
    /// The graph node of [`Self::index_pool`]'s buffer.
    pub index_pool_id: ResHandle<wgpu::Buffer>,
    /// The pool every pooled mesh keeps its vertices in: one large buffer per
    /// vertex layout, so meshes that share a layout share a buffer, and one
    /// element allocation covers every stream of a mesh at the same element
    /// index.
    pub vertex_pool: VertexStreamPool,
    /// The graph node of each of [`Self::vertex_pool`]'s buffers, by the layout
    /// the buffer is shaped for.
    pub vertex_pool_ids: HashMap<VertexBufferLayoutDesc, ResHandle<wgpu::Buffer>>,
}

impl MeshPools {
    /// Create both pools and the graph node the index pool's buffer lives under.
    ///
    /// Each stream starts with capacity for `INITIAL_MESH_ELEMENTS` and grows
    /// as meshes are allocated.
    pub(super) fn new(device: &wgpu::Device, graph: &mut ResourceGraph) -> Self {
        let index_pool = BufferPool::new(
            device,
            "unlit3d::mesh::indices",
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            size_of::<u32>() as u64 * u64::from(INITIAL_MESH_ELEMENTS),
        );
        let index_pool_id = graph.insert(index_pool.buffer().clone(), None);
        let vertex_pool = VertexStreamPool::new(
            device,
            "unlit3d::mesh::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            INITIAL_MESH_ELEMENTS,
        );

        Self {
            index_pool,
            index_pool_id,
            vertex_pool,
            vertex_pool_ids: HashMap::new(),
        }
    }
}
