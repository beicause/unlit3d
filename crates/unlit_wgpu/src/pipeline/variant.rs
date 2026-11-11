use super::*;

/// The complete identity of a built-in lit pipeline, and its blueprint.
///
/// A family resolves one from an entity's [`LitOptions`] and the facts its
/// draw carries: the mesh's vertex channels, whether the draw binds a material,
/// whether the mesh morphs, the mesh's index format and the render target. Two
/// draws that agree on all of it share one compiled pipeline; because the
/// variant is the complete description of the blueprint, the cache key is
/// canonical by construction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LitVariant {
    /// The caller's policy, with its hash computed once. The variant is the
    /// cache key, so the stored word is what every per-frame lookup hashes;
    /// rewrite the policy through [`Hashed::update`] rather than the value
    /// itself, so the word follows the change.
    pub options: Hashed<LitOptions>,
    /// The render target the pipeline is built for.
    pub surface: SurfaceKey,
    /// The per-vertex channels the mesh's layout carries.
    pub channels: LitVertexChannels,
    /// Whether the draw binds a material group and so samples a base-color
    /// texture.
    pub base_color_texture: bool,
    /// Whether the mesh carries morph displacements.
    pub morph: bool,
    /// The strip index format the mesh's index buffer requires, if any.
    pub strip_index_format: Option<wgpu::IndexFormat>,
}

