//! The frame arrays: the metadata entry every mesh owns and the pose data a
//! draw reads.
//!
//! One [`FrameArrays`] holds the arrays a frame's draws read — the mesh-metadata
//! entries, the joint matrices, the morph weights and the pooled morph
//! displacements — in whichever resource the device can read: a storage buffer
//! where it has them, a texture where it does not.
//!
//! The arrays grow but never shrink, and a replaced array is published into the
//! resource graph, which marks every bind group built from the old one dirty.

use super::*;

use crate::mesh::MorphDeltas;

/// Grow `array` to hold at least `capacity` elements, publishing the new
/// resource into the graph when it had to be replaced.
///
/// A replaced array leaves every bind group built from the old one stale; the
/// graph marks their nodes dirty here, and the next
/// [`MeshSource::maintain`](crate::mesh_source::MeshSource::maintain) rebuilds
/// them. A no-op when the array already has the room, so a steady frame
/// replaces nothing and marks nothing.
pub(super) fn grow_array_to_fit(
    world: &Scope<'_>,
    ctx: RenderContext,
    label: &str,
    array: &mut Array,
    node: &ResHandle<ArrayHandle>,
    device: &wgpu::Device,
    needed: u64,
) {
    // Grow only when the array is genuinely too small: growing by a factor
    // on every call would inflate the capacity without bound.
    if needed <= array.capacity() {
        return;
    }
    let capacity = u64::from(grown_capacity(array.capacity() as u32, needed as u32));
    if !array.grow_to(device, Some(label), capacity) {
        return;
    }
    ctx.graph(world)
        .get()
        .replace(node, array.handle())
        .expect("the array's node exists");
}

/// The arrays of one frame that are not lights: a metadata entry per live mesh,
/// and the pose data the frame's instances are packed into.
pub(crate) struct FrameArrays {
    /// The mesh-metadata array, held in whichever resource the device reads.
    pub(super) metadata_array: Array,
    /// Resource id of the mesh-metadata array.
    pub(super) metadata_buf: ResHandle<ArrayHandle>,
    /// Metadata entries, one per live uploaded mesh.
    metadata: Vec<MeshMetadata>,
    /// Metadata slots whose mesh was removed and whose index is free to hand
    /// out again.
    free_metadata: Vec<u32>,
    /// Whether the metadata array changed since it was last uploaded.
    ///
    /// Allocating or removing a mesh marks it; the next
    /// [build](FrameSource::build_scene) uploads the array then, so an upload
    /// always lands in the same encoder as the draws that read it.
    pub(super) metadata_dirty: bool,

    /// The frame's joint-matrix array, held in whichever resource the device
    /// reads.
    pub(super) joints_array: Array,
    /// Resource id of the frame's joint-matrix array.
    pub(super) joints_buf: ResHandle<ArrayHandle>,
    /// The frame's joint matrices, packed in visible-instance order.
    ///
    /// Reused across frames rather than reallocated: the packed data is exactly
    /// what is uploaded, so the array and the buffer stay one allocation each.
    pub(super) packed_joints: Vec<JointMatrix>,

    /// The frame's morph-weight array, held in whichever resource the device
    /// reads.
    pub(super) morph_weights_array: Array,
    /// Resource id of the frame's morph-weight array.
    pub(super) morph_weights_buf: ResHandle<ArrayHandle>,
    /// The frame's morph weights, packed in visible-instance order.
    pub(super) packed_morph_weights: Vec<f32>,

    /// The frame's morph-displacement array, pooling every morphed mesh's
    /// deltas into one resource.
    ///
    /// A mesh's displacements are its own geometry, but one array holds them
    /// for the frame and the mesh names its slice through its metadata entry's
    /// `morph_deltas_offset`. That is what leaves the mesh group nothing to
    /// bind.
    morph_deltas_pool: ArrayPool,
    /// Resource id of the frame's morph-displacement array.
    pub(super) morph_deltas_buf: ResHandle<ArrayHandle>,
    /// Whether a mesh's displacements were written into the pool since the
    /// array was last uploaded.
    ///
    /// Allocating or removing a mesh marks it; the next
    /// [build](FrameSource::build_scene) uploads the array then, so the upload
    /// always lands in the same encoder as the draws that read it.
    morph_deltas_dirty: bool,
}

