use super::*;

impl EguiIntegration {
    /// Build the UI integration around a caller-supplied `pipeline`,
    /// registering the UI's per-texture resources in `graph`.
    ///
    /// `pipeline` is the built-in lit pipeline the UI draws with, configured
    /// for the target the pass renders into — its color format, sample count
    /// and blend state. The caller builds it up front, so the UI does not
    /// compile a pipeline of its own.
    ///
    /// The global bind group — the camera, the frame globals and the view —
    /// is the caller's, and it is handed to [`Self::scene`] rather than kept
    /// here: the frame owns the uniforms and builds one group per camera and
    /// viewport its draws name, so a source can draw the same UI through two
    /// cameras without owning either group. Everything else the UI overlays is
    /// the caller's too: the integration owns only egui's textures and the
    /// geometry they are drawn with. Sharing the caller's graph — usually the
    /// one kept alongside the
    /// [`crate::render_attachments::RenderAttachments`] — means those
    /// resources live in the same ledger as the rest of the frame's, with the
    /// same dependency tracking.
    pub fn new(device: &wgpu::Device, pipeline: SpecializedLitPipeline) -> Self {
        Self {
            device: device.clone(),
            pipeline,
            textures: HashMap::new(),
            texture_options: HashMap::new(),
            samplers: Vec::new(),
            materials: Vec::new(),
            vertices: None,
            indices: None,
            vertex_node: None,
            index_node: None,
            instance: default_instance(device),
            vertex_capacity: 0,
            index_capacity: 0,
            frame_vertices: 0,
            vertex_staging: StagingBuffer::new(),
            index_staging: StagingBuffer::new(),
            draws: Vec::new(),
            packed_vertices: Vec::new(),
            packed_indices: Vec::new(),
        }
    }

    /// Apply `output`'s texture updates and upload its tessellated shapes,
    /// ready for [`Self::scene`].
    ///
    /// The vertex and index bytes change every frame, so they are staged
    /// through `encoder`: `scene`'s pass is recorded into the same encoder
    /// afterwards, and one submission carries the upload and the draw that
    /// reads it. Texture updates still go through `queue`.
    ///
    /// `screen` tells egui how to rasterise text and scales the tessellated points
    /// and their clip rectangles; see [`ScreenDescriptor`].
    ///
    /// This tessellates `output` with `ctx`, which is right when the context
    /// and the drawing share a world. A caller whose shapes were laid out
    /// elsewhere — in another world, by another context — uses
    /// [`Self::update_with`] and tessellates where the context lives.
    pub fn update(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
        screen: ScreenDescriptor,
    ) {
        let primitives = ctx.tessellate(output.shapes, screen.pixels_per_point);
        let textures = core::mem::take(&mut output.textures_delta);
        let mut sources = [UiFrame {
            source: Source::LOCAL,
            textures,
            primitives: &primitives,
        }];
        self.update_with(graph, queue, encoder, &mut sources, screen);
    }

