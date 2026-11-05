use super::*;

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
    /// # Panics
    ///
    /// If `count` is zero for a non-empty draw, or if the index format does
    /// not match the packed data.
    pub fn allocate_mesh(&mut self, world: &Scope<'_>, desc: MeshDesc) -> GpuMesh {
        let metadata = MeshMetadata {
            aabb_center: desc.aabb.center,
            aabb_half_extents: desc.aabb.half_extents,
            ..Default::default()
        };
        // The raw API says nothing about the mesh's channels, so the mesh is
        // not known to carry joints; a caller that uploads a skinned layout
        // goes through `allocate_lit_mesh`, which does know. It owns its
        // buffers whole, so it is drawn from vertex zero and morphs nothing.
        self.allocate_mesh_with_metadata(world, desc, metadata, false, 0)
    }

    /// Upload a mesh together with the full metadata entry it owns.
    ///
    /// The entry is appended to the CPU-side array and reaches the GPU on the
    /// next built scene; the returned handle names its index. `vertex_offset`
    /// is the draw addressing the entry carries; the morph count is the one the
    /// mesh's own displacements declare.
    ///
    /// `skinned` records whether the mesh's position stream carries joints, so
    /// a skinned draw can be required to name the pose entity it deforms by.
    pub(crate) fn allocate_mesh_with_metadata(
        &mut self,
        world: &Scope<'_>,
        desc: MeshDesc,
        mut metadata: MeshMetadata,
        skinned: bool,
        vertex_offset: u32,
    ) -> GpuMesh {
        let MeshDesc {
            vertex_buffers,
            index_buffer,
            count,
            indexed,
            aabb,
            bind_group,
            morph_deltas,
        } = desc;

        // The vertex offset is part of the entry the mesh owns, so the shader
        // reaches it through the same index the decode parameters come from.
        // It maps `@builtin(vertex_index)` back to this mesh's own vertex
        // ordinal for the morph displacements.
        metadata.vertex_offset = vertex_offset;

        // The parts, capped like the description they come from: a mesh cannot
        // have more vertex buffers than a pass can bind.
        let mut buffers = ArrayVec::<ResHandle<wgpu::Buffer>, MAX_VERTEX_BUFFERS>::new();
        let mut vertex_slots =
            ArrayVec::<(u32, ResHandle<wgpu::Buffer>), MAX_VERTEX_BUFFERS>::new();
        let mut layouts = Vec::with_capacity(vertex_buffers.len());
        for desc in vertex_buffers {
            // The mesh's virtual root is built from it, so the buffer lives
            // exactly as long as the root does.
            let id = self.context.graph(world).get().insert(desc.buffer, None);
            vertex_slots.push((desc.slot, id.clone()));
            // The layout is owned by the mesh so a family can key on it
            // without reading the description again.
            layouts.push((
                desc.slot,
                VertexBufferLayoutDesc {
                    array_stride: desc.array_stride,
                    step_mode: desc.step_mode,
                    attributes: desc.attributes,
                },
            ));
            buffers.push(id);
        }
        let vertex_layout = VertexLayout::new(layouts);

        let index_buffer = index_buffer.map(|(buffer, format)| {
            // Weak for the same reason as the vertex buffers.
            let id = self.context.graph(world).get().insert(buffer, None);
            (id, format)
        });

        // The morph displacements are this mesh's geometry, but they go into
        // the frame-wide pool rather than a resource of their own, so the mesh
        // holds an element range like it holds a vertex range. The range's
        // offset is what its metadata entry names.
        //
        // The joint matrices and the morph weights are *not* here. They are
        // per-instance pose state: the frame's global group binds them, so two
        // instances of one mesh can deform differently.
        let (morph_target_count, morph_deltas_offset, morph_deltas_allocation) = match morph_deltas
        {
            Some(morph) => {
                let range = self
                    .morph_deltas_pool
                    .allocate(&self.device(world), morph.deltas.len() as u32)
                    .expect("the morph-displacement pool grows");
                self.morph_deltas_pool.write(range, morph.deltas.as_bytes());
                self.morph_deltas_dirty = true;
                (
                    morph.target_count,
                    range.allocation.offset,
                    Some(range.allocation),
                )
            }
            None => (0, 0, None),
        };

        // A bind group the caller supplied — a custom pipeline's per-mesh data
        // — hangs under the mesh's root, so giving the mesh up frees it with
        // the rest of its parts. The pools a mesh's own data lives in
        // are deliberately not dependencies: a pooled resource changes when its
        // pool grows, and a dependency would rebuild every group of every mesh
        // sharing the pool for nothing.
        let bind_group_id =
            bind_group.map(|bind_group| self.context.graph(world).get().insert(bind_group, None));

        // The entry is owned whether or not the pipeline reads it: a draw that
        // reads no metadata simply leaves the index unused. A slot a removed
        // mesh held is reused, so the array stays as dense as the meshes that
        // are still alive.
        //
        // The morph count and displacement offset the addressing carries are
        // this mesh's own: no other values describe the displacements it
        // pooled.
        metadata.morph_count = morph_target_count;
        metadata.morph_deltas_offset = morph_deltas_offset;
        let metadata_index = match self.free_metadata.pop() {
            Some(index) => {
                self.metadata[index as usize] = metadata;
                index
            }
            None => {
                let index = self.metadata.len() as u32;
                self.metadata.push(metadata);
                index
            }
        };
        self.metadata_dirty = true;

        // The virtual root below is the one node that keeps the parts alive:
        // each part is registered on its own and the root is built from all of
        // them, so dropping the last id to the root leaves the parts nothing
        // else holds for the next `maintain` to collect. Depending on the
        // parts is also what makes a replaced part mark the root dirty.
        let mut graph_holder = self.context.graph(world);
        let graph = graph_holder.get();
        let root = graph.insert(Virtual, None);
        for id in &buffers {
            graph.add_dependency(&root, id);
        }
        if let Some((id, _format)) = &index_buffer {
            graph.add_dependency(&root, id);
        }
        if let Some(id) = &bind_group_id {
            graph.add_dependency(&root, id);
        }
        drop(graph_holder);

        GpuMesh {
            parts: Rc::new(MeshParts {
                root,
                vertex_buffers: vertex_slots,
                index_buffer,
                bind_group_id,
                vertex_allocation: None,
                index_allocation: None,
                morph_deltas_allocation,
                metadata_index,
            }),
            vertex_layout,
            count,
            first: 0,
            base_vertex: 0,
            indexed,
            aabb,
            morph_targets: morph_target_count,
            skinned,
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

    /// Free `mesh` and every resource it owns, and drop its mesh-metadata
    /// entry.
    ///
    /// The whole mesh hangs off its virtual root: the parts are held by the
    /// root and by nothing else, so dropping `mesh` — the last holder of the
    /// root — makes them collectable, and the next
    /// [`Self::maintain`](MeshSource::maintain) frees the root, its bind group
    /// and the per-mesh uniform that only fed that bind group. A mesh uploaded
    /// through [`allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh) keeps
    /// its vertices and indices in pools the source shares between meshes;
    /// those allocations are handed back here, and the pool buffers outlive the
    /// mesh. A mesh uploaded with [`allocate_mesh`](MeshSource::allocate_mesh)
    /// owns its buffers, and they die with it.
    ///
    /// Its metadata slot is freed and reused by a mesh allocated later, so
    /// removing meshes does not grow the array a long-lived source uploads.
    /// The next built scene uploads the array the shader reads. Removing a mesh
    /// does not shrink the metadata buffer: it grows to the largest array it
    /// has ever held and stays there.
    ///
    /// Any other [`GpuMesh`] value sharing this mesh's parts keeps it alive:
    /// the parts are reference-counted, so a caller that cloned the handle
    /// before calling this still reads a live mesh.
    pub fn remove_mesh(&mut self, mesh: GpuMesh) {
        if let Some(vertex) = mesh.parts.vertex_allocation {
            self.vertex_pool.release(vertex);
        }
        if let Some(index) = mesh.parts.index_allocation {
            self.index_pool.release(index);
        }
        if let Some(deltas) = mesh.parts.morph_deltas_allocation {
            self.morph_deltas_pool.release(deltas);
        }

        let metadata_index = mesh.parts.metadata_index;
        let emptied = self.metadata.get_mut(metadata_index as usize);
        if let Some(entry) = emptied {
            *entry = MeshMetadata::default();
            self.free_metadata.push(metadata_index);
            self.metadata_dirty = true;
        }
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
        if let Some(id) = self.vertex_pool_ids.get(layout) {
            return id.clone();
        }
        let buffer = self
            .vertex_pool
            .buffer(layout)
            .expect("the layout was allocated a buffer")
            .clone();
        let id = self.context.graph(world).get().insert(buffer, None);
        self.vertex_pool_ids.insert(layout.clone(), id.clone());
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
