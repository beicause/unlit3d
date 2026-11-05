use super::*;

impl MeshSource {
    // -- skin and morph arrays --------------------------------------------------

    /// Grow the frame's joint-matrix array if the packed data outgrew it, and
    /// upload it through the frame's encoder.
    ///
    /// Like the metadata array the resource is replaced only when it is too
    /// small, so a steady frame rewrites in place and rebuilds no bind group.
    pub(super) fn upload_joints(&mut self, world: &Scope<'_>, encoder: &mut wgpu::CommandEncoder) {
        let device = self.device(world);
        let needed = self.packed_joints.len().max(1) as u64;
        Self::grow_array_to_fit(
            world,
            self.context,
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
    /// resource is
    /// replaced only when it is too small, so a steady scene rewrites in place
    /// and rebuilds no bind group.
    pub(super) fn upload_morph_deltas(
        &mut self,
        world: &Scope<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if !self.morph_deltas_dirty {
            return;
        }
        self.morph_deltas_dirty = false;
        let device = self.device(world);
        if self.morph_deltas_pool.upload(&device, encoder) {
            // A replaced array invalidates every global group bound to it; the
            // next maintain rebuilds them.
            self.context
                .graph(world)
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
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let device = self.device(world);
        let needed = self.packed_morph_weights.len().max(1) as u64;
        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::pose::morph_weights",
            &mut self.morph_weights_array,
            &self.morph_weights_buf,
            &device,
            needed,
        );
        self.morph_weights_array
            .upload(&device, encoder, self.packed_morph_weights.as_bytes());
    }
    /// The ids of the source's global arrays, as a factory and a rebuild
    /// recipe see them. The camera, the globals and the view are not among
    /// them: the frame holds those.
    pub(super) fn global_resources(&self) -> GlobalResources {
        GlobalResources {
            metadata: self.metadata_buf.clone(),
            joints: self.joints_buf.clone(),
            morph_weights: self.morph_weights_buf.clone(),
            morph_deltas: self.morph_deltas_buf.clone(),
            lights: self.lights_buf.clone(),
            light_cells: self.light_cells_buf.clone(),
            light_vertices: self.light_vertices_buf.clone(),
            cluster: self.cluster_buf.clone(),
            lighting: self.lighting_buf.clone(),
        }
    }

    /// Grow `array` to hold at least `capacity` elements, publishing the new
    /// resource into the graph when it had to be replaced.
    ///
    /// A replaced array leaves every bind group built from the old one stale;
    /// the graph marks their nodes dirty here, and the next
    /// [`Self::maintain`] rebuilds them. A no-op when the array already has the
    /// room, so a steady frame replaces nothing and marks nothing.
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
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if !self.metadata_dirty {
            return;
        }
        self.metadata_dirty = false;

        let device = self.device(world);
        let needed = self.metadata.len().max(1) as u64;
        // Replace rather than resize: neither a buffer nor a texture has a size
        // that can change.
        Self::grow_array_to_fit(
            world,
            self.context,
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