impl LitVariant {
    /// The WESL `@if` features the variant asks for, paired with their state.
    ///
    /// Every name appears, so the composed variant never sees a name it does
    /// not know. The per-vertex names come from [`Self::channels`], the material
    /// one from [`Self::base_color_texture`] and the policy ones from
    /// [`Self::options`]. The per-instance record is not conditional at all, so
    /// it contributes no names here: the built-in vertex stream always
    /// declares and binds an instance buffer.
    ///
    /// `texel_arrays` is the device's answer rather than the variant's, so it
    /// is passed in from the caller that read the device.
    fn features(&self, texel_arrays: bool) -> [(&'static str, bool); 17] {
        [
            ("VERTEX_POSITION", self.channels.position.position.is_some()),
            (
                "UNCOMPRESSED_POSITION",
                matches!(
                    self.channels.position.position,
                    Some(ChannelEncoding::UncompressedPosition | ChannelEncoding::PlanarPosition)
                ),
            ),
            (
                "PLANAR_POSITION",
                matches!(
                    self.channels.position.position,
                    Some(ChannelEncoding::PlanarPosition)
                ),
            ),
            (
                "VERTEX_UV",
                self.channels.secondary.contains(SecondaryFlags::UV),
            ),
            (
                "UNCOMPRESSED_UV",
                self.channels
                    .secondary
                    .contains(SecondaryFlags::UNCOMPRESSED_UV),
            ),
            (
                "VERTEX_COLOR",
                self.channels.secondary.contains(SecondaryFlags::COLOR),
            ),
            (
                "VERTEX_NORMAL",
                self.channels.secondary.contains(SecondaryFlags::NORMAL),
            ),
            ("BASE_COLOR_TEXTURE", self.base_color_texture),
            ("SRGB_TO_LINEAR_OUTPUT", self.options.srgb_to_linear_output),
            ("VERTEX_JOINTS", self.channels.position.joints),
            ("MORPH_POSITIONS", self.morph),
            ("TEXEL_ARRAY", texel_arrays),
            (
                "BASE_COLOR_LUMINANCE",
                self.options.base_color == BaseColorChannels::Luminance,
            ),
            (
                "BASE_COLOR_LUMINANCE_ALPHA",
                self.options.base_color == BaseColorChannels::LuminanceAlpha,
            ),
            ("ALPHA_CUTOFF", self.options.alpha_cutoff),
            ("EXPANDED", self.options.expanded),
            ("LIT", !self.options.unlit),
        ]
    }

    /// Whether the variant deforms its vertices by joint matrices and so reads
    /// the global group's array of them.
    pub fn needs_joints(&self) -> bool {
        self.channels.position.joints
    }

    /// Whether the variant reads the global group's morph arrays: the
    /// per-instance weights and the per-mesh displacements.
    pub fn needs_morphs(&self) -> bool {
        self.morph
    }

    /// Whether the variant reads the global group's mesh-metadata array: the
    /// per-mesh decode parameters and the addressing a morph reads.
    ///
    /// Mirrors the shader's metadata condition, so the layout and the composed
    /// variant agree on whether the binding exists. A morph needs the array
    /// even when every channel is uncompressed, because the struct also carries
    /// the vertex offset and morph count its displacements are addressed by.
    pub fn needs_metadata(&self) -> bool {
        let compressed_position = matches!(
            self.channels.position.position,
            Some(ChannelEncoding::CompressedPosition)
        );
        let compressed_uv = self.channels.secondary.contains(SecondaryFlags::UV)
            && !self
                .channels
                .secondary
                .contains(SecondaryFlags::UNCOMPRESSED_UV);
        compressed_position || compressed_uv || self.needs_morphs()
    }

    /// The secondary vertex stream this variant expects.
    ///
    /// The stream is described in [`crate::mesh`]'s own terms: this variant's
    /// channels are translated into the ones that make up a vertex stream, so
    /// packing meshes for this pipeline needs nothing specific to it.
    pub fn secondary_stream(&self) -> SecondaryStreamWriter {
        SecondaryStreamWriter {
            flags: self.channels.secondary,
        }
    }

    /// The position vertex stream this variant expects.
    ///
    /// The joint indices and weights the variant deforms with belong to this
    /// stream rather than one of their own, so a skinned variant's stream is
    /// wider by the joint pair and nothing else changes.
    pub fn position_stream(&self) -> PositionStreamWriter {
        PositionStreamWriter {
            channels: self.channels.position,
        }
    }

    /// Reject a variant whose array path the device cannot serve.
    ///
    /// The texel path works on every device — a texture read is universal — so
    /// forcing it on a device that also has storage buffers is allowed. The
    /// reverse is not: a bind-group layout naming a storage buffer is rejected
    /// outright by a device without them, so the failure is caught here with the
    /// reason rather than left to wgpu's validation.
    ///
    /// A variant that declares no array binding at all — the UI's, which reads
    /// only full-precision vertex attributes — has nothing to serve either way,
    /// so it is not asked to pick a path.
    ///
    /// # Panics
    ///
    /// If the variant reads storage buffers on a device that has none.
    fn assert_device_supports_arrays(&self, device: &wgpu::Device, texel_arrays: bool) {
        let reads_an_array = self.needs_metadata() || self.needs_joints() || !self.options.unlit;
        assert!(
            !reads_an_array || texel_arrays || supports_storage_buffers(device),
            "this variant reads its arrays from storage buffers, but the device has none \
             (its `max_storage_buffers_per_shader_stage` is 0, as WebGL2's is): compose \
             the variant with `texel_arrays` set, the way the built-in family does"
        );
    }
}

impl LitVariant {
    /// Build the bind-group layouts this variant declares, without compiling
    /// the pipeline.
    ///
    /// The layouts come from the same variant a pipeline is built from, so the
    /// two agree: the global group always exists, and the material group only
    /// for a variant that samples a base-color texture. The mesh group is
    /// always `None` — the built-in variants bind nothing there.
    ///
    /// The array path is the device's answer rather than the variant's, so it
    /// is read here: a cache is built for one device, which is why the path
    /// never enters the variant key.
    pub fn bind_group_layouts(&self, device: &wgpu::Device) -> LitBindGroupLayouts {
        let texel_arrays = !supports_storage_buffers(device);
        self.assert_device_supports_arrays(device, texel_arrays);
        // The group holds the frame's shared inputs: the camera, the frame
        // globals, the mesh-metadata decode parameters, the pose arrays every
        // instance slices into, and the morph displacements every mesh slices
        // into. A variant that reads none of the optional ones declares no
        // binding for them. The builder is the same one a caller's own pipeline
        // uses, so the built-in variants carry no private layout path.
        let bindings = GlobalBindings {
            metadata: self.needs_metadata(),
            joints: self.needs_joints(),
            morphs: self.needs_morphs(),
            // A lit variant reads the frame's lights, its cells and its
            // polygon vertices; an unlit one declares no such binding.
            lights: !self.options.unlit,
        };
        let global = global_bind_group_layout(device, bindings, texel_arrays);

        let material = self
            .base_color_texture
            .then(|| self.options.material_bind_group_layout(device));

        // The built-in variants bind nothing at the mesh group: every input
        // they read is in the global group or on the instance stream. The
        // index remains a general extension point a caller's pipeline can use.
        let mesh = None;

        LitBindGroupLayouts {
            global,
            material,
            mesh,
        }
    }

