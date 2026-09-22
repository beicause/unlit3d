//! The top-level renderer — a resource entity component that owns GPU state and
//! drives rendering each frame from the ECS world.
//!
//! The [Renderer] is the bridge between [unlit_ecs] data and the
//! [wgpu_unlit_render] GPU pipeline. Spawn it once (typically as a resource
//! entity) and call [Renderer::render] every frame.

use core::any::TypeId;
use std::sync::Arc;

use unlit_ecs::{LocalWorld, TypeIdHashMap};
use wgpu_unlit_render::globals::{Globals, View};
use wgpu_unlit_render::mesh::{
    CompressedPosition, MeshInfo, MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use wgpu_unlit_render::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    INSTANCE_SLOT, MESH_INFO_BINDING, MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT,
    UnlitFlags, UnlitOptions, UnlitPipeline, apply_surface,
};
use wgpu_unlit_render::render_attachments::{
    AttachmentsInfo, RenderAttachments, default_depth_stencil_format,
};
use wgpu_unlit_render::resources::{Resource, ResourceGraph, ResourceId};
use wgpu_unlit_render::scene::Scene;
use wgpu_unlit_render::specialize::{
    Specializable, Specializer, SpecializerKey, SurfaceKey, VertexBufferLayoutDesc,
};
use zerocopy::IntoBytes;

use crate::bounds::Aabb;
use crate::components::{Camera, GpuMaterial, GpuMesh};
use crate::culling::VisibleMesh;
use crate::mesh::{MeshDesc, VertexBufferDesc};
use crate::pipeline::{
    DrawKey, FamilyContext, GlobalBinding, GlobalGroupRebuild, PipelineDesc, PipelineFactory,
    PipelineId, PipelineKey, RegisteredGlobal, RenderResources,
};
use crate::scene::{
    AnyFamily, EntryHandles, Family, PipelineHandles, SceneFrame, VisibleEntry, assemble_scene,
    collect_and_sort_visible,
};

/// The renderer: ECS resource component that holds GPU state and orchestrates
/// frame rendering.
///
/// Spawn this as a component on a resource entity in a [LocalWorld]. Call
/// [Renderer::render] each frame to draw every entity that carries a
/// [GpuMesh] and a [GpuPipeline]. A [Transform] places it and an
/// [InstanceColor] tints it; both are optional, defaulting to the identity
/// transform and white. An entity missing a mesh or pipeline is not drawn.
pub struct Renderer {
    /// WGPU device.
    pub device: wgpu::Device,
    /// WGPU queue.
    pub queue: wgpu::Queue,

    /// Dependency-tracked GPU resource graph.
    pub graph: ResourceGraph,

    /// Frame attachments (colour, depth, MSAA).
    pub attachments: RenderAttachments,

    /// Resource id of the camera uniform buffer.
    camera_buf: ResourceId,
    /// Resource id of the frame-globals uniform buffer.
    globals_buf: ResourceId,
    /// Resource id of the mesh-metadata storage buffer.
    metadata_buf: ResourceId,
    /// Per-frame globals (advanced every call to [Renderer::render]).
    globals: Globals,
    /// Metadata entries, one per uploaded mesh.
    metadata: Vec<MeshMetadata>,
    /// How many metadata entries the current storage buffer can hold.
    metadata_capacity: u32,

    /// A reused instance-data buffer, grown as needed.
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: u32,

    /// Temporary attachments set for external-target rendering.
    external_attachments: Option<RenderAttachments>,

    /// Every concrete pipeline registered with this renderer, in
    /// registration order.
    ///
    /// A pipeline is appended the first time a family resolves a key that
    /// needs it, so an entity's concrete pipeline exists once its family has
    /// resolved the draw's surface and mesh layout.
    pipelines: Vec<RegisteredPipeline>,

    /// Every pipeline family registered with this renderer, keyed by the
    /// [TypeId] of its key type.
    ///
    /// Registration compiles nothing: a family appends to [`Self::pipelines`]
    /// lazily, the first time one of its variant keys is resolved.
    families: TypeIdHashMap<Box<dyn AnyFamily>>,

    // -- cached per-frame allocations ------------------------------------------
    /// Reused Vec of the meshes that passed this frame's frustum culling.
    visible_meshes_cache: Vec<VisibleMesh>,
    /// Reused Vec for visible-entity collection and per-frame sorting.
    visible_cache: Vec<VisibleEntry>,
    /// Reused Vec for packed instance data.
    packed_instances_cache: Vec<MeshInstance>,
    /// Reused Vec for cloned bind groups while the scene is built.
    bind_group_cache: Vec<wgpu::BindGroup>,
    /// Reused Vec for cloned buffers while the scene is built.
    buffer_cache: Vec<wgpu::Buffer>,
    /// Reused Vec for cloned pipeline handles while the scene is built.
    pipeline_handle_cache: Vec<PipelineHandles>,
    /// Reused Vec of per-entry handle indices while the scene is built.
    entry_handle_cache: Vec<EntryHandles>,
    /// Reused draw list, whose allocation survives between frames.
    scene_cache: Scene<'static>,
}

/// Build the unlit shader's global bind group from the renderer's buffers.
///
/// A free function rather than a method so the rebuild closure the pipeline is
/// registered with can capture it without borrowing the renderer.
fn create_unlit_global_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    resources: &RenderResources,
    needs_metadata: bool,
) -> wgpu::BindGroup {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: CAMERA_BINDING,
            resource: resources.camera.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: FRAME_BINDING,
            resource: resources.globals.as_entire_binding(),
        },
    ];
    if needs_metadata {
        entries.push(wgpu::BindGroupEntry {
            binding: MESH_METADATA_BINDING,
            resource: resources.metadata.as_entire_binding(),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("unlit3d::global"),
        layout,
        entries: &entries,
    })
}

/// Register one concrete pipeline and return its index in `pipelines`.
///
/// A free function rather than a method: the family that resolves the key
/// holds the renderer mutably, so the closure it registers through can only
/// borrow the disjoint fields this needs — the pipeline list, the resource
/// graph and the ids of the renderer's global buffers.
fn register_concrete(
    pipelines: &mut Vec<RegisteredPipeline>,
    graph: &mut ResourceGraph,
    camera_buf: ResourceId,
    globals_buf: ResourceId,
    metadata_buf: ResourceId,
    desc: PipelineDesc,
) -> PipelineId {
    // The material and mesh layouts describe a pipeline's binding interface,
    // but they do not outlive registration: the renderer builds those groups
    // from the family it registered, and wgpu already holds the pipeline's own
    // layout internally.
    let PipelineDesc {
        pipeline, global, ..
    } = desc;

    // A globally bound pipeline reads the renderer's own camera, globals and
    // metadata buffers, so it depends on all three: a rebuild of any of them
    // marks the group dirty. The group is rebuilt through the pipeline's own
    // closure, so a custom pipeline keeps control of what its layout binds.
    let global = global.map(|binding| {
        let GlobalBinding {
            bind_group,
            rebuild,
        } = binding;
        let id = graph
            .insert(
                Resource::BindGroup(bind_group),
                &[camera_buf, globals_buf, metadata_buf],
            )
            .expect("the renderer's buffers exist");
        RegisteredGlobal { id, rebuild }
    });

    pipelines.push(RegisteredPipeline { pipeline, global });
    // The length before the push is the index the pipeline landed on.
    PipelineId::new((pipelines.len() - 1) as u32)
}