impl FrameArrays {
    /// Create every array the frame's draws read, each with room for one
    /// element — the smallest a shader can read, since none may be empty — and
    /// insert it into `graph`.
    pub(super) fn new(
        device: &wgpu::Device,
        graph: &mut ResourceGraph,
        array_max_dimension: Option<u32>,
    ) -> Self {
        let metadata_array = Array::new(
            device,
            Some("unlit3d::mesh_metadata"),
            <MeshMetadata as const_shader_layout::ShaderLayout>::SIZE.get(),
            1,
            array_max_dimension,
        );
        let metadata_buf = graph.insert(metadata_array.handle(), None);
        let joints_array = Array::new(
            device,
            Some("unlit3d::pose::joints"),
            <JointMatrix as const_shader_layout::ShaderLayout>::SIZE.get(),
            1,
            array_max_dimension,
        );
        let joints_buf = graph.insert(joints_array.handle(), None);
        let morph_weights_array = Array::new(
            device,
            Some("unlit3d::pose::morph_weights"),
            size_of::<f32>() as u64,
            1,
            array_max_dimension,
        );
        let morph_weights_buf = graph.insert(morph_weights_array.handle(), None);
        // The morph displacements are pooled rather than indexed: a mesh's
        // slice is as long as it has vertices, so it takes a sub-allocation
        // like a vertex stream rather than a fixed slot like an entry.
        let morph_deltas_pool = ArrayPool::new(
            device,
            "unlit3d::morph_deltas",
            size_of::<f32>() as u64,
            1,
            array_max_dimension,
        );
        let morph_deltas_buf = graph.insert(morph_deltas_pool.handle(), None);

        Self {
            metadata_array,
            metadata_buf,
            metadata: Vec::new(),
            free_metadata: Vec::new(),
            metadata_dirty: false,
            joints_array,
            joints_buf,
            packed_joints: Vec::new(),
            morph_weights_array,
            morph_weights_buf,
            packed_morph_weights: Vec::new(),
            morph_deltas_pool,
            morph_deltas_buf,
            morph_deltas_dirty: false,
        }
    }

    /// Take a metadata slot for `metadata` and mark the array for upload.
    ///
    /// A slot a removed mesh held is reused, so the array stays as dense as the
    /// meshes that are still alive.
    pub(super) fn allocate_metadata(&mut self, metadata: MeshMetadata) -> u32 {
        self.metadata_dirty = true;
        match self.free_metadata.pop() {
            Some(index) => {
                self.metadata[index as usize] = metadata;
                index
            }
            None => {
                let index = self.metadata.len() as u32;
                self.metadata.push(metadata);
                index
            }
        }
    }

    /// Clear the entry of the mesh that held `index` and hand the slot back.
    pub(super) fn release_metadata(&mut self, index: u32) {
        let emptied = self.metadata.get_mut(index as usize);
        if let Some(entry) = emptied {
            *entry = MeshMetadata::default();
            self.free_metadata.push(index);
            self.metadata_dirty = true;
        }
    }

    /// Copy `deltas` into the frame's pooled displacements and return where
    /// they landed.
    pub(super) fn allocate_morph_deltas(
        &mut self,
        device: &wgpu::Device,
        deltas: &MorphDeltas,
    ) -> (u32, unlit_wgpu::offset_allocator::Allocation) {
        let range = self
            .morph_deltas_pool
            .allocate(device, deltas.deltas.len() as u32)
            .expect("the morph-displacement pool grows");
        self.morph_deltas_pool
            .write(range, deltas.deltas.as_bytes());
        self.morph_deltas_dirty = true;
        (range.allocation.offset, range.allocation)
    }