    /// The vertex-buffer declarations the built-in shader expects, in slot
    /// order.
    ///
    /// The first two slots follow the variant's channels; the third is always
    /// the instance stream, which every built-in variant binds. A slot whose
    /// variant declares no attributes is reported as `None`, so a variant never
    /// binds a buffer the shader does not declare.
    pub fn vertex_buffer_layouts(&self) -> [Option<VertexBufferLayoutDesc>; 3] {
        // Each stream describes its own layout, so the attributes a pipeline
        // declares and the bytes a packed mesh writes come from one
        // description and cannot drift apart.
        let position = self.position_stream().channels.channels();
        let secondary = self.secondary_stream().channels();
        [
            (!position.is_empty()).then(|| position.layout()),
            (!secondary.is_empty()).then(|| secondary.layout()),
            Some(self.instance_layout()),
        ]
    }

    /// The instance stream every built-in variant expects.
    ///
    /// One record layout serves every variant of a frame — the source uploads a
    /// single `&[MeshInstance]` buffer and binds it at [`INSTANCE_SLOT`] for
    /// every draw — so the stride is always the whole [`MeshInstance`]. The
    /// declared attributes mirror the shader's own input declarations: the
    /// model matrix, the base color, the pose bases and the metadata index
    /// unconditionally, the alpha cutoff and the hull width under the features
    /// that declare them.
    /// A pipeline never declares an attribute the shader does not, which the
    /// validation layers would otherwise flag on every draw.
    ///
    /// [`MeshInstance`]: crate::mesh::MeshInstance
    fn instance_layout(&self) -> VertexBufferLayoutDesc {
        let mut attributes = crate::specialize::VertexAttributes::new();
        let field = |offset: usize| offset as u64;
        let model = core::mem::offset_of!(crate::mesh::MeshInstance, model);
        let column = core::mem::size_of::<[f32; 4]>() as u64;
        let mut fields = vec![
            (
                location::MODEL_0,
                wgpu::VertexFormat::Float32x4,
                field(model),
            ),
            (
                location::MODEL_1,
                wgpu::VertexFormat::Float32x4,
                field(model) + column,
            ),
            (
                location::MODEL_2,
                wgpu::VertexFormat::Float32x4,
                field(model) + 2 * column,
            ),
            (
                location::BASE_COLOR,
                wgpu::VertexFormat::Unorm8x4,
                field(core::mem::offset_of!(crate::mesh::MeshInstance, base_color)),
            ),
            (
                location::JOINTS_BASE,
                wgpu::VertexFormat::Uint32,
                field(core::mem::offset_of!(
                    crate::mesh::MeshInstance,
                    joints_base
                )),
            ),
            (
                location::METADATA_INDEX,
                wgpu::VertexFormat::Uint32,
                field(core::mem::offset_of!(
                    crate::mesh::MeshInstance,
                    metadata_index
                )),
            ),
        ];
        // The cutoff is conditional: the shader declares it only under
        // ALPHA_CUTOFF, so a variant that never cuts must not declare it
        // either — the validation layers flag an attribute the vertex stage
        // never touches.
        if self.options.alpha_cutoff {
            fields.push((
                location::CUTOFF,
                wgpu::VertexFormat::Float32,
                field(core::mem::offset_of!(crate::mesh::MeshInstance, cutoff)),
            ));
        }
        fields.push((
            location::MORPH_BASE,
            wgpu::VertexFormat::Uint32,
            field(core::mem::offset_of!(crate::mesh::MeshInstance, morph_base)),
        ));
        // The other conditional instance input, on the same terms: only an
        // expanding variant's shader declares the hull width.
        if self.options.expanded {
            fields.push((
                location::HULL_WIDTH,
                wgpu::VertexFormat::Float32,
                field(core::mem::offset_of!(crate::mesh::MeshInstance, hull_width)),
            ));
        }
        for (shader_location, format, offset) in fields {
            attributes.push(wgpu::VertexAttribute {
                format,
                offset,
                shader_location,
            });
        }
        VertexBufferLayoutDesc {
            // The stride is the record's whole size rather than the sum of the
            // declared formats, so a variant that declares a subset still steps
            // over the fields it omits, which keeps every instance aligned in
            // one shared buffer.
            array_stride: core::mem::size_of::<crate::mesh::MeshInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes,
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for LitVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        self.clone()
    }
}

impl PipelineDescriptor<wgpu::RenderPipeline> for LitVariant {
    /// Compose this variant's entry shader and build the pipeline, so a variant
    /// compiles through the same [PipelineDescriptor] mechanism a caller's own
    /// descriptor does.
    ///
    /// The entry shader is the built-in `lit.wesl`, with the options' own
    /// snippets ([`LitOptions::shader`]) spliced into it at its marks.
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        let texel_arrays = !supports_storage_buffers(device);
        let label = match &self.options.shader {
            Some(shader) => shader.label(),
            None => BUILT_IN_SHADER_LABEL,
        };
        let wgsl =
            compose_shader(self, texel_arrays).unwrap_or_else(|error| panic!("{label}: {error}"));
        let module = crate::util::shader_module(device, &wgsl, label);