/// The per-entity options the built-in unlit family draws with.
///
/// Two entities drawing the same family may start from different options, so
/// the options live on the entity's key rather than on the renderer. The
/// key's type is how the renderer finds the family it draws with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnlitPipelineKey {
    /// The options the entity's variants are specialized from.
    pub options: UnlitOptions,
}

impl UnlitPipelineKey {
    /// A key whose variants start from `options`.
    pub fn new(options: UnlitOptions) -> Self {
        Self { options }
    }
}

impl PipelineKey for UnlitPipelineKey {
    type Pipeline = UnlitPipeline;

    fn base_descriptor(&self) -> UnlitOptions {
        self.options.clone()
    }
}

/// The full specialization key of the built-in unlit family: the entity's
/// options, the frame's target and the mesh's vertex layout.
///
/// The mesh layout is not injective — two meshes whose raw attributes differ
/// but imply the same [UnlitFlags] rewrite the options to the same thing — so
/// the canonical form is the specialized options themselves, and those meshes
/// share one compiled pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UnlitDrawKey {
    options: UnlitOptions,
    surface: SurfaceKey,
    vertex_buffers: Vec<(u32, VertexBufferLayoutDesc)>,
}

impl From<(UnlitPipelineKey, DrawKey)> for UnlitDrawKey {
    fn from((key, draw): (UnlitPipelineKey, DrawKey)) -> Self {
        Self {
            options: key.options,
            surface: draw.surface,
            vertex_buffers: draw.vertex_buffers,
        }
    }
}

impl SpecializerKey for UnlitDrawKey {
    // The raw mesh layout is not part of the descriptor, so the primary key
    // alone is not injective; the canonical form below is.
    const IS_CANONICAL: bool = false;
    type Canonical = UnlitOptions;
}

/// The flag set a mesh's vertex layout implies, from every slot it declares.
fn unlit_flags_for_layout(layout: &[(u32, VertexBufferLayoutDesc)]) -> UnlitFlags {
    layout
        .iter()
        .fold(UnlitFlags::empty(), |flags, (slot, buffer)| {
            flags | UnlitFlags::for_vertex_buffer(*slot, buffer)
        })
}

/// Specializes the built-in unlit pipeline per entity options, frame target
/// and mesh layout.
///
/// The descriptor starts from the entity's own options, so one family serves
/// entities that differ in material or target policy. The target-dependent
/// fields are rewritten through [apply_surface], the same helper the core
/// crate's own specializer uses, and only the mesh-derived bits of
/// [UnlitFlags::MESH_MASK] are replaced. The canonical key the cache indexes
/// on is the resulting options: two draws whose specialized options agree
/// share one compiled pipeline.
#[derive(Clone, Copy, Debug, Default)]
struct UnlitDrawSpecializer;

impl Specializer<UnlitPipeline> for UnlitDrawSpecializer {
    type Key = UnlitDrawKey;

    fn specialize(&self, key: UnlitDrawKey, options: &mut UnlitOptions) -> UnlitOptions {
        *options = key.options;
        apply_surface(options, key.surface);
        options.flags =
            (options.flags & !UnlitFlags::MESH_MASK) | unlit_flags_for_layout(&key.vertex_buffers);
        options.clone()
    }
}

/// Describes a specialized [UnlitPipeline] the way the renderer registers it.
///
/// This is what keeps the built-in pipeline an ordinary client of the family
/// machinery: it packages the shader's layouts and a closure over the
/// shader's global bindings — the camera, globals and, for variants that read
/// a compressed channel, the metadata buffer.
struct UnlitFactory;

impl PipelineFactory<UnlitPipeline> for UnlitFactory {
    fn descriptor(&self, context: &FamilyContext<'_>, value: &UnlitPipeline) -> PipelineDesc {
        let layout = value.global_layout.clone();
        let needs_metadata = value.options.needs_metadata();
        let device = context.device.clone();

        let rebuild_layout = layout.clone();
        let rebuild: GlobalGroupRebuild = Arc::new(move |resources| {
            create_unlit_global_group(&device, &rebuild_layout, resources, needs_metadata)
        });

        let bind_group = rebuild(context.resources);
        PipelineDesc {
            pipeline: value.pipeline.clone(),
            global: Some(GlobalBinding {
                bind_group,
                rebuild,
            }),
            material_layout: value.material_layout.clone(),
            mesh_layout: value.mesh_layout.clone(),
        }
    }
}

/// A pipeline registered with the renderer.
struct RegisteredPipeline {
    /// The compiled pipeline.
    pipeline: wgpu::RenderPipeline,
    /// The bind group bound at the global index (0), together with the id it
    /// lives under in the resource graph and how to rebuild it. `None` for a
    /// pipeline that binds nothing there.
    global: Option<RegisteredGlobal>,
}

impl Renderer {
    /// Build a new renderer and its GPU resources.
    ///
    /// The renderer starts with no families: register the ones you draw with
    /// through [`Renderer::register_family`] — for the built-in unlit shader,
    /// [`Renderer::register_unlit_family`] does it for you. Registration
    /// compiles nothing; a family's first concrete pipeline is built the first
    /// time an entity that uses it is drawn.
    ///
    /// `width` and `height` are the initial viewport size in physical
    /// pixels.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, width: u32, height: u32) -> Self {
        let mut graph = ResourceGraph::new();

