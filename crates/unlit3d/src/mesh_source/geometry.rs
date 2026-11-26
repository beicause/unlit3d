use super::{
    ArrayVec, BufferPool, GpuMaterial, GpuMesh, MAX_VERTEX_BUFFERS, MeshEntry, MeshMetadata,
    MeshParts, MeshSource, MeshStorage, Rc, Rebuild, ResHandle, Resource, ResourceGraph, Scope,
    StorageTicket, TextureExt, TextureView, VertexBufferLayoutDesc, VertexLayout, Virtual,
};
use crate::mesh::{MeshDescriptor, MeshDraw, MeshError, MeshIndexing, MorphDeltas};

impl MeshSource {
    // -- mesh allocation -------------------------------------------------------

    /// Upload vertex and index data and return a [GpuMesh] handle.
    ///
    /// The source assumes no vertex layout: `vertex_buffers` lists exactly the
    /// buffers a draw binds, each tagged with the slot the pipeline's vertex
    /// state declares, so a mesh may carry any combination of attributes in
    /// any format. The pipeline specializes on the layout.
    ///
    /// The mesh's [`Aabb`](crate::bounds::Aabb) is recorded in the source's
    /// mesh-metadata array, which the next built scene uploads.
    ///
    /// [`MeshSource::allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh) is the helper that builds the
    /// compressed layout the built-in lit shader expects.
    ///
    /// # Errors
    ///
    /// If the descriptor does not describe a mesh that can be drawn: see
    /// [`MeshDescriptor::validate`](crate::mesh::MeshDescriptor::validate). Nothing is uploaded
    /// in that case, so a refused description leaves no parts in the graph.
    pub fn allocate_mesh(
        &mut self,
        world: &Scope<'_>,
        desc: MeshDescriptor,
    ) -> Result<GpuMesh, MeshError> {
        desc.validate()?;

        let MeshDescriptor {
            vertex_buffers,
            indexing,
            aabb,
            bind_group,
            skin,
            morph_deltas,
        } = desc;

        // Nothing is registered before the description is accepted, so a
        // refused mesh leaves no parts behind in the graph.
        let mut dependencies = ArrayVec::<ResHandle<wgpu::Buffer>, MAX_VERTEX_BUFFERS>::new();
        let mut vertex_slots =
            ArrayVec::<(u32, ResHandle<wgpu::Buffer>), MAX_VERTEX_BUFFERS>::new();
        let mut layouts = Vec::with_capacity(vertex_buffers.len());
        for buffer in vertex_buffers {
            // The mesh's virtual root is built from it, so the buffer lives
            // exactly as long as the root does.
            let id = self.context.graph(world).get().insert(buffer.buffer, None);
            vertex_slots.push((buffer.slot, id.clone()));
            dependencies.push(id);
            // The layout is owned by the mesh so a family can key on it
            // without reading the description again.
            layouts.push((
                buffer.slot,
                VertexBufferLayoutDesc {
                    array_stride: buffer.array_stride,
                    step_mode: buffer.step_mode,
                    attributes: buffer.attributes,
                },
            ));
        }
        let vertex_layout = VertexLayout::new(layouts);

        // A mesh that owns its buffers whole draws from the start of them, and
        // its indices name its own vertices, so no base vertex is needed.
        let (draw, index_buffer) = match indexing {
            MeshIndexing::NonIndexed { vertex_count } => (
                MeshDraw::Vertices {
                    vertices: 0..vertex_count,
                },
                None,
            ),
            MeshIndexing::Indexed {
                index_buffer,
                format,
                count,
            } => {
                let id = self.context.graph(world).get().insert(index_buffer, None);
                dependencies.push(id.clone());
                (
                    MeshDraw::Indexed {
                        indices: 0..count,
                        base_vertex: 0,
                    },
                    Some((id, format)),
                )
            }
        };

        let metadata = MeshMetadata {
            aabb_center: aabb.center,
            aabb_half_extents: aabb.half_extents,
            ..Default::default()
        };
        let entry = self.allocate_entry(world, metadata, morph_deltas, 0);

        // A bind group the caller supplied — a custom pipeline's per-mesh data
        // — hangs under the mesh's root, so giving the mesh up frees it with
        // the rest of its parts.
        let bind_group_id =
            bind_group.map(|bind_group| self.context.graph(world).get().insert(bind_group, None));

        let storage = MeshStorage {
            inbox: self.inbox.clone(),
            ticket: StorageTicket {
                vertex: None,
                index: None,
                morph_deltas: entry.morph_deltas,
                metadata_index: entry.metadata_index,
            },
            metadata_index: entry.metadata_index,
        };
        let parts = self.mesh_parts(
            world,
            vertex_slots,
            index_buffer,
            bind_group_id,
            storage,
            &dependencies,
        );

        Ok(GpuMesh {
            parts,
            vertex_layout,
            draw,
            aabb,
            morph_targets: entry.morph_targets,
            skin,
        })
    }

