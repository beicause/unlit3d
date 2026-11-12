//! The lights a frame draws with: the clustered arrays they are read through
//! and the uniforms that describe the clustering.

use super::*;

/// The frame's lights, the cluster assignment they are read through and the two
/// uniforms that describe both.
///
/// Clustering runs on the CPU, so the cells and the records reach the GPU the
/// same way on every backend: the arrays are the ones the metadata already
/// travels in, a storage buffer on WebGPU and a texture on WebGL2.
pub struct Lights {
    /// The frame's lights, held in whichever resource the device reads.
    pub(super) lights_array: Array,
    /// Resource id of the frame's light array.
    pub(super) lights_buf: ResHandle<ArrayHandle>,
    /// The frame's cluster cells, held in whichever resource the device reads.
    pub(super) light_cells_array: Array,
    /// Resource id of the frame's cluster-cell array.
    pub(super) light_cells_buf: ResHandle<ArrayHandle>,
    /// The frame's light vertices: the corner pool every quadrilateral light
    /// appends to.
    pub(super) light_vertices_array: Array,
    /// Resource id of the frame's light-vertex array.
    pub(super) light_vertices_buf: ResHandle<ArrayHandle>,
    /// Resource id of the cluster uniform buffer.
    pub(super) cluster_buf: ResHandle<wgpu::Buffer>,
    /// Resource id of the frame-wide lighting uniform buffer.
    pub(super) lighting_buf: ResHandle<wgpu::Buffer>,
    /// The cluster grid this frame's lights are assigned to.
    ///
    /// The source clusters every frame that has an active camera and a visible
    /// mesh, whether or not it draws a lit variant, since a frame's camera
    /// moves and its lights move with it. Its near and far planes are the
    /// clustering's own: a [Camera] carries a
    /// projection, not the extents that projection was built from.
    pub clustering: ClusterConfig,
    /// Reused per-frame light records, ranked by importance by the clusterer.
    packed_lights: Vec<PackedLight>,
    /// Reused per-frame cluster assignment.
    light_clusters: LightClusters,
    /// Reused per-frame polygon-vertex pool, which a quadrilateral light
    /// appends its corners to.
    light_vertices: Vec<glam::Vec4>,
    /// Reused per-frame light records, in the order the clusterer ranked them.
    light_records: Vec<LightGpu>,
    /// The frame-wide lighting uniform, rebuilt and uploaded on every frame
    /// that reaches the upload.
    lighting: LightingUniform,
    /// Staging buffer for the cluster uniform, reused across frames.
    cluster_staging: StagingBuffer,
    /// Staging buffer for the lighting uniform, reused across frames.
    lighting_staging: StagingBuffer,
}

impl Lights {
    /// Create the three arrays and the two uniforms a lit draw reads the frame's
    /// lights through, each with room for one element — the smallest a shader
    /// can read, since none may be empty — and insert them into `graph`.
    pub(super) fn new(
        device: &wgpu::Device,
        graph: &mut ResourceGraph,
        array_max_dimension: Option<u32>,
    ) -> Self {
        // The light array starts with room for one entry, the cells with one
        // grid's worth, and the vertices with one quadrilateral's; each grows on
        // the first built scene that needs more.
        let lights_array = Array::new(
            device,
            Some("unlit3d::lights"),
            <LightGpu as const_shader_layout::ShaderLayout>::SIZE.get(),
            1,
            array_max_dimension,
        );
        let lights_buf = graph.insert(lights_array.handle(), None);
        let light_cells_array = Array::new(
            device,
            Some("unlit3d::light_cells"),
            (CELL_WORDS * size_of::<u32>()) as u64,
            1,
            array_max_dimension,
        );
        let light_cells_buf = graph.insert(light_cells_array.handle(), None);
        let light_vertices_array = Array::new(
            device,
            Some("unlit3d::light_vertices"),
            size_of::<glam::Vec4>() as u64,
            1,
            array_max_dimension,
        );
        let light_vertices_buf = graph.insert(light_vertices_array.handle(), None);

        // A shader that reads one of the uniforms before the first lit frame
        // finds zeroed bytes rather than whatever the allocation held; a lit
        // pipeline only exists once a frame has uploaded them, so nothing reads
        // an unbuilt one.
        let cluster_buf = graph.insert(
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::cluster"),
                size: size_of::<ClusterUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            None,
        );
        let lighting_buf = graph.insert(
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::lighting"),
                size: size_of::<LightingUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            None,
        );

        Self {
            lights_array,
            lights_buf,
            light_cells_array,
            light_cells_buf,
            light_vertices_array,
            light_vertices_buf,
            cluster_buf,
            lighting_buf,
            clustering: ClusterConfig::DEFAULT,
            packed_lights: Vec::new(),
            light_clusters: LightClusters::default(),
            light_vertices: Vec::new(),
            light_records: Vec::new(),
            lighting: LightingUniform {
                directionals: [DirectionalGpu::default(); MAX_DIRECTIONAL_LIGHTS],
                ambient: glam::Vec4::ZERO,
            },
            cluster_staging: StagingBuffer::new(),
            lighting_staging: StagingBuffer::new(),
        }
    }

    /// Cluster the frame's lights, then grow and upload the three arrays and
    /// the uniform a lit draw reads them through.
    ///
    /// The assignment is rewritten every frame, because the camera and the
    /// lights can both move. A quad light's corners are appended to one
    /// frame-wide vertex pool as the lights are packed, and the record names the
    /// slice each light took.
    pub(super) fn upload(
        &mut self,
        world: &Scope<'_>,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        size: (u32, u32),
    ) {
        let device = ctx
            .device(world)
            .expect("the context's device resource exists");
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

        grow_array_to_fit(
            world,
            ctx,
            "unlit3d::lights",
            &mut self.lights_array,
            &self.lights_buf,
            &device,
            self.light_records.len().max(1) as u64,
        );
        // A cell is one element, and the clusterer stores each as a word per
        // slot: sizing by the words would ask for sixteen times the room.
        grow_array_to_fit(
            world,
            ctx,
            "unlit3d::light_cells",
            &mut self.light_cells_array,
            &self.light_cells_buf,
            &device,
            (self.light_clusters.cells.len() / CELL_WORDS).max(1) as u64,
        );
        grow_array_to_fit(
            world,
            ctx,
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
        let buffer = ctx
            .graph(world)
            .get()
            .get(&self.cluster_buf)
            .expect("cluster buffer exists")
            .clone();
        self.cluster_staging
            .write(&device, encoder, &buffer, 0, uniform.as_bytes());

        let buffer = ctx
            .graph(world)
            .get()
            .get(&self.lighting_buf)
            .expect("lighting buffer exists")
            .clone();
        self.lighting_staging
            .write(&device, encoder, &buffer, 0, self.lighting.as_bytes());
    }
}
