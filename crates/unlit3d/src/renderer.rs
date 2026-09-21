//! The top-level renderer — a resource entity component that owns GPU state and
//! drives rendering each frame from the ECS world.
//!
//! The [Renderer] is the bridge between [unlit_ecs] data and the
//! [wgpu_unlit_render] GPU pipeline. Spawn it once (typically as a resource
//! entity) and call [Renderer::render] every frame.

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

use crate::components::{BoundingSphere, Camera, GpuMaterial, GpuMesh, InstanceData, Transform};

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

    /// The compiled unlit pipeline.
    pub pipeline: UnlitPipeline,
    /// The shader variant flags that were used to build the pipeline.
    pub options: UnlitOptions,
    /// The UV-and-colour vertex stream descriptor.
    pub uv_color_stream: wgpu_unlit_render::mesh::MeshUvColorStream,
    /// Whether the pipeline needs the mesh-metadata storage buffer.
    needs_metadata: bool,

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

    /// Resource id of the global bind group.
    global_group: Option<ResourceId>,

    /// A reused instance-data buffer, grown as needed.
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: u32,

    /// Temporary attachments set for external-target rendering.
    external_attachments: Option<RenderAttachments>,
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
        let pipeline = UnlitPipeline::new(&device, &options);
        let needs_metadata = options.needs_metadata();
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
            pipeline,
            options,
            uv_color_stream,
            needs_metadata,
            graph,
            attachments,
            camera_buf,
            globals_buf,
            metadata_buf,
            globals,
            metadata: Vec::new(),
            global_group: None,
            instance_buffer: None,
            instance_capacity: 0,
            external_attachments: None,
        };

        renderer.rebuild_global_group();
        renderer
    }

    /// Resize the render target.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.attachments = RenderAttachments::new(
            &self.device,
            AttachmentsInfo::new(&self.device, width, height),
        );
    }

    /// Upload raw mesh data and return a [GpuMesh] handle.
    ///
    /// The mesh is ready to draw immediately. Positions, UVs and colours are
    /// compressed to the compact vertex formats the built-in pipeline expects.
    ///
    /// # Panics
    ///
    /// If the input slices are empty or of mismatched length (see the
    /// compressors in [wgpu_unlit_render::mesh]).
    pub fn upload_mesh(
        &mut self,
        positions: &[[f32; 3]],
        uvs: &[[f32; 2]],
        colors: &[[f32; 4]],
        indices: &[u32],
    ) -> GpuMesh {
        // Compress vertex streams.
        let mut meta = MeshMetadata::default();
        let packed_positions: Vec<_> = compress_positions(positions, &mut meta).collect();
        let packed_indices: Vec<u16> = compress_indices(indices)
            .expect("indices fit in u16")
            .collect();

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
                uvs,
                colors,
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
            .pipeline
            .mesh_layout
            .as_ref()
            .expect("the built-in pipeline has a mesh layout when metadata is needed");
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

        // Index buffer.  Pad to COPY_BUFFER_ALIGNMENT (4 bytes).
        let index_byte_len = packed_indices.len() * size_of::<u16>();
        let padded_len = index_byte_len.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize);
        let index_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit3d::mesh::index"),
            size: padded_len as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut index_data = packed_indices.as_bytes().to_vec();
        index_data.resize(padded_len, 0);
        self.queue.write_buffer(&index_buf, 0, &index_data);
        let index_id = self
            .graph
            .insert(Resource::Buffer(index_buf), &[])
            .expect("index buffer has no dependencies");

        GpuMesh {
            bind_group_id: mesh_bind_group_id,
            vertex_buffers: vec![(POSITION_SLOT, position_id), (UV_COLOR_SLOT, uv_color_id)],
            index_buffer: Some((index_id, wgpu::IndexFormat::Uint16)),
            count: packed_indices.len() as u32,
            indexed: true,
        }
    }

    /// Upload a texture and return a [GpuMaterial] handle.
    ///
    /// `data` is RGBA8 pixel data, `width` and `height` in pixels.
    /// The material is ready to use immediately.
    pub fn upload_texture(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Option<GpuMaterial> {
        if data.is_empty() || width == 0 || height == 0 {
            return None;
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
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
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * format.block_copy_size(None).expect("known format")),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

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

        let material_layout = self
            .pipeline
            .material_layout
            .as_ref()
            .expect("the material layout exists when textures are used");
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::material::bind_group"),
            layout: material_layout,
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

        // Collect visible entities.
        let visible = self.collect_visible_entities(world, &camera);

        if visible.is_empty() {
            self.clear_frame(target);
            return;
        }

        // Build instance data buffer.
        let instance_count = visible.len() as u32;
        self.ensure_instance_buffer(instance_count);

        let mut packed_instances = Vec::with_capacity(visible.len());
        for (_, instance_data) in &visible {
            packed_instances.push(*instance_data);
        }
        self.queue.write_buffer(
            self.instance_buffer
                .as_ref()
                .expect("instance buffer exists"),
            0,
            packed_instances.as_bytes(),
        );

        // Collect all wgpu handles from the graph before building the scene,
        // so prep_attachments can take &mut self later.
        let pipeline_handle = self.pipeline.pipeline.clone();
        let global_bg = self
            .graph
            .get_bind_group(self.global_group.expect("global group was built"))
            .expect("global group exists")
            .clone();
        let instance_buf = self
            .instance_buffer
            .as_ref()
            .expect("instance buffer exists")
            .clone();

        // Collect draw data: for each visible entity, gather the wgpu handles
        // we need before we give up the immutable borrow on self.
        struct DrawData {
            mesh_bg: wgpu::BindGroup,
            material_bg: Option<wgpu::BindGroup>,
            vertex_buffers: Vec<(u32, wgpu::Buffer)>,
            index_buffer: Option<(wgpu::Buffer, wgpu::IndexFormat)>,
            count: u32,
            indexed: bool,
        }

        let graph_ref = &self.graph;
        let mut draws: Vec<DrawData> = Vec::with_capacity(visible.len());
        for (entity, _instance_data) in &visible {
            let mesh = world
                .get::<GpuMesh>(*entity)
                .expect("visible entity has GpuMesh");
            let mesh_bg = graph_ref
                .get_bind_group(mesh.bind_group_id)
                .expect("mesh bind group exists")
                .clone();
            let material_bg = world
                .get::<GpuMaterial>(*entity)
                .and_then(|mat| graph_ref.get_bind_group(mat.bind_group_id))
                .cloned();
            let vertex_buffers: Vec<_> = mesh
                .vertex_buffers
                .iter()
                .map(|&(slot, buf_id)| {
                    let buf = graph_ref
                        .get_buffer(buf_id)
                        .expect("mesh vertex buffer exists")
                        .clone();
                    (slot, buf)
                })
                .collect();
            let index_buffer = mesh.index_buffer.as_ref().map(|(buf_id, fmt)| {
                let buf = graph_ref
                    .get_buffer(*buf_id)
                    .expect("mesh index buffer exists")
                    .clone();
                (buf, *fmt)
            });
            draws.push(DrawData {
                mesh_bg,
                material_bg,
                vertex_buffers,
                index_buffer,
                count: mesh.count,
                indexed: mesh.indexed,
            });
        }

        // Build the scene from the clones — no borrow on self.
        let mut pipeline_group =
            PipelineGroup::new(&pipeline_handle).with_bind_group(GLOBAL_GROUP, &global_bg);

        for (idx, draw) in draws.iter().enumerate() {
            let instance_range = idx as u32..idx as u32 + 1;
            let draw_range = if draw.indexed {
                DrawRange::indexed(0..draw.count).with_instances(instance_range)
            } else {
                DrawRange::vertices(0..draw.count).with_instances(instance_range)
            };

            let mut per_draw = MeshDraw::new(draw_range).with_bind_group(MESH_GROUP, &draw.mesh_bg);

            for (slot, buf) in &draw.vertex_buffers {
                per_draw = per_draw.with_vertex_buffer(*slot, buf.slice(..));
            }

            if let Some((ref idx_buf, fmt)) = draw.index_buffer {
                per_draw = per_draw.with_index_buffer(idx_buf.slice(..), fmt);
            }

            // Carry the instance buffer binding into per_draw.
            per_draw = per_draw.with_vertex_buffer(INSTANCE_SLOT, instance_buf.slice(..));

            let mut material_group = MaterialGroup::new();
            if let Some(ref mat_bg) = draw.material_bg {
                material_group = material_group.with_bind_group(MATERIAL_GROUP, mat_bg);
            }
            material_group = material_group.with_mesh(per_draw);
            pipeline_group = pipeline_group.with_material(material_group);
        }

        let mut scene = Scene::new();
        scene.push(pipeline_group);

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

        self.queue.submit([encoder.finish()]);
    }

    // -- internal helpers ----------------------------------------------------

    /// Rebuild or create the global bind group.
    fn rebuild_global_group(&mut self) {
        let camera_buf = self
            .graph
            .get_buffer(self.camera_buf)
            .expect("camera buffer exists");
        let globals_buf = self
            .graph
            .get_buffer(self.globals_buf)
            .expect("globals buffer exists");

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

        if self.needs_metadata {
            let metadata_buf = self
                .graph
                .get_buffer(self.metadata_buf)
                .expect("metadata buffer exists");
            entries.push(wgpu::BindGroupEntry {
                binding: MESH_METADATA_BINDING,
                resource: metadata_buf.as_entire_binding(),
            });
        }

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::global"),
            layout: &self.pipeline.global_layout,
            entries: &entries,
        });

        let deps: Vec<_> = if self.needs_metadata {
            vec![self.camera_buf, self.globals_buf, self.metadata_buf]
        } else {
            vec![self.camera_buf, self.globals_buf]
        };

        match self.global_group {
            Some(id) => {
                self.graph
                    .replace(id, Resource::BindGroup(bind_group))
                    .expect("global group exists");
                self.graph.mark_clean(id);
            }
            None => {
                self.global_group = Some(
                    self.graph
                        .insert(Resource::BindGroup(bind_group), &deps)
                        .expect("dependencies exist"),
                );
            }
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

        if self.global_group.is_some_and(|id| self.graph.is_dirty(id)) {
            self.rebuild_global_group();
        }
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
    fn collect_visible_entities(
        &self,
        world: &LocalWorld,
        camera: &Camera,
    ) -> Vec<(Entity, MeshInstance)> {
        let frustum = FrustumPlanes::from_clip_from_world(camera.clip_from_world);
        let mut visible = Vec::new();

        // Query entities with (Transform, GpuMesh).
        for (entity, (transform, _mesh)) in world.query::<(&Transform, &GpuMesh)>() {
            let tf = copy_transform(&transform);
            let instance_data = match world.get::<InstanceData>(entity) {
                Some(data) => MeshInstance::new(data.matrix, data.base_color),
                None => MeshInstance::new(
                    glam::Affine3A::from_mat4(tf.compute_matrix()),
                    glam::Vec4::new(1.0, 1.0, 1.0, 1.0),
                ),
            };

            // Frustum culling.
            if let Some(sphere) = world.get::<BoundingSphere>(entity) {
                let mat = affine_from_instance(&instance_data.model);
                let world_center = mat.transform_point3(sphere.center);
                if !frustum.test_sphere(world_center, sphere.radius) {
                    continue;
                }
            }

            visible.push((entity, instance_data));
        }

        // Entities with InstanceData but without Transform.
        for (entity, (instance_data, _mesh)) in world.query::<(&InstanceData, &GpuMesh)>() {
            if world.has::<Transform>(entity) {
                continue;
            }
            let mesh_instance = MeshInstance::new(instance_data.matrix, instance_data.base_color);

            if let Some(sphere) = world.get::<BoundingSphere>(entity) {
                let world_center = instance_data.matrix.transform_point3(sphere.center);
                if !frustum.test_sphere(world_center, sphere.radius) {
                    continue;
                }
            }

            visible.push((entity, mesh_instance));
        }

        visible
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
}