    /// Build the parts a mesh handle shares, under a root that owns them.
    ///
    /// The virtual root is the one node that keeps the parts alive: each part is
    /// registered on its own and the root depends on all of them, so dropping
    /// the last handle to the root leaves the parts nothing else holds for the
    /// next `maintain` to collect. Depending on the parts is also what makes a
    /// replaced part mark the root dirty.
    ///
    /// `dependencies` names the nodes the caller registered for the parts that
    /// are resources of their own. A pooled mesh names none of them: its buffers
    /// are the pools', which outlive it and are deliberately not dependencies —
    /// a pooled resource changes when its pool grows, and a dependency would
    /// rebuild every group of every mesh sharing the pool for nothing.
    pub(crate) fn mesh_parts(
        &mut self,
        world: &Scope<'_>,
        vertex_buffers: ArrayVec<(u32, ResHandle<wgpu::Buffer>), MAX_VERTEX_BUFFERS>,
        index_buffer: Option<(ResHandle<wgpu::Buffer>, wgpu::IndexFormat)>,
        bind_group_id: Option<ResHandle<wgpu::BindGroup>>,
        storage: MeshStorage,
        dependencies: &[ResHandle<wgpu::Buffer>],
    ) -> Rc<MeshParts> {
        let mut graph_holder = self.context.graph(world);
        let graph = graph_holder.get();
        let root = graph.insert(Virtual, None);
        for id in dependencies {
            graph.add_dependency(&root, id);
        }
        if let Some(id) = &bind_group_id {
            graph.add_dependency(&root, id);
        }
        drop(graph_holder);
        Rc::new(MeshParts {
            root,
            vertex_buffers,
            index_buffer,
            bind_group_id,
            storage,
        })
    }

    /// Record the metadata entry a mesh owns and pool its morph displacements.
    ///
    /// The entry is owned whether or not the pipeline reads it: a draw that
    /// reads no metadata simply leaves the index unused. A slot a removed mesh
    /// held is reused, so the array stays as dense as the meshes that are still
    /// alive.
    ///
    /// The vertex offset is part of the entry, so the shader reaches it through
    /// the same index the decode parameters come from. It maps
    /// `@builtin(vertex_index)` back to this mesh's own vertex ordinal for the
    /// morph displacements.
    ///
    /// The displacements are geometry the mesh owns, but they go into the
    /// frame-wide pool rather than a resource of their own, so what comes back
    /// is the range the mesh holds and the count its addressing names. The
    /// joint matrices and morph weights are *not* here: they are per-instance
    /// pose state, so two instances of one mesh can deform differently.
    pub(crate) fn allocate_entry(
        &mut self,
        world: &Scope<'_>,
        mut metadata: MeshMetadata,
        morph_deltas: Option<MorphDeltas>,
        vertex_offset: u32,
    ) -> MeshEntry {
        metadata.vertex_offset = vertex_offset;
        let (morph_targets, morph_deltas_range) = match morph_deltas {
            Some(morph) => {
                let (offset, allocation) = self
                    .arrays
                    .allocate_morph_deltas(&self.device(world), &morph);
                (morph.target_count, Some((offset, allocation)))
            }
            None => (0, None),
        };
        let (morph_deltas_offset, morph_deltas) = match morph_deltas_range {
            Some((offset, allocation)) => (offset, Some(allocation)),
            None => (0, None),
        };
        metadata.morph_count = morph_targets;
        metadata.morph_deltas_offset = morph_deltas_offset;
        let metadata_index = self.arrays.allocate_metadata(metadata);
        MeshEntry {
            metadata_index,
            morph_deltas,
            morph_targets,
        }
    }

    // -- materials -------------------------------------------------------------

    /// Insert `texture` into the resource graph and return a pair of ids: the
    /// texture itself and a default view of it.
    ///
    /// The view is recorded as depending on the texture, so replacing the
    /// texture marks every material built from the view dirty. The caller
    /// keeps ownership of the texture only until this call; afterwards the
    /// graph holds it.
    pub fn register_texture_and_default_view(
        &mut self,
        world: &Scope<'_>,
        texture: wgpu::Texture,
    ) -> (ResHandle<wgpu::Texture>, ResHandle<TextureView>) {
        let mut graph_holder = self.context.graph(world);
        let graph = graph_holder.get();
        let texture_id = graph.insert(texture, None);
        let view = TextureExt::create_view(
            graph.get(&texture_id).expect("texture exists"),
            &wgpu::TextureViewDescriptor::default(),
        );
        let view_id = graph.insert(view, None);
        graph.add_dependency(&view_id, &texture_id);
        (texture_id, view_id)
    }