    /// Apply texture updates and upload primitives from every source, ready
    /// for [`Self::scene`].
    ///
    /// Tessellation needs the [`egui::Context`] that produced the shapes — its
    /// font atlas and tessellation options — so a UI laid out in one world and
    /// drawn in another cannot be tessellated by the drawing side. Such a
    /// caller tessellates where each context lives and hands each result over
    /// with the [`Source`] it came from.
    ///
    /// A source is a namespace, not an identity: every context allocates its
    /// texture ids from zero, so the same id names different images in
    /// different sources. Keys carry the pair, and the ids a frame arrived
    /// with are the ids it is drawn by — nothing rewrites them.
    ///
    /// One call per frame: the integration packs one vertex and index buffer,
    /// and the two vertex streams meet at a single split point, so a second
    /// call would overwrite the first.
    ///
    /// Each `textures` delta is applied and then cleared, so the caller may
    /// drop it afterwards: egui panics when a delta is dropped unapplied.
    pub fn update_with(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        sources: &mut [UiFrame<'_>],
        screen: ScreenDescriptor,
    ) {
        for frame in sources.iter_mut() {
            self.apply_textures(graph, queue, frame.source, &frame.textures);
            // egui panics if a delta is dropped unapplied, so mark it handled.
            frame.textures.clear();
        }

        // The primitives of every source, flattened in place: the sources
        // keep their own tessellation and this walks it without collecting a
        // list of borrows first.
        let primitives = sources.iter().flat_map(|frame| {
            frame
                .primitives
                .iter()
                .map(move |primitive| (frame.source, primitive))
        });

        let counts = measure(primitives);
        if counts.vertices == 0 {
            self.draws.clear();
            return;
        }
        self.reserve(graph, counts);
        self.frame_vertices = counts.vertices;
        let primitives = sources.iter().flat_map(|frame| {
            frame
                .primitives
                .iter()
                .map(move |primitive| (frame.source, primitive))
        });
        self.upload_geometry(encoder, primitives, screen, counts);

        // Build every material the frame needs here, while the graph is still
        // mutably available: `scene` only reads it, so a material that does
        // not exist yet by then is one this frame cannot draw.
        //
        // The draws are walked by index because `build_material` needs `&mut
        // self`: collecting the keys first would allocate a `Vec` every frame
        // for a list that is already `self.draws`.
        for index in 0..self.draws.len() {
            let (texture, options) = {
                let draw = &self.draws[index];
                (draw.texture, draw.options)
            };
            self.build_material(graph, texture, options);
        }
    }

    /// Forget every graph node the integration registered, and its bookkeeping.
    ///
    /// The ids this drops are the last references the graph holds to the two
    /// geometry buffers, every texture, view, sampler and material, so the next
    /// [`ResourceGraph::maintain`] collects them — and, once the material nodes
    /// go, the views and textures they were the last holders of. No graph is
    /// needed here: giving a resource up is dropping its id.
    ///
    /// After this the integration is back to its freshly built state and may be
    /// reused against the same graph.
    pub fn release(&mut self) {
        self.textures.clear();
        self.texture_options.clear();
        self.materials.clear();
        self.samplers.clear();
        self.vertex_node = None;
        self.index_node = None;
        self.vertices = None;
        self.indices = None;
        self.instance = default_instance(&self.device);
        self.vertex_capacity = 0;
        self.index_capacity = 0;
        self.frame_vertices = 0;
        self.draws.clear();
    }

    /// Pack `primitives` into the vertex and index buffers, staging the bytes
    /// through `encoder`, and fill [`Self::draws`] with where each one landed.
    ///
    /// The primary and secondary streams go into two regions of the one buffer:
    /// wgpu binds one stride per slot, and the two streams differ.
    fn upload_geometry<'a>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        primitives: impl IntoIterator<Item = (Source, &'a egui::ClippedPrimitive)>,
        screen: ScreenDescriptor,
        counts: GeometryCounts,
    ) {
        // The scratch buffers are taken out of `self` for the duration, so the
        // draw list can be filled while they are written, then put back: a
        // steady UI reuses one allocation per stream rather than taking fresh
        // ones every frame.
        let mut vertices = core::mem::take(&mut self.packed_vertices);
        let mut indices = core::mem::take(&mut self.packed_indices);
        pack_geometry(
            primitives,
            screen,
            &self.texture_options,
            counts,
            &mut vertices,
            &mut indices,
            &mut self.draws,
        );

        let device = &self.device;
        let vertex_buffer = self.vertices.as_ref().expect("just reserved");
        let index_buffer = self.indices.as_ref().expect("just reserved");
        self.vertex_staging
            .write(device, encoder, vertex_buffer, 0, &vertices);
        self.index_staging
            .write(device, encoder, index_buffer, 0, &indices);
        self.packed_vertices = vertices;
        self.packed_indices = indices;
    }

    /// The frame most recently uploaded, as a scene.
    ///
    /// Every draw sets a scissor rectangle, and a scissor stays set for the
    /// rest of the pass, so record this after the draws it overlays.
    ///
    /// The materials the frame needs were built by [`Self::update`], so this
    /// only reads the graph — a scene can therefore be taken while the graph is
    /// shared with whatever else the frame is drawing.
    ///
    /// `global` is the caller's global bind group for the camera, the frame
    /// globals and the view this frame draws through; the UI binds it at
    /// [`GLOBAL_GROUP`] for every draw.
    pub fn scene(&self, graph: &ResourceGraph, global: &wgpu::BindGroup) -> Scene {
        if self.vertices.is_none() || self.indices.is_none() || self.draws.is_empty() {
            return Scene::new();
        }
        let mut scene = Scene::new();

        let (vertices, indices) = (
            self.vertices.as_ref().expect("checked"),
            self.indices.as_ref().expect("checked"),
        );
        // The position stream is the first half of the buffer, the interleaved
        // UVs and colors the second, and the two meet at the *frame's* vertex
        // count — the point `upload_geometry` packed at. The buffer usually has
        // room to spare, so splitting at `vertex_capacity` instead would bind
        // the second stream to bytes no vertex was written to.
        let positions_size = (self.frame_vertices * POSITION_STRIDE) as u64;
        let secondary_size = (self.frame_vertices * SECONDARY_STRIDE) as u64;
        let pipeline = &self.pipeline.pipeline;
        for draw in &self.draws {
            // Each draw carries its own scissor rectangle, so consecutive
            // draws sharing one are what the recorder's deduplication is for.
            let Some(material) = self.material(graph, draw.texture, draw.options) else {
                // A primitive whose texture was freed this frame has nothing
                // to sample, so it is skipped rather than bound to a stale view.
                continue;
            };
            // Both slots cover their whole stream, and the primitive's indices
            // already name its vertices pool-globally, so the draw needs no
            // base vertex: one range addresses the position, the UV and the
            // color together.
            // The trailing tier of the composite: the UI sits above the
            // depth-sorted pool whatever its depth, and a clipped draw here
            // is followed by nothing its rectangle could leak into.
            let entry = DrawEntry::new(pipeline, DrawRange::indexed(draw.indices.clone()))
                .with_order(DrawOrder::After)
                .with_bind_group(GLOBAL_GROUP, global)
                .with_bind_group(MATERIAL_GROUP, material)
                .with_vertex_buffer(INSTANCE_SLOT, &self.instance)
                .with_vertex_buffer_range(PRIMARY_SLOT, vertices, 0..positions_size)
                .with_vertex_buffer_range(
                    SECONDARY_SLOT,
                    vertices,
                    positions_size..positions_size + secondary_size,
                )
                .with_index_buffer(indices, wgpu::IndexFormat::Uint32)
                .with_scissor(draw.scissor);
            scene.draws.push(entry);
        }
        scene
    }

    /// Reallocate the buffers when they cannot hold `counts`, registering any
    /// new buffer in `graph`.
    fn reserve(&mut self, graph: &mut ResourceGraph, counts: GeometryCounts) {
        let GeometryCounts { vertices, indices } = counts;
        // Grow geometrically so a UI that grows over a few frames does not
        // reallocate every frame.
        if self.vertices.is_none() || self.vertex_capacity < vertices {
            self.vertex_capacity = vertices
                .max(self.vertex_capacity + self.vertex_capacity / 2)
                .max(1);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui::vertices"),
                size: (self.vertex_capacity * (POSITION_STRIDE + SECONDARY_STRIDE)) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vertex_node = Some(match self.vertex_node.take() {
                // A grow replaces the buffer in its existing node, so no stale
                // node is left strongly holding the old buffer.
                Some(node) => {
                    graph
                        .replace(&node, buffer.clone())
                        .expect("the vertex node is still registered");
                    node
                }
                None => graph.insert(buffer.clone(), None),
            });
            self.vertices = Some(buffer);
        }
        if self.indices.is_none() || self.index_capacity < indices {
            self.index_capacity = indices
                .max(self.index_capacity + self.index_capacity / 2)
                .max(1);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui::indices"),
                size: (self.index_capacity * size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.index_node = Some(match self.index_node.take() {
                Some(node) => {
                    graph
                        .replace(&node, buffer.clone())
                        .expect("the index node is still registered");
                    node
                }
                None => graph.insert(buffer.clone(), None),
            });
            self.indices = Some(buffer);
        }
    }
}
