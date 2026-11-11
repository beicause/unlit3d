//! Mesh allocation and family registration for the built-in lit pipeline.

use super::*;

/// Mesh allocation and family registration for the built-in lit pipeline.
///
/// These are ordinary inherent methods on [`MeshSource`]: a caller's own family
/// uses [`MeshSource::register_family`] and [`MeshSource::allocate_mesh`] the
/// same way these use them, and the module gathers them next to the shader
/// they serve.
impl MeshSource {
    /// Register the built-in lit shader as a family.
    ///
    /// The family specializes each entity's options on the frame's render
    /// target and the mesh's vertex layout: two draws that agree on both share
    /// one compiled pipeline, and a draw whose mesh layout implies different
    /// channels compiles its own. The options are the entity's: a mesh or
    /// material is built against the [LitOptions] it will be drawn with,
    /// through [`MeshSource::allocate_lit_mesh`] and
    /// [`MeshSource::allocate_lit_material`]. Those take the options
    /// rather than the key, so any pipeline whose variant reads the same
    /// channels can draw the mesh.
    ///
    /// This compiles nothing; like any family, its first concrete pipeline is
    /// built when an entity that uses it is first drawn.
    ///
    /// The source clusters and uploads every frame's
    /// [LightSource](crate::light::LightSource)s whether or not this family is
    /// registered: registration says which family a draw resolves with, not
    /// which frames draw, so the lights have to be there before the first lit
    /// draw. A source that never draws a lit variant pays that upload for
    /// nothing.
    ///
    /// # Panics
    ///
    /// If the lit family is already registered.
    pub fn register_lit_family(&mut self, world: &Scope<'_>) {
        self.register_family::<LitPipelineKey, _, _>(world, LitFactory, LitInstance);
    }