        // Camera uniform buffer.
        let camera_buf = graph
            .insert(
                Resource::Buffer(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::camera"),
                    size: size_of::<View>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })),
                &[],
            )
            .expect("camera buffer has no dependencies");

        // Globals uniform buffer.
        let globals = Globals::default();
        let globals_buf = graph
            .insert(
                Resource::Buffer(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::globals"),
                    size: size_of::<Globals>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })),
                &[],
            )
            .expect("globals buffer has no dependencies");

        // Metadata storage buffer (initially 1 entry).
        let metadata_buf = graph
            .insert(
                Resource::Buffer(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::mesh_metadata"),
                    size: size_of::<MeshMetadata>() as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })),
                &[],
            )
            .expect("metadata buffer has no dependencies");

        // Initial upload of camera and globals.
        queue.write_buffer(
            graph.get_buffer(camera_buf).expect("just inserted"),
            0,
            View::new(glam::Mat4::IDENTITY, glam::Vec3::ZERO).as_bytes(),
        );
        queue.write_buffer(
            graph.get_buffer(globals_buf).expect("just inserted"),
            0,
            globals.as_bytes(),
        );

        let attachments =
            RenderAttachments::new(&device, AttachmentsInfo::new(&device, width, height));

        Self {
            device,
            queue,
            graph,
            attachments,
            camera_buf,
            globals_buf,
            metadata_buf,
            globals,
            metadata: Vec::new(),
            metadata_capacity: 1,
            instance_buffer: None,
            instance_capacity: 0,
            external_attachments: None,
            pipelines: Vec::new(),
            families: TypeIdHashMap::default(),
            visible_meshes_cache: Vec::new(),
            visible_cache: Vec::new(),
            packed_instances_cache: Vec::new(),
            bind_group_cache: Vec::new(),
            buffer_cache: Vec::new(),
            pipeline_handle_cache: Vec::new(),
            entry_handle_cache: Vec::new(),
            scene_cache: Scene::new(),
        }
    }

    /// Resize the render target.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.attachments = RenderAttachments::new(
            &self.device,
            AttachmentsInfo::new(&self.device, width, height),
        );
    }

    /// Upload vertex and index data and return a [GpuMesh] handle.
    ///
    /// The renderer assumes no vertex layout: `vertex_buffers` lists exactly
    /// the buffers a draw binds, each tagged with the slot the pipeline's
    /// vertex state declares, so a mesh may carry any combination of
    /// attributes in any format. The pipeline specializes on the layout.
    ///
    /// The mesh's [`Aabb`](crate::Aabb) is recorded in the renderer's
    /// mesh-metadata array; call [`Renderer::update_metadata_buffer`] once the
    /// meshes for the frame are allocated to upload the array.
    ///
    /// [`Renderer::allocate_unlit_mesh`] is the helper that builds the
    /// compressed layout the built-in unlit shader expects.
    ///
    /// # Panics
    ///
    /// If `count` is zero for a non-empty draw, or if the index format does
    /// not match the packed data.
    pub fn allocate_mesh(&mut self, desc: MeshDesc) -> GpuMesh {
        let metadata = MeshMetadata {
            aabb_center: desc.aabb.center,
            aabb_half_extents: desc.aabb.half_extents,
            ..Default::default()
        };
        self.allocate_mesh_with_metadata(desc, metadata)
    }

    /// Upload a mesh together with the full metadata entry it owns.
    ///
    /// The entry is appended to the CPU-side array and reaches the GPU only
    /// when [`Renderer::update_metadata_buffer`] is called; the returned
    /// handle names its index.
    fn allocate_mesh_with_metadata(&mut self, desc: MeshDesc, metadata: MeshMetadata) -> GpuMesh {
        let MeshDesc {
            vertex_buffers,
            index_buffer,
            count,
            indexed,
            aabb,
            bind_group,
        } = desc;

        let mut buffers = Vec::with_capacity(vertex_buffers.len());
        let mut vertex_slots = Vec::with_capacity(vertex_buffers.len());
        let mut vertex_layout = Vec::with_capacity(vertex_buffers.len());
        for desc in vertex_buffers {
            let id = self
                .graph
                .insert(Resource::Buffer(desc.buffer), &[])
                .expect("a vertex buffer has no dependencies");
            vertex_slots.push((desc.slot, id));
            // The layout is owned by the mesh so a family can key on it
            // without reading the description again.
            vertex_layout.push((
                desc.slot,
                VertexBufferLayoutDesc {
                    array_stride: desc.array_stride,
                    step_mode: desc.step_mode,
                    attributes: desc.attributes,
                },
            ));
            buffers.push(id);
        }

        let index_buffer = index_buffer.map(|(buffer, format)| {
            let id = self
                .graph
                .insert(Resource::Buffer(buffer), &[])
                .expect("an index buffer has no dependencies");
            (id, format)
        });

        let bind_group_id = bind_group.map(|bind_group| {
            self.graph
                .insert(Resource::BindGroup(bind_group), &buffers)
                .expect("a mesh bind group depends on its vertex buffers")
        });

        // The entry is owned whether or not the pipeline reads it: a draw that
        // binds no metadata group simply leaves the index unused.
        let metadata_index = self.metadata.len() as u32;
        self.metadata.push(metadata);

        GpuMesh {
            vertex_buffers: vertex_slots,
            vertex_layout,
            index_buffer,
            count,
            indexed,
            aabb,
            metadata_index,
            bind_group_id,
        }
    }

    /// Upload raw mesh channels in the layout the built-in unlit shader
    /// expects, and return a [GpuMesh] handle.
    ///
    /// Positions and UVs are compressed to the compact vertex formats the
    /// shader decodes, packed into a position buffer and an interleaved
    /// UV-and-colour buffer, and bound with the mesh-metadata bind group the
    /// shader reads its decode parameters from. Colours are already stored
    /// in the width they are uploaded at, so they are copied through
    /// unchanged.
    ///
    /// This is a convenience over [`Renderer::allocate_mesh`]: it builds the
    /// same [`MeshDesc`] a caller could build by hand, and shares the
    /// compression in [wgpu_unlit_render::mesh] with anyone else who wants
    /// it.
    ///
    /// Which channels are packed is the key's own vertex layout: a slice for a
    /// channel the variant does not declare is left out, so the buffer always
    /// matches what the shader reads.
    ///
    /// # Panics
    ///
    /// If the input slices are empty or of mismatched length (see the
    /// compressors in [wgpu_unlit_render::mesh]), or if the key's options read
    /// no compressed channel and so declare no mesh-metadata group.
    pub fn allocate_unlit_mesh(
        &mut self,
        key: &UnlitPipelineKey,
        positions: &[[f32; 3]],
        uvs: Option<&[[f32; 2]]>,
        colors: Option<&[[u8; 4]]>,
        indices: Option<&[u32]>,
    ) -> GpuMesh {
        // The key's layouts are derived on the fly: the renderer keeps no
        // per-family state, and the pure layout builder is the same one the
        // family's factory uses, so the two cannot drift.
        let options = &key.options;
        let layouts = UnlitPipeline::bind_group_layouts(&self.device, options);
        let mesh_layout = layouts
            .mesh
            .clone()
            .expect("the unlit variant reads mesh metadata");
        // The stream is the key's, not the caller's: the UV-and-color buffer
        // has to be packed the way the shader reading it declares its vertex
        // layout, so a channel the key does not read is left out.
        let uv_color_stream = options.uv_color_stream();

        // Compress vertex streams.
        let mut meta = MeshMetadata::default();
        let packed_positions: Vec<_> = compress_positions(positions, &mut meta).collect();
        let vertex_count = packed_positions.len();

        let position_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::position"),
            size: (vertex_count * size_of::<CompressedPosition>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&position_buf, 0, packed_positions.as_bytes());

        // UV and colour vertex buffer, interleaved in the order the shader
        // declares: the channel a slice is given for is the channel packed.
        use wgpu::WriteOnly;
        let uv_color_len = uv_color_stream.byte_len(vertex_count);
        let uv_color_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::uv_color"),
            size: uv_color_len as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if uv_color_len > 0 {
            let mut staging = vec![0u8; uv_color_len];
            uv_color_stream.write(
                uvs.unwrap_or(&[]),
                colors.unwrap_or(&[]),
                &mut meta,
                WriteOnly::from_mut(staging.as_mut_slice()),
            );
            self.queue.write_buffer(&uv_color_buf, 0, &staging);
        }

        // MeshInfo uniform buffer (just the metadata index) and the bind
        // group the shader reads it through. The index is unknown until the
        // mesh is allocated below, and the buffer is written then.
        let mesh_info_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::info"),
            size: size_of::<MeshInfo>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mesh_info_id = self
            .graph
            .insert(Resource::Buffer(mesh_info_buf), &[])
            .expect("mesh_info buffer has no dependencies");

        let mesh_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::mesh::bind_group"),
            layout: &mesh_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: MESH_INFO_BINDING,
                resource: self
                    .graph
                    .get_buffer(mesh_info_id)
                    .expect("mesh_info buffer exists")
                    .as_entire_binding(),
            }],
        });

        // Index buffer (optional): `Uint16` when every index fits, otherwise
        // `Uint32` — the same choice the compressor makes.
        let (index_buffer, count, indexed) = match indices {
            Some(indices) if !indices.is_empty() => {
                let index_count = indices.len() as u32;
                let format = if compress_indices(indices).is_ok() {
                    wgpu::IndexFormat::Uint16
                } else {
                    wgpu::IndexFormat::Uint32
                };
                let data: Vec<u8> = match format {
                    wgpu::IndexFormat::Uint16 => compress_indices(indices)
                        .expect("checked above")
                        .collect::<Vec<u16>>()
                        .as_bytes()
                        .to_vec(),
                    _ => indices.as_bytes().to_vec(),
                };
                let padded_len = data
                    .len()
                    .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize);
                let mut padded = data;
                padded.resize(padded_len, 0);
                let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unlit3d::mesh::index"),
                    size: padded_len as u64,
                    usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.queue.write_buffer(&buf, 0, &padded);
                (Some((buf, format)), index_count, true)
            }
            _ => (None, vertex_count as u32, false),
        };

        // The vertex layout the key's options declare, slot for slot. An empty
        // stream still gets a buffer entry — the pipeline simply declares no
        // attributes for that slot — so the mesh's layout matches the key's.
        let vertex_layouts = UnlitPipeline::vertex_buffer_layouts(options);
        let layout_of = |slot: u32| -> VertexBufferLayoutDesc {
            vertex_layouts
                .get(slot as usize)
                .and_then(|layout| layout.as_ref())
                .map(VertexBufferLayoutDesc::from_wgpu)
                .unwrap_or(VertexBufferLayoutDesc {
                    array_stride: 0,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: Vec::new(),
                })
        };
        let position_layout = layout_of(POSITION_SLOT);
        let uv_color_layout = layout_of(UV_COLOR_SLOT);
        let instance_layout = layout_of(INSTANCE_SLOT);

        // The mesh owns the metadata entry `meta` — the same AABB and UV
        // decode parameters the compression just derived.
        let aabb = Aabb::new(meta.aabb_center, meta.aabb_half_extents);
        let mut mesh = self.allocate_mesh_with_metadata(
            MeshDesc {
                vertex_buffers: vec![
                    VertexBufferDesc {
                        slot: POSITION_SLOT,
                        buffer: position_buf,
                        array_stride: position_layout.array_stride,
                        step_mode: position_layout.step_mode,
                        attributes: position_layout.attributes,
                    },
                    VertexBufferDesc {
                        slot: UV_COLOR_SLOT,
                        buffer: uv_color_buf,
                        array_stride: uv_color_layout.array_stride,
                        step_mode: uv_color_layout.step_mode,
                        attributes: uv_color_layout.attributes,
                    },
                ],
                index_buffer,
                count,
                indexed,
                aabb,
                bind_group: Some(mesh_bind_group),
            },
            meta,
        );

        // The MeshInfo uniform names the entry the mesh just took.
        self.queue.write_buffer(
            self.graph
                .get_buffer(mesh_info_id)
                .expect("mesh_info buffer exists"),
            0,
            MeshInfo::new(mesh.metadata_index).as_bytes(),
        );

        // The renderer binds the per-instance buffer at [INSTANCE_SLOT] for
        // every draw, so the mesh's layout declares that slot even though the
        // buffer itself is not uploaded here. Without it the draw's key would
        // imply no [UnlitFlags::VERTEX_INSTANCE] and the pipeline would ignore
        // the instance transform.
        mesh.vertex_layout.push((INSTANCE_SLOT, instance_layout));
        mesh
    }

    /// Insert `texture` into the resource graph and return the id of a view
    /// of it.
    ///
    /// The view is recorded as depending on the texture, so replacing the
    /// texture marks every material built from the view dirty. The caller
    /// keeps ownership of the texture only until this call; afterwards the
    /// graph holds it.
    pub fn register_texture(&mut self, texture: wgpu::Texture) -> ResourceId {
        let texture_id = self
            .graph
            .insert(Resource::Texture(texture), &[])
            .expect("a texture has no dependencies");
        let view = self
            .graph
            .get_texture(texture_id)
            .expect("texture exists")
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.graph
            .insert(Resource::TextureView(view), &[texture_id])
            .expect("the view depends on its texture")
    }

    /// Create a sampler with `descriptor` — or the default one when `None` —
    /// insert it into the resource graph and return its id.
    pub fn register_sampler(
        &mut self,
        descriptor: Option<wgpu::SamplerDescriptor<'_>>,
    ) -> ResourceId {
        let sampler = self.device.create_sampler(&descriptor.unwrap_or_default());
        self.graph
            .insert(Resource::Sampler(sampler), &[])
            .expect("a sampler has no dependencies")
    }

    /// Allocate the unlit material bind group from an existing base-colour
    /// texture view and sampler, and return its [GpuMaterial] handle.
    ///
    /// Only the bind group is built. `view_id` and `sampler_id` name
    /// resources the caller has already put in the graph, and they become the
    /// group's dependencies, so replacing either marks it dirty. Returns
    /// `None` when the key's options read no base-color texture, so the
    /// variant binds no material group to put them in.
    ///
    /// # Panics
    ///
    /// If `view_id` or `sampler_id` is not a texture view or sampler in the
    /// graph.
    pub fn allocate_unlit_material(
        &mut self,
        key: &UnlitPipelineKey,
        view_id: ResourceId,
        sampler_id: ResourceId,
    ) -> Option<GpuMaterial> {
        if !key.options.flags.contains(UnlitFlags::BASE_COLOR_TEXTURE) {
            return None;
        }
        let layout = UnlitPipeline::bind_group_layouts(&self.device, &key.options)
            .material
            .clone()
            .expect("the base-color variant declares a material group");

        // Clone the handles out so the entries borrow nothing from the graph
        // while `allocate_material` mutates it.
        let view = self
            .graph
            .get_texture_view(view_id)
            .expect("the view is in the graph")
            .clone();
        let sampler = self
            .graph
            .get_sampler(sampler_id)
            .expect("the sampler is in the graph")
            .clone();
        let entries = [
            wgpu::BindGroupEntry {
                binding: BASE_COLOR_TEXTURE_BINDING,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: BASE_COLOR_SAMPLER_BINDING,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ];
        Some(self.allocate_material(&layout, &entries, &[view_id, sampler_id]))
    }

    /// Build a material bind group from `entries` against `layout` and return
    /// its [GpuMaterial] handle.
    ///
    /// Only the bind group is created here: the resources it reads are the
    /// caller's, already in the resource graph and named in `dependencies` so
    /// replacing one marks the group dirty. `layout` is the material layout of
    /// the pipeline the material is for —
    /// [`UnlitPipeline::bind_group_layouts`](wgpu_unlit_render::pipeline::UnlitPipeline::bind_group_layouts)
    /// for the built-in shader, or the one a custom pipeline registered.
    ///
    /// # Panics
    ///
    /// If a resource named in `entries` is not in the graph.
    pub fn allocate_material(
        &mut self,
        layout: &wgpu::BindGroupLayout,
        entries: &[wgpu::BindGroupEntry<'_>],
        dependencies: &[ResourceId],
    ) -> GpuMaterial {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::material::bind_group"),
            layout,
            entries,
        });
        let bind_group_id = self
            .graph
            .insert(Resource::BindGroup(bind_group), dependencies)
            .expect("a material bind group depends on graph resources");

        GpuMaterial { bind_group_id }
    }

    /// Render one frame from the ECS `world`.
    ///
    /// When `target` is `Some`, the frame is rendered into that texture
    /// view; otherwise the renderer's internal colour target is used.
    pub fn render(&mut self, world: &LocalWorld, target: Option<&wgpu::TextureView>) {
        // Find the camera.
        let camera = world
            .query::<&Camera>()
            .next()
            .map(|(_, c)| copy_camera(&c));
        let Some(camera) = camera else {
            self.clear_frame(target);
            return;
        };

        // Update global uniforms.
        self.globals.time += self.globals.delta_time;
        self.globals.frame_count += 1;
        self.queue.write_buffer(
            self.graph
                .get_buffer(self.globals_buf)
                .expect("globals buffer exists"),
            0,
            self.globals.as_bytes(),
        );

        let view = View::new(camera.clip_from_world, camera.position);
        self.queue.write_buffer(
            self.graph
                .get_buffer(self.camera_buf)
                .expect("camera buffer exists"),
            0,
            view.as_bytes(),
        );

        // Resolve the one render target this frame draws into, and key every
        // pipeline against it. The target is the caller's when given, the
        // renderer's own otherwise.
        let surface = target
            .map(|view| SurfaceKey {
                color_format: view.texture().format(),
                depth_stencil_format: Some(default_depth_stencil_format(&self.device)),
                sample_count: 1,
            })
            .unwrap_or_else(|| self.attachments.surface_key());

        // Collect, cull and sort the visible set in one pass. The cache keeps
        // its allocation between frames, so a steady scene allocates nothing.
        self.collect_and_sort_visible(world, &camera, surface);
        if self.visible_cache.is_empty() {
            self.clear_frame(target);
            return;
        }
        let instance_count = self.visible_cache.len() as u32;

        // Pack instance data into the reused scratch buffer and upload it.
        self.ensure_instance_buffer(instance_count);
        self.packed_instances_cache.clear();
        self.packed_instances_cache
            .extend(self.visible_cache.iter().map(|entry| entry.mesh.instance));
        self.queue.write_buffer(
            self.instance_buffer
                .as_ref()
                .expect("instance buffer exists"),
            0,
            self.packed_instances_cache.as_bytes(),
        );

        // The per-entry bind groups and buffers are cloned out of the graph
        // into the reused caches first. The scene then borrows those caches,
        // so nothing mutates them while it is alive.
        let mut bind_group_cache = std::mem::take(&mut self.bind_group_cache);
        let mut buffer_cache = std::mem::take(&mut self.buffer_cache);
        bind_group_cache.clear();
        buffer_cache.clear();

        let mut handles = std::mem::take(&mut self.entry_handle_cache);
        handles.clear();
        {
            let graph_ref = &self.graph;
            for entry in &self.visible_cache {
                let mesh = world
                    .get::<GpuMesh>(entry.mesh.entity)
                    .expect("visible entity has GpuMesh");

                let mesh_bg = mesh.bind_group_id.map(|id| {
                    bind_group_cache.push(
                        graph_ref
                            .get_bind_group(id)
                            .expect("mesh bind group exists")
                            .clone(),
                    );
                    bind_group_cache.len() - 1
                });

                let material_bg = match world.get::<GpuMaterial>(entry.mesh.entity) {
                    Some(material) => {
                        bind_group_cache.push(
                            graph_ref
                                .get_bind_group(material.bind_group_id)
                                .expect("material bind group exists")
                                .clone(),
                        );
                        Some(bind_group_cache.len() - 1)
                    }
                    None => None,
                };

                let vertex_start = buffer_cache.len();
                for &(_slot, buffer) in &mesh.vertex_buffers {
                    buffer_cache.push(
                        graph_ref
                            .get_buffer(buffer)
                            .expect("mesh vertex buffer exists")
                            .clone(),
                    );
                }

                let index_buffer = match mesh.index_buffer {
                    Some((buffer, format)) => {
                        buffer_cache.push(
                            graph_ref
                                .get_buffer(buffer)
                                .expect("mesh index buffer exists")
                                .clone(),
                        );
                        Some((buffer_cache.len() - 1, format))
                    }
                    None => None,
                };

                handles.push(EntryHandles {
                    mesh_bg,
                    material_bg,
                    vertex_start,
                    index_buffer,
                });
            }
        }

        // Every registered pipeline's handle and global bind group, indexed
        // the same way [`Renderer::pipelines`] is. A pipeline that binds no
        // global group carries `None`. The list is reused between frames, so
        // a steady scene allocates nothing.
        let mut pipeline_handles = std::mem::take(&mut self.pipeline_handle_cache);
        pipeline_handles.clear();
        pipeline_handles.extend(self.pipelines.iter().map(|registered| PipelineHandles {
            pipeline: registered.pipeline.clone(),
            global: registered.global.as_ref().map(|global| {
                self.graph
                    .get_bind_group(global.id)
                    .expect("global group exists")
                    .clone()
            }),
        }));

        let instance_buf = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();

        // Reuse the draw list's allocation across frames: take it back, lend it
        // this frame's lifetime, and return it once recorded.
        let mut scene = std::mem::take(&mut self.scene_cache).reborrow();
        assemble_scene(
            &mut scene,
            &self.visible_cache,
            world,
            &pipeline_handles,
            &bind_group_cache,
            &buffer_cache,
            &handles,
            &instance_buf,
        );

        // Record and submit the pass.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("unlit3d::encoder"),
            });

        let attachments = self.prep_attachments(target);
        {
            let mut pass = attachments.begin_pass(
                &mut encoder,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                wgpu::LoadOp::Clear(attachments.depth_clear()),
                wgpu::LoadOp::Clear(0),
            );
            scene.record(&mut pass);
        }

        // The scene is recorded, so recycle its allocation first: doing so
        // consumes the scene and ends the borrow of the caches, which can then
        // go back into self.
        self.scene_cache = scene.recycle();
        self.bind_group_cache = bind_group_cache;
        self.buffer_cache = buffer_cache;
        self.pipeline_handle_cache = pipeline_handles;
        self.entry_handle_cache = handles;

        self.queue.submit([encoder.finish()]);
    }
    // -- internal helpers ----------------------------------------------------

    /// Register a pipeline family for the key type `K`.
    ///
    /// This is the only way a pipeline enters the renderer, for the built-in
    /// unlit shader and a caller's own alike. The key type is the family's
    /// identity: entities draw with it when they carry a [GpuPipeline] of that
    /// type, and registering a second family for the same key type is a
    /// programming error.
    ///
    /// A family compiles nothing on registration: its concrete pipelines are
    /// built lazily, the first time a draw resolves a variant key, and are
    /// appended to [`Renderer::pipelines`] in resolution order.
    ///
    /// [`Renderer::register_unlit_family`] is the built-in unlit family; a
    /// pipeline with nothing to specialize on is registered with the
    /// [`TrivialSpecializer`](crate::TrivialSpecializer) and
    /// [`RenderPipelineFactory`](crate::RenderPipelineFactory) helpers.
    ///
    /// # Panics
    ///
    /// If a family is already registered for `K`.
    pub fn register_family<K, T, S, F>(&mut self, specializer: S, factory: F)
    where
        K: PipelineKey<Pipeline = T> + 'static,
        T: Specializable + 'static,
        S: Specializer<T> + 'static,
        S::Key: From<(K, DrawKey)>,
        F: PipelineFactory<T> + 'static,
    {
        let key = TypeId::of::<K>();
        assert!(
            !self.families.contains_key(&key),
            "a pipeline family is already registered for this key type"
        );
        self.families.insert(
            key,
            Box::new(Family::<T, S, F, K>::new(
                &self.device,
                specializer,
                factory,
            )),
        );
    }

    /// Register the built-in unlit shader as a family.
    ///
    /// The family specializes each entity's options on the frame's render
    /// target and the mesh's vertex layout: two draws that agree on both share
    /// one compiled pipeline, and a draw whose mesh layout implies different
    /// channels compiles its own. The options are the entity's: a mesh or
    /// material is built against the [UnlitPipelineKey] it will be drawn with,
    /// through [`Renderer::allocate_unlit_mesh`] and
    /// [`Renderer::allocate_unlit_material`].
    ///
    /// This compiles nothing; like any family, its first concrete pipeline is
    /// built when an entity that uses it is first drawn.
    ///
    /// # Panics
    ///
    /// If the unlit family is already registered.
    pub fn register_unlit_family(&mut self) {
        self.register_family::<UnlitPipelineKey, _, _, _>(UnlitDrawSpecializer, UnlitFactory);
    }

    /// The renderer's global buffers, cloned out of the resource graph.
    fn render_resources(&self) -> RenderResources {
        RenderResources {
            camera: self
                .graph
                .get_buffer(self.camera_buf)
                .expect("camera buf")
                .clone(),
            globals: self
                .graph
                .get_buffer(self.globals_buf)
                .expect("globals buf")
                .clone(),
            metadata: self
                .graph
                .get_buffer(self.metadata_buf)
                .expect("meta buf")
                .clone(),
        }
    }

    /// Rebuild the global bind group of every registered pipeline whose
    /// dependencies changed.
    ///
    /// Each pipeline supplies its own rebuild closure, so a replacement of
    /// the camera, globals or metadata buffer refreshes the built-in unlit
    /// pipeline and any custom one that binds those buffers alike.
    fn rebuild_dirty_global_groups(&mut self) {
        let resources = self.render_resources();
        let rebuilt: Vec<_> = self
            .pipelines
            .iter()
            .filter_map(|registered| {
                let global = registered.global.as_ref()?;
                self.graph
                    .is_dirty(global.id)
                    .then(|| (global.id, Arc::clone(&global.rebuild)))
            })
            .collect();

        for (id, rebuild) in rebuilt {
            let bind_group = rebuild(&resources);
            self.graph
                .replace(id, Resource::BindGroup(bind_group))
                .expect("global group exists");
            self.graph.mark_clean(id);
        }
    }

    /// Upload the mesh-metadata array to the GPU.
    ///
    /// `allocate_mesh` appends an entry for every mesh but leaves uploading
    /// to the caller: call this once after allocating or changing the meshes
    /// for a frame, before rendering it. The storage buffer is recreated only
    /// when the array outgrows it, so a steady scene rewrites in place and
    /// rebuilds no bind group.
    pub fn update_metadata_buffer(&mut self) {
        let needed = self.metadata.len().max(1) as u32;
        if needed > self.metadata_capacity {
            // Grow geometrically so repeated allocations amortize, and
            // recreate rather than resize: a buffer has a fixed size.
            let capacity = needed.max(self.metadata_capacity * 2);
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unlit3d::mesh_metadata"),
                size: capacity as u64 * size_of::<MeshMetadata>() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.graph
                .replace(self.metadata_buf, Resource::Buffer(buf))
                .expect("metadata buffer exists");
            self.metadata_capacity = capacity;
            // A replaced buffer invalidates every global group bound to it.
            self.rebuild_dirty_global_groups();
        }
        if self.metadata.is_empty() {
            return;
        }
        self.queue.write_buffer(
            self.graph
                .get_buffer(self.metadata_buf)
                .expect("metadata buffer exists"),
            0,
            self.metadata.as_bytes(),
        );
    }

    /// Grow the per-instance buffer geometrically when needed.
    fn ensure_instance_buffer(&mut self, count: u32) {
        if count <= self.instance_capacity {
            return;
        }
        let new_cap = count.max(self.instance_capacity * 2).max(1);
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::instance"),
            size: (new_cap as u64) * size_of::<MeshInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.instance_buffer = Some(buf);
        self.instance_capacity = new_cap;
    }

    /// Collect entities that pass frustum culling, then sort them for drawing.
    ///
    /// The pass itself lives in [crate::scene]; this only unpacks the
    /// renderer's caches and the closure that registers a newly resolved
    /// pipeline, so a family never borrows the whole renderer.
    ///
    /// `surface` is the render target this frame draws into; it is threaded
    /// into every entity's pipeline resolution, so each entry's `pipeline_id`
    /// is a concrete pipeline valid for that target.
    fn collect_and_sort_visible(
        &mut self,
        world: &LocalWorld,
        camera: &Camera,
        surface: SurfaceKey,
    ) {
        let resources = self.render_resources();
        let device = self.device.clone();

        // Split the borrows: the families mutate the pipeline list and the
        // resource graph through the register closure, while the caches are
        // disjoint fields.
        let (families, meshes, visible) = (
            &mut self.families,
            &mut self.visible_meshes_cache,
            &mut self.visible_cache,
        );
        let (pipelines, graph) = (&mut self.pipelines, &mut self.graph);
        let (camera_buf, globals_buf, metadata_buf) =
            (self.camera_buf, self.globals_buf, self.metadata_buf);
        let mut register = |desc| {
            register_concrete(
                pipelines,
                graph,
                camera_buf,
                globals_buf,
                metadata_buf,
                desc,
            )
        };

        collect_and_sort_visible(
            SceneFrame {
                families,
                meshes,
                visible,
                register: &mut register,
            },
            world,
            camera,
            surface,
            &device,
            &resources,
        );
    }

    /// Prepare attachments for returning a reference to the attachment set.
    fn prep_attachments(&mut self, target: Option<&wgpu::TextureView>) -> &RenderAttachments {
        let Some(view) = target else {
            return &self.attachments;
        };

        let target_width = view.texture().width().max(1);
        let target_height = view.texture().height().max(1);
        let depth_fmt = default_depth_stencil_format(&self.device);
        let info = AttachmentsInfo {
            color: Some(view.texture().format()),
            depth_stencil: Some(depth_fmt),
            width: target_width,
            height: target_height,
            sample_count: 1,
            transient_depth: true,
        };

        let depth_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("unlit3d::external_depth"),
            size: wgpu::Extent3d {
                width: target_width,
                height: target_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: depth_fmt,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let ext = RenderAttachments::new_with_targets(
            &self.device,
            info,
            Some(view.clone()),
            Some(depth_view),
        );
        self.external_attachments = Some(ext);
        self.external_attachments.as_ref().expect("just set")
    }

    /// Clear the frame without drawing anything.
    fn clear_frame(&mut self, target: Option<&wgpu::TextureView>) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("unlit3d::encoder"),
            });
        let attachments = self.prep_attachments(target);
        {
            let mut _pass = attachments.begin_pass(
                &mut encoder,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                wgpu::LoadOp::Clear(attachments.depth_clear()),
                wgpu::LoadOp::Clear(0),
            );
        }
        self.queue.submit([encoder.finish()]);
    }
}

