//! Pooled mesh allocation: meshes that share the source's pools.
//!
//! A pooled mesh keeps its vertices in the source's vertex streams and its
//! indices in its index pool, so every such mesh draws out of the same buffers
//! and names its own range through [GpuMesh::draw]. [`MeshSource::allocate_pooled_mesh`] is the one
//! entry: the caller states the streams the mesh occupies and fills each of
//! them, and the source allocates the ranges, applies the pool offset where the
//! device requires it and hands back a complete handle.
//!
//! [`MeshSource::allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh)
//! is the built-in family's use of it; a caller's own family allocates its
//! meshes the same way.

use super::{
    ArrayVec, GpuMesh, MAX_VERTEX_BUFFERS, MeshMetadata, MeshSource, MeshStorage, Scope,
    StorageTicket, VertexBufferLayoutDesc, VertexLayout, VertexStreamPool,
};
use crate::bounds::Aabb;
use crate::mesh::{MeshDraw, MorphDeltas, SkinShape};
use core::mem::size_of;
use core::num::NonZeroU64;
use unlit_wgpu::mesh::index_fits_u16;
use wgpu::WriteOnly;
use zerocopy::byteorder::native_endian::{U16, U32};

/// The byte size of one index of `format`.
///
/// A pooled index range starts at a multiple of [`wgpu::COPY_BUFFER_ALIGNMENT`],
/// which is a multiple of either format's size, so dividing its byte offset by
/// this yields a whole number of indices.
fn index_format_size(format: wgpu::IndexFormat) -> u32 {
    match format {
        wgpu::IndexFormat::Uint16 => size_of::<u16>() as u32,
        wgpu::IndexFormat::Uint32 => size_of::<u32>() as u32,
    }
}

/// Where a mesh's vertex-pool offset goes, given whether the device has
/// `base_vertex`.
///
/// A mesh's vertices sit somewhere inside the pool its streams share, so a draw
/// has to be offset to reach them. A device with `base_vertex` names the offset
/// on the draw, which leaves the indices mesh-local and keeps them narrow; a
/// device without it — WebGL2, whose GLES 3.0 has no such draw — has to add the
/// offset to the indices at upload, where the draw's own stays zero. The two
/// are complementary, and exactly one is non-zero.
///
/// Baking can widen the indices when the offset pushes one past `u16::MAX`,
/// which is the cost of binding the whole pool instead of rebinding a vertex
/// buffer per mesh. A draw's `base_vertex` has no such cost, but is a signed
/// 32-bit integer, so the offset has to fit one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexOffset {
    /// Added to every index at upload.
    baked: u32,
    /// Carried by the draw as its `base_vertex`.
    draw_base_vertex: u32,
}

impl IndexOffset {
    /// `index` with the pool offset baked in.
    ///
    /// # Panics
    ///
    /// When the sum overflows `u32`, which a pool too large to address at all
    /// reaches; wrapping would silently point the index at another mesh.
    fn baked_index(self, index: u32) -> u32 {
        index
            .checked_add(self.baked)
            .expect("a pooled index stays inside the 32-bit index space")
    }

    /// Split `vertex_offset` between the indices and the draw.
    fn new(supports_base_vertex: bool, vertex_offset: u32) -> Self {
        if supports_base_vertex {
            Self {
                baked: 0,
                draw_base_vertex: vertex_offset,
            }
        } else {
            Self {
                baked: vertex_offset,
                draw_base_vertex: 0,
            }
        }
    }