        let layouts = self.bind_group_layouts(device);
        let layout_refs = [
            Some(&layouts.global),
            layouts.material.as_ref(),
            layouts.mesh.as_ref(),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unlit_wgpu::lit::layout"),
            bind_group_layouts: &layout_refs,
            immediate_size: 0,
        });

        // The caller's policy is the base; the target and the mesh's strip
        // index format are applied here, because the variant carries them
        // rather than the caller. The policy is read field by field rather
        // than rewritten in place: the options are keyed on, and the target is
        // applied after hashing rather than into it.
        let options = &*self.options;
        let depth_stencil = self
            .surface
            .depth_stencil_format
            .map(|format| options.depth_stencil.state(format));
        let color_target = wgpu::ColorTargetState {
            format: self.surface.color_format,
            blend: options.blend,
            write_mask: options.write_mask,
        };

        let vertex_buffers = self.vertex_buffer_layouts();
        let vertex_buffers: Vec<Option<wgpu::VertexBufferLayout<'_>>> = vertex_buffers
            .iter()
            .map(|layout| layout.as_ref().map(VertexBufferLayoutDesc::as_wgpu))
            .collect();
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unlit_wgpu::lit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(VS_MAIN),
                compilation_options: Default::default(),
                buffers: &vertex_buffers,
            },
            primitive: {
                let mut primitive = options.primitive;
                primitive.strip_index_format = self.strip_index_format;
                primitive
            },
            depth_stencil,
            multisample: wgpu::MultisampleState {
                count: self.surface.sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(FS_MAIN),
                compilation_options: Default::default(),
                targets: &[Some(color_target)],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

/// Compose `variant`'s entry shader into one WGSL module.
///
/// The main module is the built-in `lit.wesl`, with the options' snippets
/// spliced into it at its hook marks. It is composed as the package's own
/// `lit` module, so a snippet that names `package::light` or
/// `unlit_wgpu::light` reaches the modules the shader around it imports, and
/// sees the variant's flags.
///
/// `texel_arrays` is the device answer rather than the variant one, so it is
/// passed in from whoever read the device; it selects the shader array path
/// and never enters the variant key, because a cache is built for one device.
///
/// # Panics
///
/// If the variant contradicts itself: the base-color texture is sampled with a
/// per-vertex UV that is not read, a deformation displaces a position that is
/// not read, or a luminance layout expands a texel that is never sampled.
///
/// The per-vertex channels cannot contradict each other the way flags once
/// could: an encoding is a property of the channel it encodes, so
/// [`LitVertexChannels`] has no state in which a channel is uncompressed
/// without being read. The per-instance fields cannot either: the instance
/// stream is bound for every variant, so there is no state in which one is
/// read without the stream that carries it.
pub(super) fn compose_shader(
    variant: &LitVariant,
    texel_arrays: bool,
) -> Result<String, crate::compose::ComposeError> {
    assert!(
        !variant.base_color_texture || variant.channels.secondary.contains(SecondaryFlags::UV),
        "the base-color texture is sampled with the per-vertex UV, so sampling one requires a UV channel"
    );
    assert!(
        !variant.channels.position.joints || variant.channels.position.position.is_some(),
        "the joint pair is part of the position stream, so a skinned variant needs a position to deform"
    );
    assert!(
        !variant.morph || variant.channels.position.position.is_some(),
        "a morph target displaces the position, so `MORPH_POSITIONS` requires a position channel"
    );
    assert!(
        variant.options.base_color == BaseColorChannels::Rgba || variant.base_color_texture,
        "the luminance flags describe how a sampled base-color texel expands, so `BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` require a sampled base-color texture"
    );
    assert!(
        !variant.options.expanded || variant.channels.secondary.contains(SecondaryFlags::NORMAL),
        "an expanding variant pushes the vertex along its normal, so `EXPANDED` requires a normal channel"
    );

    let features = variant.features(texel_arrays);
    let replacement = variant.options.shader.as_ref().map(|shader| {
        crate::compose::splice(
            crate::shader::lit::MODULE.source,
            shader.module(),
            shader.vertex(),
            shader.fragment(),
        )
    });
    crate::compose::compose(BUILT_IN_MODULE, &features, replacement.as_deref())
}