/// Copy a camera out of its ECS cell so the borrow does not block world access.
fn copy_camera(cam: &Camera) -> Camera {
    Camera {
        clip_from_world: cam.clip_from_world,
        position: cam.position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Transform, Transparent, UnlitPipeline};
    use unlit_ecs::Entity;

    fn test_perspective() -> glam::Mat4 {
        glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0)
    }

    #[test]
    fn mesh_instance_from_transform() {
        let t = Transform {
            translation: glam::Vec3::new(1.0, 2.0, 3.0),
            rotation: glam::Quat::IDENTITY,
            scale: glam::Vec3::ONE,
        };
        let instance = MeshInstance::new(t.compute_matrix(), glam::Vec4::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(instance.model[0].w, 1.0);
        assert_eq!(instance.model[1].w, 2.0);
        assert_eq!(instance.model[2].w, 3.0);
    }

    // -- pipeline registration ---------------------------------------------

    /// A renderer on wgpu's noop backend with the built-in unlit family
    /// registered, plus a standard key to draw with.
    fn noop_renderer() -> (Renderer, UnlitPipelineKey) {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue, 64, 64);
        renderer.register_unlit_family();
        let key = UnlitPipelineKey::new(UnlitOptions::standard(&renderer.device));
        (renderer, key)
    }

    /// Register a 2D texture with `renderer` and return a view id and a
    /// sampler id, ready for [`Renderer::allocate_unlit_material`].
    fn test_material_resources(renderer: &mut Renderer) -> (ResourceId, ResourceId) {
        let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test::texture"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = renderer.register_texture(texture);
        let sampler = renderer.register_sampler(None);
        (view, sampler)
    }

    /// A variant of the standard one that reads no UV — so no base-color
    /// texture either — and therefore packs its vertices differently.
    fn uv_less_options(device: &wgpu::Device) -> UnlitOptions {
        use wgpu_unlit_render::pipeline::UnlitFlags;
        let mut options = UnlitOptions::standard(device);
        options.flags &= !(UnlitFlags::VERTEX_UV | UnlitFlags::BASE_COLOR_TEXTURE);
        options
    }

    #[test]
    fn registering_a_family_compiles_nothing() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue, 64, 64);

        renderer.register_unlit_family();

        // Registration records the family; its first concrete pipeline is
        // built only when a draw resolves a variant.
        assert!(renderer.pipelines.is_empty());
        assert_eq!(renderer.families.len(), 1);
    }

    #[test]
    fn a_renderer_starts_with_no_pipelines() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let renderer = Renderer::new(device, queue, 64, 64);

        // Nothing is privileged: pipelines arrive only through registration.
        assert!(renderer.pipelines.is_empty());
    }

    #[test]
    fn a_unlit_mesh_is_built_from_its_keys_options() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue, 64, 64);
        renderer.register_unlit_family();

        let standard = UnlitPipelineKey::new(UnlitOptions::standard(&renderer.device));
        let uv_less = UnlitPipelineKey::new(uv_less_options(&renderer.device));

        // Each mesh packs the channels its own key reads, not the last
        // registered family's.
        let standard_mesh = tri_mesh(&mut renderer, &standard);
        let uv_less_mesh = tri_mesh(&mut renderer, &uv_less);
        assert!(
            unlit_flags_for_layout(&standard_mesh.vertex_layout).contains(UnlitFlags::VERTEX_UV)
        );
        assert!(
            !unlit_flags_for_layout(&uv_less_mesh.vertex_layout).contains(UnlitFlags::VERTEX_UV)
        );

        // A material is built against the key's layout: the standard variant
        // samples a base-color texture, the UV-less one does not.
        let (view, sampler) = test_material_resources(&mut renderer);
        assert!(
            renderer
                .allocate_unlit_material(&standard, view, sampler)
                .is_some()
        );
        assert!(
            renderer
                .allocate_unlit_material(&uv_less, view, sampler)
                .is_none()
        );
    }

    #[test]
    fn every_registered_pipeline_gets_its_own_global_group() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let a = renderer.attachments.surface_key();
        // A second target, so the family resolves two variants.
        let b = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            depth_stencil_format: a.depth_stencil_format,
            sample_count: a.sample_count,
        };
        resolve_draw(&mut renderer, &key, a, &mesh);
        resolve_draw(&mut renderer, &key, b, &mesh);
        assert_eq!(renderer.pipelines.len(), 2);

        let ids: Vec<_> = renderer
            .pipelines
            .iter()
            .map(|registered| registered.global.as_ref().expect("has a global group").id)
            .collect();
        for (index, &id) in ids.iter().enumerate() {
            assert!(
                renderer.graph.get_bind_group(id).is_some(),
                "pipeline {index}'s global group is in the graph"
            );
            // No two pipelines share one: each binds its own layout.
            assert!(!ids[..index].contains(&id));
        }
    }

    #[test]
    fn a_variant_is_compiled_when_a_draw_resolves_it() {
        let (mut renderer, key) = noop_renderer();

        // A family has no concrete pipeline until a draw asks for a variant.
        let mesh = tri_mesh(&mut renderer, &key);
        let surface = renderer.attachments.surface_key();
        resolve_draw(&mut renderer, &key, surface, &mesh);

        assert_eq!(renderer.pipelines.len(), 1);
    }

    #[test]
    fn a_material_is_allocated_from_caller_owned_resources() {
        let (mut renderer, key) = noop_renderer();
        let (view, sampler) = test_material_resources(&mut renderer);

        let material = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        assert!(
            renderer
                .graph
                .get_bind_group(material.bind_group_id)
                .is_some()
        );
    }

    #[test]
    fn a_variant_without_a_base_color_texture_allocates_no_material() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue, 64, 64);
        renderer.register_unlit_family();
        let key = UnlitPipelineKey::new(uv_less_options(&renderer.device));
        let (view, sampler) = test_material_resources(&mut renderer);

        assert!(
            renderer
                .allocate_unlit_material(&key, view, sampler)
                .is_none()
        );
    }

    // -- draw ordering ------------------------------------------------------

    /// A camera at `eye` looking at the origin, with a frustum wide enough
    /// that nothing in these tests is culled.
    fn test_camera(eye: glam::Vec3) -> Camera {
        let view = glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y);
        Camera {
            clip_from_world: test_perspective() * view,
            position: eye,
        }
    }

    /// The entities of [`Renderer::visible_cache`] in draw order.
    fn drawn(renderer: &Renderer) -> Vec<Entity> {
        renderer
            .visible_cache
            .iter()
            .map(|entry| entry.mesh.entity)
            .collect()
    }

    /// A triangle mesh allocated on `renderer` for `key`.
    ///
    /// The standard key declares UV and vertex-colour channels, so the three
    /// slices must be present and of equal length.
    fn tri_mesh(renderer: &mut Renderer, key: &UnlitPipelineKey) -> GpuMesh {
        renderer.allocate_unlit_mesh(
            key,
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[[255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]),
            Some(&[0u32, 1, 2]),
        )
    }

    #[test]
    fn opaque_entities_are_drawn_before_transparent_ones() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();

        // The transparent entity is the nearer of the two, so depth alone
        // would put it first: it is drawn second because it is transparent.
        let transparent = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            Transparent,
        ));
        let opaque = world.spawn((Transform::default(), mesh.clone(), UnlitPipeline::new(key)));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.attachments.surface_key());

        assert_eq!(drawn(&renderer), vec![opaque, transparent]);
        assert!(renderer.visible_cache[0].depth > renderer.visible_cache[1].depth);
    }

    #[test]
    fn transparent_entities_are_drawn_back_to_front() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();

        let near = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            Transparent,
        ));
        let middle = world.spawn((
            Transform {
                translation: glam::Vec3::ZERO,
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            Transparent,
        ));
        let far = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, -4.0),
                ..Default::default()
            },
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            Transparent,
        ));
        // Spawned out of order on purpose: registration order is not draw
        // order for transparent entities.
        assert_ne!(drawn(&renderer), vec![far, middle, near]);

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.attachments.surface_key());

        assert_eq!(drawn(&renderer), vec![far, middle, near]);
    }

    #[test]
    fn entities_are_drawn_in_pipeline_id_order() {
        let (mut renderer, key) = noop_renderer();
        // A second key whose specialized options differ, so it resolves to its
        // own concrete pipeline.
        let uv_less_key = UnlitPipelineKey::new(uv_less_options(&renderer.device));

        // Warm both variants, first key first, so their pipeline ids follow
        // the order they were resolved in.
        let standard_mesh = tri_mesh(&mut renderer, &key);
        let uv_less_mesh = tri_mesh(&mut renderer, &uv_less_key);
        let surface = renderer.attachments.surface_key();
        let first_id = resolve_draw(&mut renderer, &key, surface, &standard_mesh);
        let second_id = resolve_draw(&mut renderer, &uv_less_key, surface, &uv_less_mesh);
        assert!(first_id < second_id);

        let mut world = LocalWorld::new();
        // Spawn the later-drawn entity first: draw order is the pipeline id,
        // not the spawn order.
        let second = world.spawn((
            Transform::default(),
            uv_less_mesh,
            UnlitPipeline::new(uv_less_key),
        ));
        let first = world.spawn((Transform::default(), standard_mesh, UnlitPipeline::new(key)));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, surface);

        assert_eq!(drawn(&renderer), vec![first, second]);
    }

    #[test]
    fn opaque_draws_sharing_a_material_stay_adjacent() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let (view, sampler) = test_material_resources(&mut renderer);
        let shared = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        let (view, sampler) = test_material_resources(&mut renderer);
        let other = renderer
            .allocate_unlit_material(&key, view, sampler)
            .expect("the standard variant reads a base-color texture");
        assert_ne!(shared.sort_key(), other.sort_key());

        let mut world = LocalWorld::new();
        let a = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            shared.clone(),
        ));
        // The odd one out: sharing no material with the other two, so it is
        // the draw that has to sit on the other side of the pair.
        let _other_material = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            other.clone(),
        ));
        let c = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            shared.clone(),
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.attachments.surface_key());

        // The two draws sharing a material are neighbours, so the renderer
        // binds its bind group once for the pair.
        let order = drawn(&renderer);
        assert_eq!(order.len(), 3);
        let (pos_a, pos_c) = (
            order.iter().position(|&e| e == a).expect("a is drawn"),
            order.iter().position(|&e| e == c).expect("c is drawn"),
        );
        assert_eq!(pos_a.abs_diff(pos_c), 1);
    }

    #[test]
    fn sorting_survives_a_second_frame_on_the_reused_cache() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();
        let opaque = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
        ));
        let transparent = world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key),
            Transparent,
        ));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.attachments.surface_key());
        assert_eq!(drawn(&renderer), vec![opaque, transparent]);

        // The cache is cleared and refilled, not reallocated.
        let capacity = renderer.visible_cache.capacity();
        renderer.collect_and_sort_visible(&world, &camera, renderer.attachments.surface_key());
        assert_eq!(drawn(&renderer), vec![opaque, transparent]);
        assert_eq!(renderer.visible_cache.capacity(), capacity);
    }

    // -- specialization cache ------------------------------------------------

    /// A raw-layout key for `mesh` on `surface`.
    fn draw_key(surface: SurfaceKey, mesh: &GpuMesh) -> DrawKey {
        DrawKey::for_mesh(surface, mesh)
    }

    /// Resolve one entity carrying `key` on `surface` and return the
    /// concrete pipeline it was drawn with.
    fn resolve_draw(
        renderer: &mut Renderer,
        key: &UnlitPipelineKey,
        surface: SurfaceKey,
        mesh: &GpuMesh,
    ) -> PipelineId {
        let mut world = LocalWorld::new();
        world.spawn((
            Transform::default(),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
        ));
        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, surface);
        renderer
            .visible_cache
            .last()
            .expect("the entity is visible")
            .pipeline_id
    }

    /// Force a variant for `mesh` on `surface` and return its pipeline id.
    fn resolve(
        renderer: &mut Renderer,
        key: &UnlitPipelineKey,
        surface: SurfaceKey,
        mesh: &GpuMesh,
    ) -> PipelineId {
        resolve_draw(renderer, key, surface, mesh)
    }

    #[test]
    fn a_render_target_change_respecializes_the_family() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);

        let internal = renderer.attachments.surface_key();
        let first = resolve(&mut renderer, &key, internal, &mesh);
        // The same key twice reuses one compiled pipeline.
        assert_eq!(resolve(&mut renderer, &key, internal, &mesh), first);

        // A different color format is a different surface, so the family
        // compiles a second variant for it.
        let other = SurfaceKey {
            color_format: wgpu::TextureFormat::Bgra8Unorm,
            ..internal
        };
        let second = resolve(&mut renderer, &key, other, &mesh);
        assert_ne!(second, first);
        assert_eq!(renderer.pipelines.len(), 2);
    }

    #[test]
    fn a_mesh_layout_change_respecializes_the_family() {
        let (mut renderer, key) = noop_renderer();
        let surface = renderer.attachments.surface_key();
        let standard = tri_mesh(&mut renderer, &key);
        let first = resolve(&mut renderer, &key, surface, &standard);

        // A second mesh whose layout declares fewer channels implies a
        // different variant. Dropping only the color attribute keeps the UV
        // the base-color flag needs, so the variant differs without
        // invalidating the base options.
        const COLOR: u32 = 2;
        let mut other = standard.clone();
        other.vertex_layout = other
            .vertex_layout
            .into_iter()
            .map(|(slot, mut layout)| {
                layout
                    .attributes
                    .retain(|attribute| attribute.shader_location != COLOR);
                (slot, layout)
            })
            .collect();
        assert_ne!(draw_key(surface, &other), draw_key(surface, &standard));
        let second = resolve(&mut renderer, &key, surface, &other);

        assert_ne!(second, first);
        assert_eq!(renderer.pipelines.len(), 2);
    }

    #[test]
    fn meshes_that_share_flags_share_one_variant() {
        let (mut renderer, key) = noop_renderer();
        let surface = renderer.attachments.surface_key();
        let base = tri_mesh(&mut renderer, &key);
        let first = resolve(&mut renderer, &key, surface, &base);

        // A layout the flags cannot tell from `base`: the attribute is at the
        // same location, so the derived flags are identical even though the
        // raw attribute list differs. The raw keys differ, so a canonical key
        // is what makes both resolve to one compiled pipeline.
        let mut twin = base.clone();
        twin.vertex_layout = twin
            .vertex_layout
            .into_iter()
            .map(|(slot, mut layout)| {
                for attribute in &mut layout.attributes {
                    attribute.offset += 4;
                }
                layout.array_stride += 4;
                (slot, layout)
            })
            .collect();
        assert_ne!(draw_key(surface, &twin), draw_key(surface, &base));

        let second = resolve(&mut renderer, &key, surface, &twin);
        assert_eq!(second, first);
        assert_eq!(renderer.pipelines.len(), 1);
    }

    #[test]
    fn an_entity_without_a_pipeline_is_not_drawn() {
        let (mut renderer, key) = noop_renderer();
        let mesh = tri_mesh(&mut renderer, &key);
        let mut world = LocalWorld::new();
        // No GpuPipeline: the query filters it out.
        let _unpipelined = world.spawn((Transform::default(), mesh.clone()));
        let drawn_entity = world.spawn((Transform::default(), mesh, UnlitPipeline::new(key)));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera, renderer.attachments.surface_key());

        assert_eq!(drawn(&renderer), vec![drawn_entity]);
    }

    #[test]
    fn registering_compiles_no_pipeline() {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut renderer = Renderer::new(device, queue, 64, 64);
        renderer.register_unlit_family();

        assert!(renderer.pipelines.is_empty());
        assert_eq!(renderer.families.len(), 1);
    }
}