    /// Create a sampler with `descriptor` — or the default one when `None` —
    /// insert it into the resource graph and return its id.
    pub fn register_sampler(
        &mut self,
        world: &Scope<'_>,
        descriptor: Option<wgpu::SamplerDescriptor<'_>>,
    ) -> ResHandle<wgpu::Sampler> {
        let sampler = self
            .device(world)
            .create_sampler(&descriptor.unwrap_or_default());
        self.context.graph(world).get().insert(sampler, None)
    }

    /// Build a material bind group with `build` and return its [GpuMaterial]
    /// handle.
    ///
    /// Only the bind group is created here: the resources it reads are the
    /// caller's, already in the resource graph and named in `dependencies` so
    /// replacing one marks the group dirty. `build` owns the layout the group is
    /// built against and is called again by [`Self::maintain`] whenever a
    /// dependency is replaced, so it has to read whatever it binds back out of
    /// the graph rather than capture a handle. The layout is the material layout
    /// of the pipeline the material is for —
    /// [`LitVariant::bind_group_layouts`](unlit_wgpu::pipeline::LitVariant::bind_group_layouts)
    /// for the built-in shader, or the one a custom pipeline registered.
    ///
    /// # Panics
    ///
    /// If a resource `build` reads is not in the graph.
    pub fn allocate_material(
        &mut self,
        world: &Scope<'_>,
        build: impl Fn(&ResourceGraph) -> wgpu::BindGroup + 'static,
        dependencies: impl IntoIterator<Item = ResHandle>,
    ) -> GpuMaterial {
        // The first build is eager: the node has to hold a bind group from the
        // start, and only the rebuilds that follow are deferred to `maintain`.
        let bind_group = {
            let mut graph_holder = self.context.graph(world);
            let graph = graph_holder.get();
            build(graph)
        };
        let mut graph_holder = self.context.graph(world);
        let graph = graph_holder.get();
        let rebuild = Rebuild::new(move |graph| Resource::BindGroup(build(graph)));
        let bind_group_id = graph.insert(bind_group, Some(rebuild));
        for dependency in dependencies {
            graph.add_dependency(&bind_group_id, &dependency);
        }

        GpuMaterial { bind_group_id }
    }

    /// The graph node of the vertex pool's buffer for `layout`, creating it if
    /// the pool has never been asked for the layout before.
    ///
    /// The node is strong: the source owns the pool, no mesh does. See
    /// [`MeshSource::sync_pool_node`] for why nothing may depend on it.
    pub(crate) fn vertex_node(
        &mut self,
        world: &Scope<'_>,
        layout: &VertexBufferLayoutDesc,
    ) -> ResHandle<wgpu::Buffer> {
        if let Some(id) = self.pools.vertex_pool_ids.get(layout) {
            return id.clone();
        }
        let buffer = self
            .pools
            .vertex_pool
            .buffer(layout)
            .expect("the layout was allocated a buffer")
            .clone();
        let id = self.context.graph(world).get().insert(buffer, None);
        self.pools
            .vertex_pool_ids
            .insert(layout.clone(), id.clone());
        id
    }

    /// Point the stream node `id` at the vertex pool's buffer for `layout`.
    ///
    /// The pool hands out a new buffer when it grows, so the node has to
    /// follow it.
    pub(crate) fn sync_vertex_node(
        &mut self,
        world: &Scope<'_>,
        id: &ResHandle<wgpu::Buffer>,
        layout: &VertexBufferLayoutDesc,
    ) {
        let buffer = self
            .pools
            .vertex_pool
            .buffer(layout)
            .expect("the layout has a buffer");
        let mut graph_holder = self.context.graph(world);
        let graph = graph_holder.get();
        if graph.get(id) != Some(buffer) {
            graph
                .replace(id, buffer.clone())
                .expect("a stream node exists");
        }
    }

    /// Point the graph node `id` at the buffer `pool` currently hands out.
    ///
    /// A pool grows by creating a new buffer, and a mesh keeps the pool's node
    /// id rather than its buffer, so the node has to follow the buffer. The
    /// check is the pool's own handle compared against what the node holds: no
    /// counter to keep in step, and repeating the call is free.
    ///
    /// Nothing depends on a pool node — the built-in variants bind no
    /// per-mesh resource at all — so replacing one marks nothing else dirty. A
    /// dependency added onto a pool buffer makes every grow rebuild it, which
    /// is why there must not be one.
    pub(crate) fn sync_pool_node(
        pool: &BufferPool,
        id: &ResHandle<wgpu::Buffer>,
        graph: &mut ResourceGraph,
    ) {
        if graph.get(id) != Some(pool.buffer()) {
            graph
                .replace(id, pool.buffer().clone())
                .expect("a pool node exists");
        }
    }
}
