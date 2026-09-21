//! The top-level renderer — a resource entity component that owns GPU state and
//! drives rendering each frame from the ECS world.
//!
//! The [Renderer] is the bridge between [unlit_ecs] data and the
//! [wgpu_unlit_render] GPU pipeline. Spawn it once (typically as a resource
//! entity) and call [Renderer::render] every frame.

use core::cmp::Ordering;

use glam::Vec3;
use unlit_ecs::{Entity, LocalWorld};
use wgpu_unlit_render::globals::{Globals, View};
use wgpu_unlit_render::mesh::{
    MeshInfo, MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use wgpu_unlit_render::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_GROUP, MESH_INFO_BINDING,
    MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT, UnlitOptions, UnlitPipeline,
};
use wgpu_unlit_render::render_attachments::{
    AttachmentsInfo, RenderAttachments, default_depth_stencil_format,
};
use wgpu_unlit_render::resources::{Resource, ResourceGraph, ResourceId};
use wgpu_unlit_render::scene::{DrawRange, MaterialGroup, MeshDraw, PipelineGroup, Scene};
use zerocopy::IntoBytes;

use crate::components::{
    BoundingSphere, Camera, GpuMaterial, GpuMesh, GpuPipeline, InstanceData, Transform, Transparent,
};

/// Index into [`Renderer::pipelines`] of the pipeline [Renderer::new] builds
/// from its options.
///
/// Entities without a [GpuPipeline] component draw with it. The built-in
/// pipeline is an ordinary registered pipeline in the same list as the ones
/// [Renderer::create_unlit_pipeline] adds, so it is ordered — and looked up
/// — by index like any other.
pub const DEFAULT_PIPELINE_INDEX: u32 = 0;

/// The renderer: ECS resource component that holds GPU state and orchestrates
/// frame rendering.
///
/// Spawn this as a component on a resource entity in a [LocalWorld]. Call
/// [Renderer::render] each frame to draw every entity that carries both
/// a [GpuMesh] and a [Transform] (or [InstanceData]).
pub struct Renderer {
    /// WGPU device.
    pub device: wgpu::Device,
    /// WGPU queue.
    pub queue: wgpu::Queue,

    /// The UV-and-colour vertex stream descriptor that meshes are packed
    /// into, shared by every registered pipeline.
    ///
    /// A pipeline whose variant packs its vertices differently cannot draw
    /// the meshes already uploaded, so [Renderer::create_unlit_pipeline]
    /// requires a matching layout.
    pub uv_color_stream: wgpu_unlit_render::mesh::MeshUvColorStream,

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

    /// A reused instance-data buffer, grown as needed.
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: u32,

    /// Temporary attachments set for external-target rendering.
    external_attachments: Option<RenderAttachments>,

    /// Every pipeline registered with this renderer, in registration order.
    ///
    /// Index [`DEFAULT_PIPELINE_INDEX`] is the one [Renderer::new] built
    /// from the options it was given; entities without a [GpuPipeline]
    /// component draw with it.
    pipelines: Vec<RegisteredPipeline>,

    // -- cached per-frame allocations ------------------------------------------
    /// Reused Vec for visible-entity collection and per-frame sorting.
    visible_cache: Vec<VisibleEntry>,
    /// Reused Vec for packed instance data.
    packed_instances_cache: Vec<MeshInstance>,
    /// Reused Vec for cloned bind groups while the scene is built.
    bind_group_cache: Vec<wgpu::BindGroup>,
    /// Reused Vec for cloned buffers while the scene is built.
    buffer_cache: Vec<wgpu::Buffer>,
}

/// A visible entity awaiting its draw command, tagged with everything the
/// scene builder needs to group it.
///
/// Entries are sorted once per frame (see [`Renderer::render`]) so that a
/// single linear pass can emit every [`PipelineGroup`] and [`MaterialGroup`]
/// without intermediate scratch buffers: opaque entries first, keyed by
/// material so shared-state draws stay adjacent, then transparent entries
/// keyed by camera distance so they are drawn back-to-front.
struct VisibleEntry {
    entity: Entity,
    instance: MeshInstance,
    /// Index into [`Renderer::pipelines`].
    pipeline_index: u32,
    /// Groups opaque draws by material, so neighbours share a bind group.
    /// Ignored for transparent entries.
    sort_key: u64,
    /// Distance to the camera, used to order transparent entries back-to-front.
    /// Ignored for opaque entries.
    depth: f32,
    /// True when the entity carries [`Transparent`].
    transparent: bool,
}

/// A pipeline registered with the renderer.
struct RegisteredPipeline {
    /// The compiled pipeline and the variant it was built for.
    pipeline: UnlitPipeline,
    /// The bind group for the global group (index 0), built from this
    /// pipeline's layout.
    global_group: ResourceId,
}