    /// Hand a pooled displacement range back.
    pub(super) fn release_morph_deltas(
        &mut self,
        allocation: unlit_wgpu::offset_allocator::Allocation,
    ) {
        self.morph_deltas_pool.release(allocation);
    }

    /// Grow the frame's joint-matrix array if the packed data outgrew it, and
    /// upload it through the frame's encoder.
    ///
    /// Like the metadata array the resource is replaced only when it is too
    /// small, so a steady frame rewrites in place and rebuilds no bind group.
    pub(super) fn upload_joints(
        &mut self,
        world: &Scope<'_>,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let device = ctx
            .device(world)
            .expect("the context's device resource exists");
        let needed = self.packed_joints.len().max(1) as u64;
        grow_array_to_fit(
            world,
            ctx,
            "unlit3d::pose::joints",
            &mut self.joints_array,
            &self.joints_buf,
            &device,
            needed,
        );
        self.joints_array
            .upload(&device, encoder, self.packed_joints.as_bytes());
    }

    /// Grow the frame's morph-displacement array if a mesh outgrew it, and
    /// upload the pooled slices through the frame's encoder.
    ///
    /// Allocating or removing a mesh marks the array dirty, so the upload
    /// lands in the next frame together with the draws that read it. The
    /// resource is replaced only when it is too small, so a steady scene
    /// rewrites in place and rebuilds no bind group.
    pub(super) fn upload_morph_deltas(
        &mut self,
        world: &Scope<'_>,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if !self.morph_deltas_dirty {
            return;
        }
        self.morph_deltas_dirty = false;
        let device = ctx
            .device(world)
            .expect("the context's device resource exists");
        if self.morph_deltas_pool.upload(&device, encoder) {
            // A replaced array invalidates every global group bound to it; the
            // next maintain rebuilds them.
            ctx.graph(world)
                .get()
                .replace(&self.morph_deltas_buf, self.morph_deltas_pool.handle())
                .expect("the morph-displacement array's node exists");
        }
    }

    /// Grow the frame's morph-weight array if the packed data outgrew it, and
    /// upload it through the frame's encoder.
    pub(super) fn upload_morph_weights(
        &mut self,
        world: &Scope<'_>,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let device = ctx
            .device(world)
            .expect("the context's device resource exists");
        let needed = self.packed_morph_weights.len().max(1) as u64;
        grow_array_to_fit(
            world,
            ctx,
            "unlit3d::pose::morph_weights",
            &mut self.morph_weights_array,
            &self.morph_weights_buf,
            &device,
            needed,
        );
        self.morph_weights_array
            .upload(&device, encoder, self.packed_morph_weights.as_bytes());
    }

    /// Grow the metadata buffer if the array outgrew it, and upload the array
    /// through the frame's encoder if it changed.
    ///
    /// Allocating or removing a mesh marks the array dirty, so the upload lands
    /// in the next frame together with the draws that read it. The storage
    /// buffer is recreated only when the array outgrows it, so a steady scene
    /// rewrites in place and rebuilds no bind group.
    pub(super) fn upload_metadata(
        &mut self,
        world: &Scope<'_>,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if !self.metadata_dirty {
            return;
        }
        self.metadata_dirty = false;

        let device = ctx
            .device(world)
            .expect("the context's device resource exists");
        let needed = self.metadata.len().max(1) as u64;
        // Replace rather than resize: neither a buffer nor a texture has a size
        // that can change.
        grow_array_to_fit(
            world,
            ctx,
            "unlit3d::mesh_metadata",
            &mut self.metadata_array,
            &self.metadata_buf,
            &device,
            needed,
        );
        self.metadata_array
            .upload(&device, encoder, self.metadata.as_bytes());
    }
}
