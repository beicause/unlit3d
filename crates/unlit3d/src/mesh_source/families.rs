use super::*;

impl MeshSource {
    // -- pipeline registration -------------------------------------------------

    /// Register a pipeline family for the key type `K`.
    ///
    /// This is the only way a pipeline enters the source, for the built-in
    /// unlit shader and a caller own alike. The key type is the family
    /// identity: entities draw with it when they carry a
    /// [GpuRenderPipeline](crate::components::GpuRenderPipeline) of that type, and registering a
    /// second family for the same key type is a programming error.
    ///
    /// A family compiles nothing on registration: its concrete pipelines are
    /// built lazily, the first time a draw resolves a variant, and are
    /// appended to the source pipeline list in resolution order.
    ///
    /// [`MeshSource::register_lit_family`](crate::mesh_source::MeshSource::register_lit_family) is the built-in lit family; a
    /// caller registers their own by supplying the key whose
    /// [RenderPipelineKey::variant] resolves a draw into a variant, the
    /// [RenderPipelineFactory] that describes the compiled result, and the
    /// [InstanceData] that writes its per-instance records. A family with no
    /// per-instance state passes `()`.
    ///
    /// # Panics
    ///
    /// If a family is already registered for `K`.
    pub fn register_family<K, F, I>(&mut self, world: &Scope<'_>, factory: F, instances: I)
    where
        K: RenderPipelineKey + 'static,
        F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>
            + 'static,
        I: InstanceData,
    {
        let key = TypeId::of::<K>();
        assert!(
            !self.family_ids.contains_key(&key),
            "a pipeline family is already registered for this key type"
        );
        let id = FamilyId::new(self.families.len() as u32);
        self.families.push(Box::new(Family::<K, F, I>::new(
            &self.device(world),
            factory,
            instances,
        )));
        self.family_ids.insert(key, id);
    }
    /// Pack and upload every family's per-instance data for this frame.
    ///
    /// Each family owns one instance stream. The records are written in
    /// visible order, so the entry an instance index names is the entry whose
    /// draw range covers that index.
    pub(super) fn pack_family_instances(
        &mut self,
        world: &Scope<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let device = self.device(world);
        // Split the borrows: each entry is written while its family's stream
        // grows, and while the two frame arrays a family may append to grow.
        let Self {
            families,
            caches,
            arrays,
            ..
        } = self;
        let visible_cache = &mut caches.visible_cache;
        let packed_joints = &mut arrays.packed_joints;
        let packed_morph_weights = &mut arrays.packed_morph_weights;
        packed_joints.clear();
        packed_morph_weights.clear();
        for family in families.iter_mut() {
            family.begin_instances();
        }
        for entry in visible_cache.iter_mut() {
            let mesh = &entry.mesh;
            let mut context = InstanceContext::new(
                *world,
                mesh.entity,
                mesh.world_from_local,
                packed_joints,
                packed_morph_weights,
            );
            entry.instance_index = families[entry.family.as_usize()].push_instance(&mut context);
        }
        for family in families.iter_mut() {
            family.upload_instances(&device, encoder);
        }
    }

    /// Collect entities that pass frustum culling, then sort them for drawing.
    ///
    /// The pass itself lives in [crate::scene]; this only unpacks the source's
    /// caches and the closure that registers a newly resolved pipeline, so a
    /// family never borrows the whole source.
    ///
    /// `surface` is the render target this frame draws into; it is threaded
    /// into every entity's pipeline resolution, so each entry's `pipeline_id`
    /// is a concrete pipeline valid for that target.
    pub(super) fn collect_and_sort_visible(
        &mut self,
        world: &Scope<'_>,
        camera: &Camera,
        surface: SurfaceKey,
    ) {
        let resources = self.global_resources();
        let device = self.device(world);

        // Split the borrows: the families push into the pipeline list through
        // the register closure, while the caches are disjoint fields.
        let (families, meshes, visible) = (
            &mut self.families,
            &mut self.caches.visible_meshes_cache,
            &mut self.caches.visible_cache,
        );
        let pipelines = &mut self.pipelines;
        let mut register = |desc| register_concrete(pipelines, desc);

        collect_and_sort_visible(
            SceneFrame {
                families,
                meshes,
                visible,
                register: &mut register,
            },
            *world,
            camera,
            surface,
            &device,
            resources,
        );
    }

    /// Assemble the draws of the frame's visible set into `self.scene`.
    ///
    /// The handles every draw names — the bind groups, the vertex and index
    /// buffers, the pipeline — are cloned out of the resource graph into reused
    /// scratch tables first, so assembling touches the graph once per distinct
    /// draw state and the assembled scene owns everything it names.
    ///
    /// A scene usually draws many entities per mesh, and every one of them
    /// names the same handles. The visible set therefore carries a
    /// [`DrawHandlesKey`] by value rather than the handles themselves, and the
    /// expensive part — reading the mesh component and cloning its buffers out
    /// of the graph — happens once per distinct key instead of once per entity.
    pub(super) fn assemble_frame(
        &mut self,
        world: &Scope<'_>,
        scene: &mut Scene,
        uniforms: &FrameUniforms,
    ) {
        let mut handles = std::mem::take(&mut self.caches.entry_handle_cache);
        handles.clear();
        {
            profiling::scope!("mesh_source.assemble.handles");
            let mut graph_holder = self.context.graph(world);
            let graph = graph_holder.get();
            for entry in &self.caches.visible_cache {
                let key = entry.handles_key.clone();
                if handles.contains_key(&key) {
                    continue;
                }

                let mut mesh_row = world
                    .query_one::<&GpuMesh>(entry.mesh.entity)
                    .unwrap_or_else(|_| {
                        panic!("visible entity has GpuMesh: {:?}", entry.mesh.entity)
                    });
                let mesh = mesh_row.get();

                let mesh_bg = mesh
                    .parts
                    .bind_group_id
                    .as_ref()
                    .map(|id| graph.get(id).expect("mesh bind group exists").clone());

                let material_bg = key
                    .material
                    .as_ref()
                    .map(|id| graph.get(id).expect("material bind group exists").clone());

                let mut vertex_buffers = ArrayVec::new();
                for (slot, buffer) in &mesh.parts.vertex_buffers {
                    let buffer = graph
                        .get(buffer)
                        .expect("mesh vertex buffer exists")
                        .clone();
                    // A mesh binds its vertex buffers whole; the draw's range
                    // is what picks the mesh's slice out of the pool.
                    vertex_buffers.push((*slot, buffer.clone(), 0..buffer.size()));
                }

                let index_buffer = mesh.parts.index_buffer.as_ref().map(|(buffer, format)| {
                    (
                        graph.get(buffer).expect("mesh index buffer exists").clone(),
                        *format,
                    )
                });

                handles.insert(
                    key,
                    EntryHandles {
                        mesh_bg,
                        material_bg,
                        vertex_buffers,
                        index_buffer,
                    },
                );
            }
        }

        // Every registered pipeline's handle and global bind group, indexed the
        // same way [`MeshSource::pipelines`] is. A pipeline that binds no
        // global group carries `None`. The list is reused between frames, so a
        // steady scene allocates nothing.
        //
        // The group binds the camera and view buffers themselves, so it is the
        // frame's group for the pair this draw names: a pipeline drawn through
        // another camera, or into another viewport, gets a second group built
        // from the same recipe.
        let mut pipeline_handles = std::mem::take(&mut self.caches.pipeline_handle_cache);
        pipeline_handles.clear();
        {
            profiling::scope!("mesh_source.assemble.globals");
            let key = uniforms.key();
            let frame = uniforms.frame;
            let mut graph_holder = self.context.graph(world);
            let graph = graph_holder.get();
            let device = self.device(world);
            pipeline_handles.extend(self.pipelines.iter_mut().map(|registered| {
                let global = registered.global.as_mut().map(|global| {
                    let recipe = global.recipe.clone();
                    let group = global.groups.group(graph, key, frame, |graph| {
                        recipe.insert(&device, graph, uniforms)
                    });
                    // A pair this pipeline no longer draws -- a camera that is
                    // gone, a viewport no pass uses -- gives its group back, so
                    // a world that changes cameras holds one group rather than
                    // one per camera it ever had.
                    global.groups.prune(frame);
                    graph.get(&group).expect("global group exists").clone()
                });
                RenderPipelineHandles {
                    pipeline: registered.pipeline.clone(),
                    global,
                }
            }));
        }

        // One instance-stream binding per family, indexed the same way
        // [`MeshSource::families`] is. A family with no instance stream —
        // stride zero — carries `None`, and its draws bind no instance slot.
        let mut family_instances = std::mem::take(&mut self.caches.family_instance_cache);
        family_instances.clear();
        family_instances.extend(self.families.iter().map(|family| family.instance_binding()));

        // The driver cleared the scene for this (pass, source) pair, so the
        // draws below are the whole of it.
        profiling::scope!("mesh_source.assemble.draws");
        assemble_scene(
            scene,
            &self.caches.visible_cache,
            &pipeline_handles,
            &handles,
            &family_instances,
        );

        self.caches.pipeline_handle_cache = pipeline_handles;
        self.caches.entry_handle_cache = handles;
        self.caches.family_instance_cache = family_instances;
    }
}