impl Renderer {
    /// Build a new renderer and its GPU resources.
    ///
    /// `options` selects the shader variant. `width` and `height` are the
    /// initial viewport size in physical pixels.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        options: UnlitOptions,
        width: u32,
        height: u32,
    ) -> Self {
        let uv_color_stream = options.uv_color_stream();

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

        let mut renderer = Self {
            device,
            queue,
            uv_color_stream,
            graph,
            attachments,
            camera_buf,
            globals_buf,
            metadata_buf,
            globals,
            metadata: Vec::new(),
            instance_buffer: None,
            instance_capacity: 0,
            external_attachments: None,
            pipelines: Vec::new(),
            visible_cache: Vec::new(),
            packed_instances_cache: Vec::new(),
            bind_group_cache: Vec::new(),
            buffer_cache: Vec::new(),
        };

        // The built-in pipeline: registered like any other, so entities that
        // name no pipeline fall back to DEFAULT_PIPELINE_INDEX by default.
        renderer.register_pipeline(options);
        renderer
    }

    /// Resize the render target.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.attachments = RenderAttachments::new(
            &self.device,
            AttachmentsInfo::new(&self.device, width, height),
        );
    }

    /// Allocate GPU buffers for raw mesh data and return a [GpuMesh] handle.
    ///
    /// The mesh is ready to draw immediately. Positions, UVs and colours are
    /// compressed to the compact vertex formats the registered pipelines
    /// expect, so every variant of the unlit shader can draw it.
    ///
    /// # Panics
    ///
    /// If the input slices are empty or of mismatched length (see the
    /// compressors in [wgpu_unlit_render::mesh]).
    pub fn allocate_mesh(
        &mut self,
        positions: &[[f32; 3]],
        uvs: Option<&[[f32; 2]]>,
        colors: Option<&[[f32; 4]]>,
        indices: Option<&[u32]>,
    ) -> GpuMesh {
        // Compress vertex streams.
        let mut meta = MeshMetadata::default();
        let packed_positions: Vec<_> = compress_positions(positions, &mut meta).collect();
        let vertex_count = packed_positions.len();

        // Position vertex buffer.
        let pos_stride = size_of::<wgpu_unlit_render::mesh::CompressedPosition>();
        let position_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::position"),
            size: (vertex_count * pos_stride) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&position_buf, 0, packed_positions.as_bytes());
        let position_id = self
            .graph
            .insert(Resource::Buffer(position_buf), &[])
            .expect("position buffer has no dependencies");

        // UV and colour vertex buffer (interleaved via stream).
        use wgpu::WriteOnly;
        let uv_color_len = self.uv_color_stream.byte_len(vertex_count);
        let uv_color_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::uv_color"),
            size: uv_color_len as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if uv_color_len > 0 {
            let mut staging = vec![0u8; uv_color_len];
            self.uv_color_stream.write(
                uvs.unwrap_or(&[]),
                colors.unwrap_or(&[]),
                &mut meta,
                WriteOnly::from_mut(staging.as_mut_slice()),
            );
            self.queue.write_buffer(&uv_color_buf, 0, &staging);
        }
        let uv_color_id = self
            .graph
            .insert(Resource::Buffer(uv_color_buf), &[])
            .expect("uv_color buffer has no dependencies");

        // Append metadata entry.
        let metadata_index = self.metadata.len() as u32;
        self.metadata.push(meta);
        self.rebuild_metadata_buffer();

        // MeshInfo uniform buffer (just the metadata index).
        let mesh_info = MeshInfo::new(metadata_index);
        let mesh_info_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::info"),
            size: size_of::<MeshInfo>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&mesh_info_buf, 0, mesh_info.as_bytes());
        let mesh_info_id = self
            .graph
            .insert(Resource::Buffer(mesh_info_buf), &[])
            .expect("mesh_info buffer has no dependencies");

        // Mesh bind group (MESH_GROUP).
        let mesh_layout = self
            .default_pipeline()
            .mesh_layout
            .as_ref()
            .expect("the default pipeline has a mesh layout when metadata is needed");
        let mesh_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::mesh::bind_group"),
            layout: mesh_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: MESH_INFO_BINDING,
                resource: self
                    .graph
                    .get_buffer(mesh_info_id)
                    .expect("mesh_info buffer exists")
                    .as_entire_binding(),
            }],
        });
        let mesh_bind_group_id = self
            .graph
            .insert(Resource::BindGroup(mesh_bind_group), &[mesh_info_id])
            .expect("mesh_info buffer is a dependency");

        // Index buffer (optional).  The format is chosen automatically:
        // Uint16 when every index fits, otherwise Uint32.
        let (index_buffer, count, indexed) = match indices {
            Some(indices) if !indices.is_empty() => {
                // Number of indices before packing.
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
                let id = self
                    .graph
                    .insert(Resource::Buffer(buf), &[])
                    .expect("index buffer has no dependencies");
                (Some((id, format)), index_count, true)
            }
            _ => (None, vertex_count as u32, false),
        };

        GpuMesh {
            bind_group_id: mesh_bind_group_id,
            vertex_buffers: vec![(POSITION_SLOT, position_id), (UV_COLOR_SLOT, uv_color_id)],
            index_buffer,
            count,
            indexed,
        }
    }

    /// Allocate a material bind group for `texture` and return a
    /// [GpuMaterial] handle.
    ///
    /// The caller creates and fills the texture; this takes ownership of it
    /// and inserts it into the renderer's resource graph together with a view
    /// and a sampler, so the material is ready to use immediately.
    ///
    /// The bind group is built from the default pipeline's material layout,
    /// so the texture must be a 2D texture that layout can sample — one from
    /// [Renderer::allocate_unlit_texture], say. Returns `None` when that
    /// pipeline's variant reads no base-color texture and so has no material
    /// layout to bind against.
    pub fn allocate_unlit_material(&mut self, texture: wgpu::Texture) -> Option<GpuMaterial> {
        let material_layout = self.default_pipeline().material_layout.clone()?;

        let texture_id = self
            .graph
            .insert(Resource::Texture(texture), &[])
            .expect("texture has no dependencies");

        let view = self
            .graph
            .get_texture(texture_id)
            .expect("texture exists")
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_id = self
            .graph
            .insert(Resource::TextureView(view), &[texture_id])
            .expect("texture is a dependency");

        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("unlit3d::material::sampler"),
            ..Default::default()
        });
        let sampler_id = self
            .graph
            .insert(Resource::Sampler(sampler), &[])
            .expect("sampler has no dependencies");

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::material::bind_group"),
            layout: &material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(
                        self.graph.get_texture_view(view_id).expect("view exists"),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(
                        self.graph.get_sampler(sampler_id).expect("sampler exists"),
                    ),
                },
            ],
        });
        let bind_group_id = self
            .graph
            .insert(Resource::BindGroup(bind_group), &[view_id, sampler_id])
            .expect("view and sampler are dependencies");

        Some(GpuMaterial { bind_group_id })
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

        // Collect, cull and sort the visible set in one pass. The cache keeps
        // its allocation between frames, so a steady scene allocates nothing.
        self.collect_and_sort_visible(world, &camera);
        if self.visible_cache.is_empty() {
            self.clear_frame(target);
            return;
        }
        let instance_count = self.visible_cache.len() as u32;

        // Pack instance data into the reused scratch buffer and upload it.
        self.ensure_instance_buffer(instance_count);
        self.packed_instances_cache.clear();
        self.packed_instances_cache
            .extend(self.visible_cache.iter().map(|entry| entry.instance));
        self.queue.write_buffer(
            self.instance_buffer
                .as_ref()
                .expect("instance buffer exists"),
            0,
            self.packed_instances_cache.as_bytes(),
        );

        // Collect wgpu handles from the graph before building the scene,
        // so prep_attachments can take &mut self later.
        let instance_buf = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();

        // Every registered pipeline's render handle and global bind group,
        // indexed the same way [`Renderer::pipelines`] is.
        struct PipelineRes {
            handle: wgpu::RenderPipeline,
            global_bg: wgpu::BindGroup,
        }
        let pipeline_res: Vec<PipelineRes> = self
            .pipelines
            .iter()
            .map(|registered| PipelineRes {
                handle: registered.pipeline.pipeline.clone(),
                global_bg: self
                    .graph
                    .get_bind_group(registered.global_group)
                    .expect("global group exists")
                    .clone(),
            })
            .collect();

        // Build the scene in two passes over the sorted entries. The first
        // fills the scratch caches with the handles the draws need, the second
        // borrows those handles to emit the draw commands; splitting them is
        // what lets the borrow checker see that nothing mutates the caches
        // while the scene still borrows them.
        //
        // The caches are taken out of self instead of freshly allocated, so a
        // steady scene allocates nothing per frame.
        let mut bind_group_cache = std::mem::take(&mut self.bind_group_cache);
        let mut buffer_cache = std::mem::take(&mut self.buffer_cache);
        bind_group_cache.clear();
        buffer_cache.clear();

        // Per entry: the index into bind_group_cache of its mesh and material
        // bind groups, and the range in buffer_cache holding its vertex
        // buffers followed by its optional index buffer.
        struct EntryHandles {
            mesh_bg: usize,
            material_bg: Option<usize>,
            vertex_start: usize,
            index_buffer: Option<(usize, wgpu::IndexFormat)>,
        }
        let mut handles: Vec<EntryHandles> = Vec::with_capacity(self.visible_cache.len());

        {
            let graph_ref = &self.graph;
            for entry in &self.visible_cache {
                let mesh = world
                    .get::<GpuMesh>(entry.entity)
                    .expect("visible entity has GpuMesh");

                bind_group_cache.push(
                    graph_ref
                        .get_bind_group(mesh.bind_group_id)
                        .expect("mesh bind group exists")
                        .clone(),
                );
                let mesh_bg = bind_group_cache.len() - 1;

                let material_bg = match world.get::<GpuMaterial>(entry.entity) {
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

        let mut scene = Scene::new();

        // The pipeline group being filled, and which pipeline opened it.
        let mut open_pipeline: Option<u32> = None;
        let mut open_pg: Option<PipelineGroup> = None;
        // The material group being filled, and which material opened it.
        let mut open_material: Option<ResourceId> = None;
        let mut material_open = false;
        let mut open_mg: Option<MaterialGroup> = None;

        for (draw_idx, entry) in self.visible_cache.iter().enumerate() {
            let mesh = world
                .get::<GpuMesh>(entry.entity)
                .expect("visible entity has GpuMesh");
            let material = world
                .get::<GpuMaterial>(entry.entity)
                .map(|material| material.bind_group_id);
            let handle = &handles[draw_idx];

            // -- pipeline change: close the open groups, open a new one -----
            if Some(entry.pipeline_index) != open_pipeline {
                if let Some(mg) = open_mg.take() {
                    let pg = open_pg.take().expect("a pipeline group is open");
                    open_pg = Some(pg.with_material(mg));
                }
                if let Some(pg) = open_pg.take() {
                    scene.push(pg);
                }
                open_material = None;
                material_open = false;
                let res = &pipeline_res[entry.pipeline_index as usize];
                open_pg = Some(
                    PipelineGroup::new(&res.handle).with_bind_group(GLOBAL_GROUP, &res.global_bg),
                );
                open_pipeline = Some(entry.pipeline_index);
            }

            // -- material change: close the material group, open a new one --
            if !material_open || material != open_material {
                if let Some(mg) = open_mg.take() {
                    let pg = open_pg.take().expect("a pipeline group is open");
                    open_pg = Some(pg.with_material(mg));
                }
                let mut mg = MaterialGroup::new();
                if let Some(index) = handle.material_bg {
                    let bind_group = &bind_group_cache[index];
                    mg = mg.with_bind_group(MATERIAL_GROUP, bind_group);
                }
                open_mg = Some(mg);
                open_material = material;
                material_open = true;
            }

            // -- the draw itself --------------------------------------------
            let instance_range = (draw_idx as u32)..(draw_idx as u32 + 1);
            let range = if mesh.indexed {
                DrawRange::indexed(0..mesh.count).with_instances(instance_range)
            } else {
                DrawRange::vertices(0..mesh.count).with_instances(instance_range)
            };
            let mut draw =
                MeshDraw::new(range).with_bind_group(MESH_GROUP, &bind_group_cache[handle.mesh_bg]);
            for (offset, &(slot, _)) in mesh.vertex_buffers.iter().enumerate() {
                let buffer = &buffer_cache[handle.vertex_start + offset];
                draw = draw.with_vertex_buffer(slot, buffer.slice(..));
            }
            if let Some((index, format)) = handle.index_buffer {
                let buffer = &buffer_cache[index];
                draw = draw.with_index_buffer(buffer.slice(..), format);
            }
            draw = draw.with_vertex_buffer(INSTANCE_SLOT, instance_buf.slice(..));

            let mg = open_mg.take().expect("a material group is open");
            open_mg = Some(mg.with_mesh(draw));
        }

        // Close the groups left open by the last entry.
        if let Some(mg) = open_mg.take() {
            let pg = open_pg.take().expect("a pipeline group is open");
            open_pg = Some(pg.with_material(mg));
        }
        if let Some(pg) = open_pg.take() {
            scene.push(pg);
        }

        // Record and submit the pass.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

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

        // The scene is recorded, so nothing borrows the caches any more: put
        // them back, allocations and all, ready for the next frame.
        self.bind_group_cache = bind_group_cache;
        self.buffer_cache = buffer_cache;

        self.queue.submit([encoder.finish()]);
    }
    // -- internal helpers ----------------------------------------------------

    /// The pipeline entities draw with when they carry no [GpuPipeline]:
    /// the one at [`DEFAULT_PIPELINE_INDEX`].
    ///
    /// Its layouts are what [Renderer::allocate_mesh] packs vertices for and
    /// what [Renderer::allocate_unlit_material] binds a texture against.
    pub fn default_pipeline(&self) -> &UnlitPipeline {
        &self.pipelines[DEFAULT_PIPELINE_INDEX as usize].pipeline
    }

    /// Create a texture for [`Renderer::allocate_unlit_material`] to bind.
    ///
    /// The texture is a 2D one with no mips, sampled by the material bind
    /// group and writable by [`wgpu::Queue::write_texture`]. The caller
    /// creates the texture so it can pick its own size, format and usage —
    /// a render target to render into, say — rather than being given one
    /// this renderer chose.
    pub fn allocate_unlit_texture(
        &self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("unlit3d::material::texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Compile an unlit pipeline variant, register it with the renderer and
    /// return a handle to it.
    ///
    /// The pipeline shares the renderer's camera, globals and mesh-metadata
    /// buffers, and draws the same meshes as every other registered
    /// pipeline. Entities that carry the returned [`GpuPipeline`] component
    /// draw with this pipeline instead of the default one.
    ///
    /// The returned index is the pipeline's position in the registration
    /// order [`Renderer::render`] sorts by, so pipelines register in the
    /// order you want them drawn.
    ///
    /// # Panics
    ///
    /// If `options` packs its vertices differently from the variant the
    /// renderer was built with: meshes are already uploaded in that layout,
    /// and a pipeline expecting another cannot draw them.
    pub fn create_unlit_pipeline(&mut self, options: UnlitOptions) -> GpuPipeline {
        assert_eq!(
            options.uv_color_stream(),
            self.uv_color_stream,
            "the new pipeline must pack its vertices like the meshes already uploaded"
        );
        GpuPipeline {
            pipeline_index: self.register_pipeline(options),
        }
    }

    /// Build `options`' pipeline, register it and return its index.
    fn register_pipeline(&mut self, options: UnlitOptions) -> u32 {
        let pipeline = UnlitPipeline::new(&self.device, &options);
        let global_group = self.create_global_group(&pipeline, &options);
        self.pipelines.push(RegisteredPipeline {
            pipeline,
            global_group,
        });
        // Length before the push is the index the pipeline landed on.
        (self.pipelines.len() - 1) as u32
    }

    /// Build the global bind group `pipeline` needs, insert it into the
    /// resource graph keyed on the buffers the variant binds, and return its
    /// id.
    fn create_global_group(
        &mut self,
        pipeline: &UnlitPipeline,
        options: &UnlitOptions,
    ) -> ResourceId {
        let bind_group = self.build_global_bind_group(pipeline, options);

        let mut deps = vec![self.camera_buf, self.globals_buf];
        if options.needs_metadata() {
            deps.push(self.metadata_buf);
        }
        self.graph
            .insert(Resource::BindGroup(bind_group), &deps)
            .expect("camera, globals and metadata buffers exist")
    }

    /// Bind the camera, globals and — for variants that read a compressed
    /// channel — metadata buffers to `pipeline`'s global layout.
    fn build_global_bind_group(
        &self,
        pipeline: &UnlitPipeline,
        options: &UnlitOptions,
    ) -> wgpu::BindGroup {
        let camera_buf = self.graph.get_buffer(self.camera_buf).expect("camera buf");
        let globals_buf = self
            .graph
            .get_buffer(self.globals_buf)
            .expect("globals buf");
        let meta_buf = options
            .needs_metadata()
            .then(|| self.graph.get_buffer(self.metadata_buf).expect("meta buf"));
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: CAMERA_BINDING,
                resource: camera_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: FRAME_BINDING,
                resource: globals_buf.as_entire_binding(),
            },
        ];
        if let Some(meta) = meta_buf {
            entries.push(wgpu::BindGroupEntry {
                binding: MESH_METADATA_BINDING,
                resource: meta.as_entire_binding(),
            });
        }
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::global"),
            layout: &pipeline.global_layout,
            entries: &entries,
        })
    }

    /// Rebuild the global bind group of every registered pipeline whose
    /// dependencies changed.
    ///
    /// The new bind groups are built before any is written back: building one
    /// borrows the whole renderer, so collecting first is what lets the graph
    /// be mutated afterwards.
    fn rebuild_dirty_global_groups(&mut self) {
        let rebuilt: Vec<_> = self
            .pipelines
            .iter()
            .filter(|registered| self.graph.is_dirty(registered.global_group))
            .map(|registered| {
                let bind_group = self
                    .build_global_bind_group(&registered.pipeline, &registered.pipeline.options);
                (registered.global_group, bind_group)
            })
            .collect();

        for (global_group, bind_group) in rebuilt {
            self.graph
                .replace(global_group, Resource::BindGroup(bind_group))
                .expect("global group exists");
            self.graph.mark_clean(global_group);
        }
    }

    /// Resize and rewrite the metadata storage buffer.
    fn rebuild_metadata_buffer(&mut self) {
        if self.metadata.is_empty() {
            return;
        }
        let size = (self.metadata.len() * size_of::<MeshMetadata>()) as u64;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh_metadata"),
            size: size.max(size_of::<MeshMetadata>() as u64),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buf, 0, self.metadata.as_bytes());

        self.graph
            .replace(self.metadata_buf, Resource::Buffer(buf))
            .expect("metadata buffer exists");

        self.rebuild_dirty_global_groups();
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

    /// Collect entities that pass frustum culling and build their per-instance
    /// data.
    /// Collect entities that pass frustum culling, then sort them for drawing.
    ///
    /// Results land in [`Renderer::visible_cache`], whose allocation is reused
    /// between frames. The ordering is the whole point of this pass: opaque
    /// entities first, grouped by pipeline and then by material so a draw
    /// never re-binds state a neighbour already set; transparent entities
    /// after them, sorted back-to-front by camera distance so blending is
    /// order-independent.
    fn collect_and_sort_visible(&mut self, world: &LocalWorld, camera: &Camera) {
        let frustum = FrustumPlanes::from_clip_from_world(camera.clip_from_world);
        self.visible_cache.clear();

        // Query entities with (Transform, GpuMesh).
        for (entity, (transform, _mesh)) in world.query::<(&Transform, &GpuMesh)>() {
            let tf = copy_transform(&transform);
            let instance = match world.get::<InstanceData>(entity) {
                Some(data) => MeshInstance::new(data.matrix, data.base_color),
                None => MeshInstance::new(
                    glam::Affine3A::from_mat4(tf.compute_matrix()),
                    glam::Vec4::new(1.0, 1.0, 1.0, 1.0),
                ),
            };

            // Frustum culling.
            if let Some(sphere) = world.get::<BoundingSphere>(entity) {
                let mat = affine_from_instance(&instance.model);
                let world_center = mat.transform_point3(sphere.center);
                if !frustum.test_sphere(world_center, sphere.radius) {
                    continue;
                }
            }

            self.push_visible(world, camera, entity, instance);
        }

        // Entities with InstanceData but without Transform.
        for (entity, (instance_data, _mesh)) in world.query::<(&InstanceData, &GpuMesh)>() {
            if world.has::<Transform>(entity) {
                continue;
            }
            let instance = MeshInstance::new(instance_data.matrix, instance_data.base_color);

            if let Some(sphere) = world.get::<BoundingSphere>(entity) {
                let world_center = instance_data.matrix.transform_point3(sphere.center);
                if !frustum.test_sphere(world_center, sphere.radius) {
                    continue;
                }
            }

            self.push_visible(world, camera, entity, instance);
        }

        // Sort: opaque before transparent, then by pipeline, then by the key
        // that matters for that kind. Opaque draws are keyed by material so
        // neighbours share a bind group; transparent ones by camera distance
        // so they are composited back-to-front.
        self.visible_cache.sort_unstable_by(|a, b| {
            a.transparent
                .cmp(&b.transparent)
                .then_with(|| a.pipeline_index.cmp(&b.pipeline_index))
                .then_with(|| {
                    if a.transparent {
                        // Back-to-front: the farthest entity is drawn first.
                        b.depth.partial_cmp(&a.depth).unwrap_or(Ordering::Equal)
                    } else {
                        a.sort_key.cmp(&b.sort_key)
                    }
                })
        });
    }

    /// Push one entity into [`Renderer::visible_cache`] with its sort key.
    fn push_visible(
        &mut self,
        world: &LocalWorld,
        camera: &Camera,
        entity: Entity,
        instance: MeshInstance,
    ) {
        let transparent = world.has::<Transparent>(entity);
        let pipeline_index = world
            .get::<GpuPipeline>(entity)
            .map_or(DEFAULT_PIPELINE_INDEX, |pipeline| pipeline.pipeline_index);
        let centre = glam::Vec3A::new(
            instance.model[0].w,
            instance.model[1].w,
            instance.model[2].w,
        );
        let depth = (centre - glam::Vec3A::from(camera.position)).length();
        let sort_key = match world.get::<GpuMaterial>(entity) {
            Some(material) => material.sort_key(),
            None => 0,
        };
        self.visible_cache.push(VisibleEntry {
            entity,
            instance,
            pipeline_index,
            sort_key,
            depth,
            transparent,
        });
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
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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

/// Reconstruct an [glam::Affine3A] from [MeshInstance::model]'s packed columns.
fn affine_from_instance(model: &[glam::Vec4; 3]) -> glam::Affine3A {
    let c0: glam::Vec3A = model[0].truncate().into();
    let c1: glam::Vec3A = model[1].truncate().into();
    let c2: glam::Vec3A = model[2].truncate().into();
    glam::Affine3A {
        matrix3: glam::Mat3A::from_cols(c0, c1, c2),
        translation: glam::Vec3A::new(model[0].w, model[1].w, model[2].w),
    }
}

/// Copy a camera out of its ECS cell so the borrow does not block world access.
fn copy_camera(cam: &Camera) -> Camera {
    Camera {
        clip_from_world: cam.clip_from_world,
        position: cam.position,
    }
}

/// Copy a transform out of its ECS cell.
fn copy_transform(tf: &Transform) -> Transform {
    Transform {
        translation: tf.translation,
        rotation: tf.rotation,
        scale: tf.scale,
    }
}

/// Six frustum planes derived from a clip-space matrix.
struct FrustumPlanes {
    planes: [glam::Vec4; 6],
}

impl FrustumPlanes {
    /// Extract frustum planes from a clip-from-world matrix.
    fn from_clip_from_world(clip_from_world: glam::Mat4) -> Self {
        let m = clip_from_world;
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        Self {
            planes: [
                Self::normalize_plane(r3 + r0),
                Self::normalize_plane(r3 - r0),
                Self::normalize_plane(r3 - r1),
                Self::normalize_plane(r3 + r1),
                Self::normalize_plane(r3 + r2),
                Self::normalize_plane(r3 - r2),
            ],
        }
    }

    fn normalize_plane(row: glam::Vec4) -> glam::Vec4 {
        let len = glam::Vec3::new(row.x, row.y, row.z).length();
        if len > 1e-10 { row / len } else { row }
    }

    /// Test sphere-frustum intersection. Returns true when partially visible.
    fn test_sphere(&self, center: Vec3, radius: f32) -> bool {
        for &plane in &self.planes {
            if plane.x * center.x + plane.y * center.y + plane.z * center.z + plane.w < -radius {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_perspective() -> glam::Mat4 {
        glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0)
    }

    #[test]
    fn frustum_planes_from_identity_accepts_origin() {
        let planes = FrustumPlanes::from_clip_from_world(glam::Mat4::IDENTITY);
        assert!(planes.test_sphere(Vec3::ZERO, 0.1));
    }

    #[test]
    fn frustum_culls_outside_sphere() {
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let clip_from_world = test_perspective() * view;
        let planes = FrustumPlanes::from_clip_from_world(clip_from_world);
        assert!(planes.test_sphere(Vec3::new(0.0, 0.0, 0.0), 0.5));
        assert!(!planes.test_sphere(Vec3::new(0.0, 0.0, 20.0), 0.5));
    }

    #[test]
    fn frustum_culls_sphere_outside_left_plane() {
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let clip_from_world = test_perspective() * view;
        let planes = FrustumPlanes::from_clip_from_world(clip_from_world);
        assert!(!planes.test_sphere(Vec3::new(-10.0, 0.0, 0.0), 0.1));
    }

    #[test]
    fn mesh_instance_from_transform() {
        let t = Transform {
            translation: glam::Vec3::new(1.0, 2.0, 3.0),
            rotation: glam::Quat::IDENTITY,
            scale: glam::Vec3::ONE,
        };
        let matrix = glam::Affine3A::from_mat4(t.compute_matrix());
        let instance = MeshInstance::new(matrix, glam::Vec4::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(instance.model[0].w, 1.0);
        assert_eq!(instance.model[1].w, 2.0);
        assert_eq!(instance.model[2].w, 3.0);
    }

    // -- pipeline registration ---------------------------------------------

    /// A renderer on wgpu's noop backend, which stubs every GPU operation
    /// out and so needs no adapter.
    fn noop_renderer() -> Renderer {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let options = UnlitOptions::standard(&device);
        Renderer::new(device, queue, options, 64, 64)
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
    fn the_builtin_pipeline_is_registered_at_the_default_index() {
        let renderer = noop_renderer();

        // It sits in the same list as the ones created later, with no field
        // of its own on the renderer.
        assert_eq!(renderer.pipelines.len(), 1);
        assert_eq!(
            renderer.default_pipeline().options,
            UnlitOptions::standard(&renderer.device)
        );
        assert_eq!(
            renderer.default_pipeline().options.uv_color_stream(),
            renderer.uv_color_stream
        );
    }

    #[test]
    fn created_pipelines_are_appended_in_registration_order() {
        let mut renderer = noop_renderer();

        let first = renderer.create_unlit_pipeline(UnlitOptions::standard(&renderer.device));
        let second = renderer.create_unlit_pipeline(UnlitOptions::standard(&renderer.device));

        // The default pipeline keeps index 0 and is drawn first; later ones
        // follow in the order they were created.
        assert_eq!(first.pipeline_index, DEFAULT_PIPELINE_INDEX + 1);
        assert_eq!(second.pipeline_index, DEFAULT_PIPELINE_INDEX + 2);
        assert_eq!(renderer.pipelines.len(), 3);
    }

    #[test]
    fn every_registered_pipeline_gets_its_own_global_group() {
        let mut renderer = noop_renderer();
        renderer.create_unlit_pipeline(UnlitOptions::standard(&renderer.device));

        let ids: Vec<_> = renderer
            .pipelines
            .iter()
            .map(|registered| registered.global_group)
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
    #[should_panic(expected = "pack its vertices like the meshes")]
    fn a_pipeline_packing_vertices_differently_is_rejected() {
        let mut renderer = noop_renderer();
        let options = uv_less_options(&renderer.device);
        let _ = renderer.create_unlit_pipeline(options);
    }

    #[test]
    fn a_material_is_allocated_from_a_caller_owned_texture() {
        let mut renderer = noop_renderer();
        let texture = renderer.allocate_unlit_texture(4, 4, wgpu::TextureFormat::Rgba8UnormSrgb);

        let material = renderer
            .allocate_unlit_material(texture)
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
        let options = uv_less_options(&device);
        let mut renderer = Renderer::new(device, queue, options, 64, 64);
        let texture = renderer.allocate_unlit_texture(4, 4, wgpu::TextureFormat::Rgba8UnormSrgb);

        assert!(renderer.allocate_unlit_material(texture).is_none());
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
            .map(|entry| entry.entity)
            .collect()
    }

    /// A triangle mesh allocated on `renderer`.
    ///
    /// The standard variant declares UV and vertex-colour channels, so the
    /// three slices must be present and of equal length.
    fn tri_mesh(renderer: &mut Renderer) -> GpuMesh {
        renderer.allocate_mesh(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            Some(&[
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
            ]),
            Some(&[0u32, 1, 2]),
        )
    }

    #[test]
    fn opaque_entities_are_drawn_before_transparent_ones() {
        let mut renderer = noop_renderer();
        let mesh = tri_mesh(&mut renderer);
        let mut world = LocalWorld::new();

        // The transparent entity is the nearer of the two, so depth alone
        // would put it first: it is drawn second because it is transparent.
        let transparent = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            Transparent,
        ));
        let opaque = world.spawn((Transform::default(), mesh.clone()));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera);

        assert_eq!(drawn(&renderer), vec![opaque, transparent]);
        assert!(renderer.visible_cache[0].depth > renderer.visible_cache[1].depth);
    }

    #[test]
    fn transparent_entities_are_drawn_back_to_front() {
        let mut renderer = noop_renderer();
        let mesh = tri_mesh(&mut renderer);
        let mut world = LocalWorld::new();

        let near = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, 4.0),
                ..Default::default()
            },
            mesh.clone(),
            Transparent,
        ));
        let middle = world.spawn((
            Transform {
                translation: glam::Vec3::ZERO,
                ..Default::default()
            },
            mesh.clone(),
            Transparent,
        ));
        let far = world.spawn((
            Transform {
                translation: glam::Vec3::new(0.0, 0.0, -4.0),
                ..Default::default()
            },
            mesh.clone(),
            Transparent,
        ));
        // Spawned out of order on purpose: registration order is not draw
        // order for transparent entities.
        assert_ne!(drawn(&renderer), vec![far, middle, near]);

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera);

        assert_eq!(drawn(&renderer), vec![far, middle, near]);
    }

    #[test]
    fn entities_are_drawn_in_pipeline_registration_order() {
        let mut renderer = noop_renderer();
        let late = renderer.create_unlit_pipeline(UnlitOptions::standard(&renderer.device));
        let early = renderer.create_unlit_pipeline(UnlitOptions::standard(&renderer.device));
        // `early` registered later, so it draws later — the order is the
        // index order, not the order the handles happen to be used in.
        assert!(early.pipeline_index > late.pipeline_index);

        let mesh = tri_mesh(&mut renderer);
        let mut world = LocalWorld::new();
        let second = world.spawn((Transform::default(), mesh.clone(), early));
        let first = world.spawn((Transform::default(), mesh.clone(), late));
        let unassigned = world.spawn((Transform::default(), mesh.clone()));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera);

        // The default pipeline is at DEFAULT_PIPELINE_INDEX, so it draws
        // before both of the others.
        assert_eq!(drawn(&renderer), vec![unassigned, first, second]);
    }

    #[test]
    fn opaque_draws_sharing_a_material_stay_adjacent() {
        let mut renderer = noop_renderer();
        let mesh = tri_mesh(&mut renderer);
        let shared = renderer
            .allocate_unlit_material(renderer.allocate_unlit_texture(
                4,
                4,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ))
            .expect("the standard variant reads a base-color texture");
        let other = renderer
            .allocate_unlit_material(renderer.allocate_unlit_texture(
                4,
                4,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ))
            .expect("the standard variant reads a base-color texture");
        assert_ne!(shared.sort_key(), other.sort_key());

        let mut world = LocalWorld::new();
        let a = world.spawn((Transform::default(), mesh.clone(), shared.clone()));
        // The odd one out: sharing no material with the other two, so it is
        // the draw that has to sit on the other side of the pair.
        let _other_material = world.spawn((Transform::default(), mesh.clone(), other.clone()));
        let c = world.spawn((Transform::default(), mesh.clone(), shared.clone()));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera);

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
        let mut renderer = noop_renderer();
        let mesh = tri_mesh(&mut renderer);
        let mut world = LocalWorld::new();
        let opaque = world.spawn((Transform::default(), mesh.clone()));
        let transparent = world.spawn((Transform::default(), mesh.clone(), Transparent));

        let camera = test_camera(glam::Vec3::new(0.0, 0.0, 5.0));
        renderer.collect_and_sort_visible(&world, &camera);
        assert_eq!(drawn(&renderer), vec![opaque, transparent]);

        // The cache is cleared and refilled, not reallocated.
        let capacity = renderer.visible_cache.capacity();
        renderer.collect_and_sort_visible(&world, &camera);
        assert_eq!(drawn(&renderer), vec![opaque, transparent]);
        assert_eq!(renderer.visible_cache.capacity(), capacity);
    }
}