    /// The format `indices` pack into and the number of bytes they occupy,
    /// with the pool offset applied.
    ///
    /// An offset carried by the draw leaves the indices mesh-local, so they
    /// stay as narrow as the mesh's own vertex count allows; an offset baked in
    /// here can push an index past `u16::MAX`, which widens *every* index of
    /// the mesh to `Uint32`. Either way the width is decided on the baked
    /// values.
    ///
    /// The length is padded to [`wgpu::COPY_BUFFER_ALIGNMENT`], which is the
    /// alignment an index pool hands its ranges out at, so it is both the size
    /// to allocate and the number of bytes [`Self::write`] fills.
    ///
    /// The slice must not be empty: an empty mesh has no index buffer at all.
    fn packed_len(self, indices: &[u32]) -> (wgpu::IndexFormat, usize) {
        let widened = indices
            .iter()
            .any(|&index| !index_fits_u16(self.baked_index(index)));
        let element = if widened {
            size_of::<u32>()
        } else {
            size_of::<u16>()
        };
        let format = if widened {
            wgpu::IndexFormat::Uint32
        } else {
            wgpu::IndexFormat::Uint16
        };
        let padded_len =
            (indices.len() * element).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize);
        (format, padded_len)
    }

    /// Write `indices` with the pool offset applied into `out`, which must be
    /// exactly [`Self::packed_len`]`(indices)` bytes.
    ///
    /// The offset is applied as the bytes are written, so no intermediate index
    /// buffer exists; the trailing alignment padding is zeroed.
    ///
    /// The slice must not be empty: an empty mesh has no index buffer at all.
    fn write(self, indices: &[u32], out: wgpu::WriteOnly<'_, [u8]>) {
        let (format, padded_len) = self.packed_len(indices);
        assert_eq!(
            out.len(),
            padded_len,
            "the target must hold exactly the packed index buffer"
        );
        let element = index_format_size(format) as usize;
        let (data, mut padding) = out.split_at(indices.len() * element);
        match format {
            wgpu::IndexFormat::Uint16 => {
                let (chunks, _remainder) = data.into_chunks::<{ size_of::<u16>() }>();
                chunks.write_iter(
                    indices
                        .iter()
                        .map(|&index| U16::new(self.baked_index(index) as u16).into()),
                );
            }
            wgpu::IndexFormat::Uint32 => {
                let (chunks, _remainder) = data.into_chunks::<{ size_of::<u32>() }>();
                chunks.write_iter(
                    indices
                        .iter()
                        .map(|&index| U32::new(self.baked_index(index)).into()),
                );
            }
        }
        padding.fill(0);
    }

    /// [`Self::write`] into a fresh `Vec`, for tests that inspect the bytes.
    #[cfg(test)]
    fn pack(self, indices: &[u32]) -> (wgpu::IndexFormat, Vec<u8>) {
        let (format, padded_len) = self.packed_len(indices);
        let mut data = vec![0u8; padded_len];
        self.write(indices, wgpu::WriteOnly::from_mut(data.as_mut_slice()));
        (format, data)
    }
}

/// The streams of a mesh uploaded through
/// [`MeshSource::allocate_pooled_mesh`], slot by slot.
///
/// Every stream describes the same vertices, in the same order, and the mesh
/// holds one element allocation across all of them, which is what lets a draw
/// address them all with one `firstVertex` or `baseVertex`. A stream whose
/// layout declares a stride of zero is listed but never written, which is how a
/// slot the pipeline declares and the mesh does not fill stays empty.
pub struct PooledMeshDesc<'a> {
    /// The slots the mesh fills, with the layout each one declares.
    pub streams: &'a [(u32, VertexBufferLayoutDesc)],
    /// How many vertices every stream describes.
    pub vertex_count: usize,
    /// Triangle indices, for an indexed draw.
    pub indices: Option<&'a [u32]>,
    /// The morph displacements that deform the mesh.
    pub morph_deltas: Option<MorphDeltas>,
    /// How many joint matrices the mesh's position stream is deformed by, or
    /// `None` when it carries no joint indices.
    ///
    /// A skinned mesh is drawn by an entity that names the pose it deforms by,
    /// so this is what lets the draw path require that binding and hold the
    /// pose to hold a matrix for every joint.
    pub skin: Option<SkinShape>,
}

