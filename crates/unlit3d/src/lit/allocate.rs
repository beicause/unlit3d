//! Mesh allocation and family registration for the built-in lit pipeline.

use super::*;
use crate::mesh_source::{PooledMeshDesc, PooledStream};

/// Mesh allocation and family registration for the built-in lit pipeline.
///
/// The allocation below is a use of [`MeshSource::allocate_pooled_mesh`], the
/// same pooled path a caller's own family allocates its meshes through: the
/// built-in shader's channel layout and packed stream writers are what this
/// module adds, not access to the pools.
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
    /// This is a use of [`MeshSource::allocate_pooled_mesh`]: it derives the
    /// streams the channels imply and packs them with the writers in
    /// [unlit_wgpu::mesh], while the pooled allocation itself is shared with
    /// every family.
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

        // The vertex layout the channels declare, slot for slot. An empty
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
        let streams = [
            (PRIMARY_SLOT, layout_of(PRIMARY_SLOT)),
            (SECONDARY_SLOT, layout_of(SECONDARY_SLOT)),
        ];

        // The compression derives the mesh's metadata as it packs, so each
        // stream writes into the entry the pooled allocation hands it. The two
        // streams are packed one after the other, in slot order.
        self.allocate_pooled_mesh(
            world,
            PooledMeshDesc {
                streams: &streams,
                vertex_count: positions.len(),
                indices,
                morph_deltas,
                skinned,
            },
            |stream: PooledStream<'_>| {
                if stream.slot == PRIMARY_SLOT {
                    position_stream.write(
                        positions,
                        joints.iter().copied(),
                        compress_weights(weights),
                        stream.metadata,
                        stream.out,
                    );
                } else {
                    secondary_stream.write(
                        uvs.unwrap_or(&[]),
                        colors.unwrap_or(&[]),
                        normals.unwrap_or(&[]),
                        stream.metadata,
                        stream.out,
                    );
                }
            },
        )
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