    /// Upload raw mesh channels in the layout the built-in lit shader
    /// expects, and return a [GpuMesh] handle.
    ///
    /// Positions and UVs are compressed to the compact vertex formats the
    /// shader decodes, packed into a position buffer and an interleaved
    /// UV-and-colour buffer, and bound with the mesh-metadata bind group the
    /// shader reads its decode parameters from. Colours are already stored in
    /// the width they are uploaded at, so they are copied through unchanged.
    ///
    /// A skinned mesh's joint indices and weights are packed into the *same*
    /// stream as its positions, right after them. A morphed mesh's per-vertex
    /// displacements go into a buffer of its own.
    ///
    /// Neither mesh carries a pose: the joint matrices and morph weights a
    /// frame deforms by are per-instance state the CPU writes, so they live in
    /// the [`SkinPose`](crate::components::SkinPose) and
    /// [`MorphWeights`](crate::components::MorphWeights) components the mesh's
    /// entity names through a [`SkinBinding`](crate::components::SkinBinding)
    /// and a [`MorphBinding`](crate::components::MorphBinding). The frame packs
    /// them once per frame, so animating a mesh costs a component write and no
    /// re-upload.
    ///
    /// This is a convenience over [`MeshSource::allocate_mesh`]: it builds the
    /// same [`MeshDesc`] a caller could build by hand, and shares the
    /// compression in [unlit_wgpu::mesh] with anyone else who wants it.
    ///
    /// Which channels are packed follows the slices the caller passes: a
    /// channel with no slice is left out, and the mesh's vertex layout records
    /// exactly the channels that were packed, so the pipeline it is later
    /// drawn with reads back the same shape.
    ///
    /// # Panics
    ///
    /// If the input slices are empty or of mismatched length (see the
    /// compressors in [unlit_wgpu::mesh]); if only one of
    /// [`LitMeshDesc::joints`] and [`LitMeshDesc::weights`] is `Some`; or
    /// if [`LitMeshDesc::morph_deltas`] is present but does not hold three
    /// components per vertex and target.
    pub fn allocate_lit_mesh(
        &mut self,
        world: &Scope<'_>,
        options: &Hashed<LitOptions>,
        desc: LitMeshDesc<'_>,
    ) -> GpuMesh {
        let device = self.device(world);
        let queue = self.queue(world);

        let LitMeshDesc {
            positions,
            uvs,
            colors,
            normals,
            indices,
            joints,
            weights,
            morph_deltas,
        } = desc;

        // Which channels the mesh carries is the caller's answer, not the
        // key's: the options are pure policy, so the streams are derived from
        // the slices the caller actually passed. A channel with no slice is
        // left out, and the pipeline the mesh is later drawn with reads that
        // same layout back off the mesh.
        //
        // The streams are always packed in their compressed encodings: a
        // caller that wants to upload full-precision vertices builds its own
        // buffers and its own variant instead, the way the UI path does.
        assert_eq!(
            joints.is_some(),
            weights.is_some(),
            "a mesh's joints and weights come as a pair, so one cannot be present without the other"
        );
        let mut secondary = SecondaryFlags::empty();
        if uvs.is_some() {
            secondary |= SecondaryFlags::UV;
        }
        if colors.is_some() {
            secondary |= SecondaryFlags::COLOR;
        }
        if normals.is_some() {
            secondary |= SecondaryFlags::NORMAL;
        }
        let channels = LitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: joints.is_some() && weights.is_some(),
            },
            secondary,
        };
        // The variant is built only to read the streams and layouts the
        // channels imply. Its surface, material and strip index format play no
        // part in any of those, so they are left at placeholders.
        let variant = LitVariant {
            options: options.clone(),
            surface: SurfaceKey {
                color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth_stencil_format: None,
                sample_count: 1,
            },
            channels,
            base_color_texture: false,
            morph: morph_deltas.is_some(),
            strip_index_format: None,
        };
        let position_stream = variant.position_stream();
        let secondary_stream = variant.secondary_stream();

        // The joint pair belongs to the position stream, so a skinned mesh's
        // stream is wider by it and no separate buffer exists. A variant that
        // deforms by joints reads them from that stream, so it needs the
        // caller's joint indices and weights exactly as a compressed variant
        // needs its UVs. A variant that reads none ignores joints it was
        // handed, as it ignores UVs it does not declare.
        let skinned = variant.needs_joints();
        let (joints, weights) = if skinned {
            let joints = joints.expect("a variant that reads joints needs the mesh's joints");
            let weights = weights.expect("a variant that reads joints needs the mesh's weights");
            assert_eq!(
                joints.len(),
                positions.len(),
                "the joint stream must describe the same vertices as the positions"
            );
            assert_eq!(
                weights.len(),
                positions.len(),
                "the weight stream must describe the same vertices as the positions"
            );
            (joints, weights)
        } else {
            // A variant that reads none ignores the joints it was handed, as
            // it ignores UVs it does not declare.
            (&[] as &[[u16; 4]], &[] as &[[f32; 4]])
        };

        // Both streams' lengths are known before anything is uploaded, so the
        // pool can be sized first and each stream packed straight into it. The
        // position stream's compression derives the mesh metadata as it runs.
        let mut meta = MeshMetadata::default();
        let vertex_count = positions.len();
        let position_len = position_stream.byte_len(vertex_count);
        let secondary_len = secondary_stream.byte_len(vertex_count);

        // The morph displacements, when the draw reads them. They are geometry
        // the mesh owns, written once here; the weights that blend them are
        // per-instance pose state the frame's global group binds. The source
        // pools them into the frame-wide array, so what it hands over is the
        // raw data rather than a resource.
        let morph_deltas = variant.needs_morphs().then(|| {
            let morph_deltas = morph_deltas.expect(
                "a variant that reads morph positions needs the mesh's morph displacements",
            );
            assert_ne!(
                morph_deltas.target_count, 0,
                "a morphing mesh needs at least one target"
            );
            assert_eq!(
                morph_deltas.deltas.len(),
                positions.len() * morph_deltas.target_count as usize * 3,
                "a morph displacement array must hold three components per vertex and target"
            );
            morph_deltas
        });

        // The vertex layout the key's options declare, slot for slot. An empty
        // stream still gets a buffer entry — the pipeline simply declares no
        // attributes for that slot — so the mesh's layout matches the key's.
        let vertex_layouts = variant.vertex_buffer_layouts();
        let layout_of = |slot: u32| -> VertexBufferLayoutDesc {
            vertex_layouts
                .get(slot as usize)
                .and_then(|layout| layout.as_ref())
                .cloned()
                .unwrap_or(VertexBufferLayoutDesc {
                    array_stride: 0,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: VertexAttributes::new(),
                })
        };
        let position_layout = layout_of(PRIMARY_SLOT);
        let secondary_layout = layout_of(SECONDARY_SLOT);

        // The mesh's streams are the vertex pool's: one element allocation
        // covers every stream of the mesh at the same element index, which is
        // what lets a draw address them all with one `firstVertex` or
        // `baseVertex`. A stream the variant declares nothing for — an empty
        // secondary stream, say — gets no buffer and no part in it.
        let stream_layouts = [position_layout.clone(), secondary_layout.clone()];
        let vertices = self
            .vertex_pool
            .allocate(&device, &queue, &stream_layouts, vertex_count as u32)
            .expect("the vertex pool grows with the mesh");
        let vertex_offset = vertices.allocation.offset;
        // Each stream is packed straight into the pool through a queue write,
        // so no CPU-side buffer is built first. The write view is the only
        // mutable borrow, so the two streams are packed one after the other.
        for (slot, layout, len) in [
            (PRIMARY_SLOT, &position_layout, position_len),
            (SECONDARY_SLOT, &secondary_layout, secondary_len),
        ] {
            if layout.array_stride == 0 {
                continue;
            }
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
                    core::num::NonZeroU64::new(len as u64)
                        .expect("a declared stream is never empty"),
                )
                .expect("the stream's range fits its buffer");
            if slot == PRIMARY_SLOT {
                position_stream.write(
                    positions,
                    joints.iter().copied(),
                    compress_weights(weights),
                    &mut meta,
                    view.slice(..),
                );
            } else {
                secondary_stream.write(
                    uvs.unwrap_or(&[]),
                    colors.unwrap_or(&[]),
                    normals.unwrap_or(&[]),
                    &mut meta,
                    view.slice(..),
                );
            }
        }

        // Index buffer (optional): `Uint16` when every index fits, otherwise
        // `Uint32` — the same choice the compressor makes. The indices go into
        // the index pool, so every indexed mesh draws out of one shared buffer
        // and names its own slice through `GpuMesh::first`.
        //
        // A mesh's vertices are somewhere inside the pool its streams share, so
        // a draw has to be offset by `vertex_offset` to reach them. How it is
        // offset is the one thing that depends on the device: a draw's
        // `base_vertex` is the direct way, but WebGL2 has no `base_vertex`, so
        // there the offset is added to the indices themselves at upload and the
        // draw's stays zero. Baking widens the indices if they no longer fit
        // `u16`, which is the cost of binding the whole pool instead of
        // rebinding a vertex buffer per mesh.
        // A draw's `base_vertex` is a signed 32-bit integer, so an offset the
        // draw has to carry must fit one. A baked offset is a `u32` index like
        // any other and has no such limit.
        let supports_base_vertex = self.capabilities(world).base_vertex();
        assert!(
            !supports_base_vertex || vertex_offset <= i32::MAX as u32,
            "a vertex offset has to fit the i32 a draw's base vertex is"
        );
        // Where the pool offset goes. A device with `base_vertex` leaves the
        // indices mesh-local and lets the draw add the offset; one without has
        // to bake it into the indices and keep the draw's at zero. The two are
        // complementary, and exactly one of them is non-zero.
        let offset = IndexOffset::new(supports_base_vertex, vertex_offset);
        let draw_base_vertex = offset.draw_base_vertex;
        let (index_buffer, count, indexed, index_allocation, first) = match indices {
            Some(indices) if !indices.is_empty() => {
                let index_count = indices.len() as u32;
                // The bytes the draw will read, offset where the device
                // requires and sized for the pool's own alignment. An offset
                // that pushes an index past `u16::MAX` has to widen the buffer
                // it is written into, so the format and the bytes are decided
                // together.
                let (format, packed_len) = offset.packed_len(indices);
                let range = self
                    .index_pool
                    .allocate(&device, &queue, packed_len as u32)
                    .expect("the index pool grows with the mesh");
                {
                    let mut graph_holder = self.context.graph(world);
                    let graph = graph_holder.get();
                    MeshSource::sync_pool_node(&self.index_pool, &self.index_pool_id, graph);
                    let buffer = graph
                        .get(&self.index_pool_id)
                        .expect("the index pool node exists")
                        .clone();
                    let mut view = queue
                        .write_buffer_with(
                            &buffer,
                            u64::from(range.allocation.offset),
                            core::num::NonZeroU64::new(packed_len as u64)
                                .expect("an empty index buffer is never allocated"),
                        )
                        .expect("the index range fits its buffer");
                    offset.write(indices, view.slice(..));
                }
                let first = range.allocation.offset / index_format_size(format);
                (
                    Some((self.index_pool_id.clone(), format)),
                    index_count,
                    true,
                    Some(range.allocation),
                    first,
                )
            }
            // A non-indexed draw names its first vertex directly, so the pool
            // offset goes on the draw's range. A non-zero `base_vertex` is
            // ignored for a non-indexed draw, so it cannot carry the offset.
            _ => (None, vertex_count as u32, false, None, vertex_offset),
        };

        // The mesh owns the metadata entry `meta` — the same AABB and UV
        // decode parameters the compression just derived, plus the addressing
        // the draw needs. The vertex offset is known here, and the mesh's own
        // morph count is taken from the displacements it uploaded.
        let aabb = Aabb::new(meta.aabb_center, meta.aabb_half_extents);
        let mut mesh = self.allocate_mesh_with_metadata(
            world,
            MeshDesc {
                // The streams are the vertex pool's, not the mesh's own: the
                // mesh names the pool's node for each slot and its own range,
                // so no buffer is registered under the mesh for them.
                vertex_buffers: ArrayVec::new(),
                // The indices are the index pool's, not the mesh's own: the
                // mesh names the pool's node and its own range, so no buffer
                // is registered under the mesh for them.
                index_buffer: None,
                count,
                indexed,
                aabb,
                // The built-in variants bind nothing at the mesh group, so the
                // mesh carries no group of its own.
                bind_group: None,
                morph_deltas,
            },
            meta,
            skinned,
            vertex_offset,
        );

        // The mesh's parts are complete only now: the pool-backed slots and
        // ranges are known once the pools have been allocated above. The handle
        // is freshly built and shared with nothing, so it is uniquely owned
        // here and can be patched in place.
        let parts = Rc::get_mut(&mut mesh.parts).expect("a fresh mesh handle is unshared");

        // The mesh's layout is only the vertex streams it draws from: the
        // per-instance stream is the family's own and is bound by the source
        // at draw time, so it is not part of the mesh's layout at all.
        let mut layouts = ArrayVec::<_, MAX_VERTEX_BUFFERS>::new();

        // The mesh's slices of the pools it shares: the draw names its ranges
        // by `first` and `base_vertex`, and the allocations are handed back on
        // removal. The layout is the key's own, slot by slot: the family keys
        // on it, so it is owned by the mesh even though the buffers behind it
        // are the pool's. A stream the variant declares nothing for is left
        // out, as it is for a mesh that owns its buffers.
        for (slot, layout) in [
            (PRIMARY_SLOT, &position_layout),
            (SECONDARY_SLOT, &secondary_layout),
        ] {
            if layout.array_stride > 0 {
                layouts.push((slot, layout.clone()));
                // Resolved before the parts are borrowed mutably below: the
                // lookup needs the source, which the borrow would rule out.
                let node = self.vertex_node(world, layout);
                parts.vertex_buffers.push((slot, node));
            }
        }
        // The pool-backed slots join the layout only here, once the pools have
        // been allocated; the mesh's handle is replaced with the complete one.
        parts.index_buffer = index_buffer;
        parts.index_allocation = index_allocation;
        parts.vertex_allocation = Some(vertices.allocation);
        mesh.vertex_layout = VertexLayout::new(layouts);
        mesh.first = first;
        // Zero whenever the offset was baked into the indices instead, which is
        // what a draw on a device without `base_vertex` requires. The metadata
        // entry carries `vertex_offset` either way: the shader reads
        // pool-global vertex ordinals on both paths.
        mesh.base_vertex = draw_base_vertex;
        mesh
    }

    /// Allocate the lit material bind group from an existing base-colour
    /// texture view and sampler, and return its [GpuMaterial] handle.
    ///
    /// Only the bind group is built. `view_id` and `sampler_id` name resources
    /// the caller has already put in the graph, and they become the group
    /// dependencies, so replacing either marks it dirty. Whether a draw
    /// samples the texture is the draw own answer — it is what decides the
    /// variant — so a caller that wants a material calls this, and one that
    /// does not leaves it out.
    ///
    /// # Panics
    ///
    /// If `view_id` or `sampler_id` is not a texture view or sampler in the
    /// graph.
    pub fn allocate_lit_material(
        &mut self,
        world: &Scope<'_>,
        options: &Hashed<LitOptions>,
        view_id: ResHandle<TextureView>,
        sampler_id: ResHandle<wgpu::Sampler>,
    ) -> GpuMaterial {
        let layout = options.material_bind_group_layout(&self.device(world));

        let device = self.device(world).clone();
        // The recipe reads the view and the sampler back out of the graph each
        // time it runs, so replacing either rebuilds the group from the current
        // handle rather than the one captured here.
        let mut dependencies = ArrayVec::<ResHandle, 2>::new();
        dependencies.push(view_id.erase());
        dependencies.push(sampler_id.erase());
        self.allocate_material(
            world,
            move |graph| {
                let view = graph
                    .get(&view_id)
                    .expect("the view is in the graph")
                    .view()
                    .clone();
                let sampler = graph
                    .get(&sampler_id)
                    .expect("the sampler is in the graph")
                    .clone();
                let mut entries = ArrayVec::<wgpu::BindGroupEntry<'_>, 2>::new();
                entries.push(wgpu::BindGroupEntry {
                    binding: BASE_COLOR_TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(&view),
                });
                entries.push(wgpu::BindGroupEntry {
                    binding: BASE_COLOR_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("unlit3d::material::bind_group"),
                    layout: &layout,
                    entries: &entries,
                })
            },
            dependencies,
        )
    }
}
