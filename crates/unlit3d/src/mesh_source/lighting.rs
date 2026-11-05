use super::*;

impl MeshSource {
    /// Cluster the frame's lights, then grow and upload the three arrays and
    /// the uniform a lit draw reads them through.
    ///
    /// Clustering runs on the CPU, so the cells and the records reach the GPU
    /// the same way on every backend: the arrays are the ones the metadata
    /// already travels in, a storage buffer on WebGPU and a texture on WebGL2.
    /// The assignment is rewritten every frame, because the camera and the
    /// lights can both move.
    ///
    /// A quad light's corners are appended to one frame-wide vertex pool as the
    /// lights are packed, and the record names the slice each light took.
    pub(super) fn upload_lights(
        &mut self,
        world: &Scope<'_>,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        size: (u32, u32),
    ) {
        let device = self.device(world);
        crate::light::collect_lights(
            world,
            camera.position(),
            &mut self.packed_lights,
            &mut self.light_vertices,
            &mut self.lighting,
        );
        self.light_clusters.update(
            &self.clustering,
            &camera.clip_from_view,
            &camera.view_from_world,
            size,
            &mut self.packed_lights,
        );
        // The clusterer ranks the lights, and a cell indexes them by that rank,
        // so the array is the ranked order rather than the spawn order.
        self.light_records.clear();
        self.light_records
            .extend(self.packed_lights.iter().map(|light| light.gpu));

        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::lights",
            &mut self.lights_array,
            &self.lights_buf,
            &device,
            self.light_records.len().max(1) as u64,
        );
        // A cell is one element, and the clusterer stores each as a word per
        // slot: sizing by the words would ask for sixteen times the room.
        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::light_cells",
            &mut self.light_cells_array,
            &self.light_cells_buf,
            &device,
            (self.light_clusters.cells.len() / CELL_WORDS).max(1) as u64,
        );
        Self::grow_array_to_fit(
            world,
            self.context,
            "unlit3d::light_vertices",
            &mut self.light_vertices_array,
            &self.light_vertices_buf,
            &device,
            self.light_vertices.len().max(1) as u64,
        );
        self.lights_array
            .upload(&device, encoder, self.light_records.as_bytes());
        self.light_cells_array
            .upload(&device, encoder, self.light_clusters.cells.as_bytes());
        self.light_vertices_array
            .upload(&device, encoder, self.light_vertices.as_bytes());

        let uniform = self.clustering.uniform(
            size,
            camera.clip_from_view.row(3).truncate() != glam::Vec3::ZERO,
        );
        let buffer = self
            .context
            .graph(world)
            .get()
            .get(&self.cluster_buf)
            .expect("cluster buffer exists")
            .clone();
        self.cluster_staging
            .write(&device, encoder, &buffer, 0, uniform.as_bytes());

        let buffer = self
            .context
            .graph(world)
            .get()
            .get(&self.lighting_buf)
            .expect("lighting buffer exists")
            .clone();
        self.lighting_staging
            .write(&device, encoder, &buffer, 0, self.lighting.as_bytes());
    }
}