/// One stream of a pooled mesh, handed to its writer as it is filled.
///
/// `out` is the pool range the stream occupies, exactly its stride times the
/// vertex count. `metadata` is the entry the mesh will own: a writer that
/// derives decode parameters from the data it packs — the position compression
/// and the UV extent among them — fills it here, and the caller sees what it
/// derived.
pub struct PooledStream<'a> {
    /// The slot this stream fills.
    pub slot: u32,
    /// The layout the stream declares.
    pub layout: &'a VertexBufferLayoutDesc,
    /// The mesh's metadata entry, as far as the streams have filled it.
    pub metadata: &'a mut MeshMetadata,
    /// The pool range the stream is packed into.
    pub out: WriteOnly<'a, [u8]>,
}

impl MeshSource {
    /// Upload a mesh into the source's pools and return a [GpuMesh] handle.
    ///
    /// Unlike [`Self::allocate_mesh`], the mesh owns neither its vertex buffers
    /// nor its index buffer: its vertices go into the streams the source shares
    /// between meshes and its indices into the source's index pool, and the
    /// handle names the pool nodes together with the ranges this mesh holds.
    /// Once the mesh dies — the last handle dropped and the graph's maintain
    /// run — the sweep hands those ranges back while the pool buffers outlive
    /// them.
    ///
    /// `write` fills each declared stream in turn, into the pool range it is
    /// handed; it is called once per stream whose layout declares a stride. The
    /// metadata it derives reaches the mesh's entry, and with it the shader's
    /// decode parameters.
    ///
    /// How a draw reaches a mesh's vertices inside the pool is the one thing
    /// that depends on the device: a draw's `base_vertex` carries the offset
    /// where the device has one, and where it does not — WebGL2 — the offset is
    /// added to the indices at upload and the draw's stays zero. Baking can
    /// widen the indices from `u16` to `u32`, which is the cost of binding the
    /// whole pool instead of rebinding a vertex buffer per mesh.
    ///
    /// # Panics
    ///
    /// If `vertex_count` is zero; if a stream's range does not fit its pool
    /// buffer; or if a pool offset that a draw has to carry does not fit the
    /// `i32` a `base_vertex` is.
    pub fn allocate_pooled_mesh(
        &mut self,
        world: &Scope<'_>,
        desc: PooledMeshDesc<'_>,
        mut write: impl FnMut(PooledStream<'_>),
    ) -> GpuMesh {
        let device = self.device(world);
        let queue = self.queue(world);
        let PooledMeshDesc {
            streams,
            vertex_count,
            indices,
            morph_deltas,
            skin,
        } = desc;
        assert!(vertex_count > 0, "a pooled mesh covers at least one vertex");

        // One element allocation covers every stream at the same element index.
        // A stream the mesh declares nothing for gets no buffer and no part in
        // the allocation, so it is listed with a stride of zero.
        let layouts: ArrayVec<_, MAX_VERTEX_BUFFERS> =
            streams.iter().map(|(_, layout)| layout.clone()).collect();
        let vertices = self
            .pools
            .vertex_pool
            .allocate(&device, &queue, &layouts, vertex_count as u32)
            .expect("the vertex pool grows with the mesh");
        let vertex_offset = vertices.allocation.offset;

        // Each stream is packed straight into the pool through a queue write,
        // so no CPU-side buffer is built first. The write view is the only
        // mutable borrow of the staging memory, so the streams are packed one
        // after the other.
        let mut metadata = MeshMetadata::default();
        for (slot, layout) in streams {
            if layout.array_stride == 0 {
                continue;
            }
            let len = layout.array_stride as usize * vertex_count;
            let id = self.vertex_node(world, layout);
            self.sync_vertex_node(world, &id, layout);
            let buffer = self
                .context
                .graph(world)
                .get()
                .get(&id)
                .expect("the stream's node exists")
                .clone();
            let mut view = queue
                .write_buffer_with(
                    &buffer,
                    VertexStreamPool::byte_offset(layout, vertex_offset),
                    NonZeroU64::new(len as u64).expect("a declared stream is never empty"),
                )
                .expect("the stream's range fits its buffer");
            write(PooledStream {
                slot: *slot,
                layout,
                metadata: &mut metadata,
                out: view.slice(..),
            });
        }

        // A draw's `base_vertex` is a signed 32-bit integer, so an offset the
        // draw has to carry must fit one. A baked offset is a `u32` index like
        // any other and has no such limit.
        let supports_base_vertex = self.capabilities(world).base_vertex();
        assert!(
            !supports_base_vertex || vertex_offset <= i32::MAX as u32,
            "a vertex offset has to fit the i32 a draw's base vertex is"
        );
        let offset = IndexOffset::new(supports_base_vertex, vertex_offset);
        // Zero whenever the offset was baked into the indices instead, which is
        // what a draw on a device without `base_vertex` requires. The metadata
        // entry carries `vertex_offset` either way: the shader reads
        // pool-global vertex ordinals on both paths.
        let draw_base_vertex = offset.draw_base_vertex;
        let (index_buffer, draw, index_allocation) = match indices {
            Some(indices) if !indices.is_empty() => {
                // An offset that pushes an index past `u16::MAX` has to widen
                // the buffer it is written into, so the format and the bytes
                // are decided together.
                let (format, packed_len) = offset.packed_len(indices);
                let range = self
                    .pools
                    .index_pool
                    .allocate(&device, &queue, packed_len as u32)
                    .expect("the index pool grows with the mesh");
                {
                    let mut graph_holder = self.context.graph(world);
                    let graph = graph_holder.get();
                    Self::sync_pool_node(&self.pools.index_pool, &self.pools.index_pool_id, graph);
                    let buffer = graph
                        .get(&self.pools.index_pool_id)
                        .expect("the index pool node exists")
                        .clone();
                    let mut view = queue
                        .write_buffer_with(
                            &buffer,
                            u64::from(range.allocation.offset),
                            NonZeroU64::new(packed_len as u64)
                                .expect("an empty index buffer is never allocated"),
                        )
                        .expect("the index range fits its buffer");
                    offset.write(indices, view.slice(..));
                }
                let first = range.allocation.offset / index_format_size(format);
                (
                    Some((self.pools.index_pool_id.clone(), format)),
                    MeshDraw::Indexed {
                        indices: first..first + indices.len() as u32,
                        base_vertex: draw_base_vertex as i32,
                    },
                    Some(range.allocation),
                )
            }
            // A non-indexed draw names its first vertex directly, so the pool
            // offset goes on the draw's range. A non-zero `base_vertex` is
            // ignored for a non-indexed draw, so it cannot carry the offset.
            _ => (
                None,
                MeshDraw::Vertices {
                    vertices: vertex_offset..vertex_offset + vertex_count as u32,
                },
                None,
            ),
        };

        // The mesh's layout is only the streams it draws from: the per-instance
        // stream is the family's own and is bound by the source at draw time,
        // so it is not part of the mesh's layout at all. A slot the pipeline
        // declares and the mesh does not fill is left out, since its buffer is
        // the pool's whole stream rather than anything this mesh owns.
        let mut layout = ArrayVec::<_, MAX_VERTEX_BUFFERS>::new();
        let mut vertex_buffers = ArrayVec::<_, MAX_VERTEX_BUFFERS>::new();
        for (slot, stream_layout) in streams {
            if stream_layout.array_stride == 0 {
                continue;
            }
            layout.push((*slot, stream_layout.clone()));
            let node = self.vertex_node(world, stream_layout);
            vertex_buffers.push((*slot, node));
        }
        let vertex_layout = VertexLayout::new(layout);

        // The mesh gives up its own metadata entry and displacement range only
        // when it dies, so both are recorded on the ticket its parts carry.
        let entry = self.allocate_entry(world, metadata, morph_deltas, vertex_offset);
        let storage = MeshStorage {
            inbox: self.inbox.clone(),
            ticket: StorageTicket {
                vertex: Some(vertices.allocation),
                index: index_allocation,
                morph_deltas: entry.morph_deltas,
                metadata_index: entry.metadata_index,
            },
            metadata_index: entry.metadata_index,
        };
        let aabb = Aabb::new(metadata.aabb_center, metadata.aabb_half_extents);
        let parts = self.mesh_parts(world, vertex_buffers, index_buffer, None, storage, &[]);
        GpuMesh {
            parts,
            vertex_layout,
            draw,
            aabb,
            morph_targets: entry.morph_targets,
            skin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_with_base_vertex_puts_the_offset_on_the_draw() {
        let offset = IndexOffset::new(true, 7);

        assert_eq!(offset.draw_base_vertex, 7, "the draw carries the offset");
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(
            format,
            wgpu::IndexFormat::Uint16,
            "the indices stay mesh-local, so they keep their width"
        );
        assert_eq!(packed_indices(format, &data, 3), vec![0, 1, 2]);
    }

    #[test]
    fn a_device_without_base_vertex_bakes_the_offset_into_the_indices() {
        // WebGL2's GLES 3.0 has no `base_vertex`, and the backend would panic
        // rather than emulate it, so the offset has to be in the indices.
        let offset = IndexOffset::new(false, 7);

        assert_eq!(offset.draw_base_vertex, 0, "the draw must not name one");
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(packed_indices(format, &data, 3), vec![7, 8, 9]);
    }

    #[test]
    fn a_mesh_at_the_start_of_the_pool_needs_no_offset_either_way() {
        // The common case, and the one where both paths agree: nothing is
        // baked and the draw's `base_vertex` is zero.
        for supports_base_vertex in [true, false] {
            let offset = IndexOffset::new(supports_base_vertex, 0);
            assert_eq!(offset.baked, 0);
            assert_eq!(offset.draw_base_vertex, 0);
            let (format, data) = offset.pack(&[0, 1, 2]);
            assert_eq!(format, wgpu::IndexFormat::Uint16);
            assert_eq!(packed_indices(format, &data, 3), vec![0, 1, 2]);
        }
    }

    #[test]
    fn a_baked_offset_can_widen_the_indices() {
        // Baking is what forces the wider format: an index that fitted `u16`
        // before the offset may not after it, and the compression is decided on
        // the baked values.
        let indices = [0u32, 1, 2];
        let offset = IndexOffset::new(false, u32::from(u16::MAX));
        let (format, data) = offset.pack(&indices);
        assert_eq!(
            format,
            wgpu::IndexFormat::Uint32,
            "an index past `u16::MAX` cannot be narrowed"
        );
        assert_eq!(packed_indices(format, &data, 3), vec![65535, 65536, 65537]);
    }

    #[test]
    fn packed_indices_are_padded_to_the_pool_alignment() {
        // The pool hands ranges out at `COPY_BUFFER_ALIGNMENT`, so `pack` pads
        // what it writes and its length is the size the pool is asked for.
        let offset = IndexOffset::new(true, 0);

        // Three `Uint16` indices are 6 bytes, rounded up to the alignment.
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(
            data.len(),
            (3 * size_of::<u16>()).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize)
        );
        assert!(
            data[3 * size_of::<u16>()..].iter().all(|&byte| byte == 0),
            "the padding is zeroed"
        );

        // Many indices round up by that alignment without a whole extra block
        // when they already land on one.
        let indices: Vec<u32> = (0..(u16::MAX as u32 - 1)).collect();
        let (format, data) = offset.pack(&indices);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(
            data.len(),
            (indices.len() * size_of::<u16>())
                .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize)
        );
    }

    /// The first `count` indices [`IndexOffset::pack`] wrote, as `u32`s.
    ///
    /// The bytes past them are the pool-alignment padding, which the caller
    /// sizes to but does not read.
    fn packed_indices(format: wgpu::IndexFormat, bytes: &[u8], count: usize) -> Vec<u32> {
        match format {
            wgpu::IndexFormat::Uint16 => bytes[..count * size_of::<u16>()]
                .as_chunks::<{ size_of::<u16>() }>()
                .0
                .iter()
                .map(|index| u32::from(U16::from(*index).get()))
                .collect(),
            wgpu::IndexFormat::Uint32 => bytes[..count * size_of::<u32>()]
                .as_chunks::<{ size_of::<u32>() }>()
                .0
                .iter()
                .map(|index| U32::from(*index).get())
                .collect(),
        }
    }
}
