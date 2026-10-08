//! Shared harness for unlit3d GPU integration tests.
//!
//! Re-exports general GPU test helpers and adds crate-specific helpers
//! tailored to the `unlit3d` ECS-based rendering API.

pub use unlit_wgpu_test_util::{Ctx, Frame, assert_image_snapshot, count_pixels_off_background};

use core::ops::DerefMut;
use unlit_ecs::{Entity, World};
use unlit_wgpu::resources::{ResourceGraph, ResourceId, TextureExt, TextureView};
use unlit3d::prelude::*;

// Test constants matching what `unlit_wgpu`'s own tests use.
/// Width of every test frame, in texels.
pub const WIDTH: u32 = 256;
/// Height of every test frame, in texels.
pub const HEIGHT: u32 = 192;
/// The clear colour every test frame is opened with.
pub const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
/// The colour format every test frame is created with.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The frame's GPU context, its mesh source and its frame driver, spawned into
/// a world the caller owns.
///
/// Keeping the handles apart from the [`World`] is what lets a test spawn
/// entities and allocate meshes in the same scope: the helpers borrow the world
/// shared, so a `&mut world` stays free for structural changes.
pub struct TestGpu {
    /// The world addresses of the frame's device, queue and resource graph.
    pub context: RenderContext,
    /// The [`Renderer`] resource entity, driven by [`Self::render`].
    pub renderer: Entity,
    /// The built-in mesh source, or `None` in a UI-only world.
    source: Option<Entity>,
    /// The built-in unlit family's key; every renderable entity carries an
    /// [`UnlitPipeline`] built from it. Meaningless without a mesh source.
    pub key: UnlitPipelineKey,
}

impl TestGpu {
    /// Spawn the frame's context, a mesh source with the built-in unlit family
    /// registered, and the frame driver into `world`.
    pub fn new(world: &mut World, ctx: &Ctx) -> Self {
        let mut test = Self::frame_only(world, ctx);
        let mut source = MeshSource::new(world, test.context);
        source.register_unlit_family(world);
        test.source = Some(spawn_source(world, source));
        test
    }

    /// Spawn only the frame's context and driver, with no mesh source.
    ///
    /// What a UI-only test needs: the frame must draw without the 3D path
    /// being present at all.
    pub fn frame_only(world: &mut World, ctx: &Ctx) -> Self {
        let context = spawn_context(
            world,
            ctx.device.clone(),
            ctx.queue.clone(),
            ResourceGraph::new(),
            ctx.capabilities,
        );
        // The harness's default key reads the per-instance stream, which
        // carries the tint as well as the transform: the ECS tests tint their
        // entities, and a variant that read no instance stream would draw them
        // all in their mesh colour.
        let key = UnlitPipelineKey::new(unlit_options(&ctx.device));
        let renderer = world.spawn((Renderer::new(context),));
        Self {
            context,
            renderer,
            source: None,
            key,
        }
    }

    /// The built-in mesh source's entity.
    ///
    /// # Panics
    ///
    /// In a world built with [`Self::frame_only`], which has none.
    pub fn mesh_source(&self) -> Entity {
        self.source.expect("this test world has a mesh source")
    }

    /// Run `f` on the mesh source.
    pub fn with_mesh_source<R>(
        &self,
        world: &World,
        f: impl FnOnce(&mut MeshSource, &World) -> R,
    ) -> R {
        let entity = self.mesh_source();
        let mut source = world
            .get_mut::<Source>(entity)
            .expect("the source entity exists");
        let mesh = source
            .as_mut::<MeshSource>()
            .expect("the source is a MeshSource");
        f(mesh, world)
    }

    /// Run `f` on the frame driver.
    pub fn with_renderer<R>(&self, world: &World, f: impl FnOnce(&mut Renderer, &World) -> R) -> R {
        world
            .with_mut::<Renderer, _>(self.renderer, |renderer| f(renderer, world))
            .expect("the renderer entity exists")
    }

    /// The frame's resource graph.
    pub fn graph<'w>(&self, world: &'w World) -> impl DerefMut<Target = ResourceGraph> + 'w {
        world
            .get_mut::<ResourceGraph>(self.context.graph)
            .expect("the context's graph exists")
    }

    /// Register `texture` in the frame's graph together with a default view of
    /// it, returning both ids.
    ///
    /// Frame-level, so a test that has no mesh source can still build a render
    /// target: nothing here belongs to the 3D path.
    pub fn register_texture(
        &self,
        world: &World,
        texture: wgpu::Texture,
    ) -> (ResourceId<wgpu::Texture>, ResourceId<TextureView>) {
        let mut graph = self.graph(world);
        let texture_id = graph.insert(texture, None);
        let view = TextureExt::create_view(
            graph
                .get(&texture_id)
                .expect("the texture was just inserted"),
            &wgpu::TextureViewDescriptor::default(),
        );
        let view_id = graph.insert(view, None);
        graph.add_dependency(&view_id, &texture_id);
        (texture_id, view_id)
    }

    /// Allocate a cube mesh through the source.
    pub fn allocate_cube_mesh(&self, world: &World) -> GpuMesh {
        self.allocate_offset_cube_mesh(world, glam::Vec3::ZERO)
    }

    /// Allocate a cube mesh whose vertices are offset by `offset` in mesh
    /// space, returning the `GpuMesh` handle.
    ///
    /// The offset is baked into the vertices, so two cubes allocated from
    /// different offsets draw differently even at the same transform — which is
    /// what tells a mesh apart from the one whose pool range it sits next to.
    pub fn allocate_offset_cube_mesh(&self, world: &World, offset: glam::Vec3) -> GpuMesh {
        let (positions, uvs, colors, indices) = cube();
        let positions = positions
            .into_iter()
            .map(|position| {
                [
                    position[0] + offset.x,
                    position[1] + offset.y,
                    position[2] + offset.z,
                ]
            })
            .collect::<Vec<_>>();
        self.with_mesh_source(world, |source, world| {
            source.allocate_unlit_mesh(
                world,
                &self.key,
                UnlitMeshDesc {
                    positions: &positions,
                    uvs: Some(&uvs),
                    colors: Some(&colors),
                    indices: Some(&indices),
                    ..Default::default()
                },
            )
        })
    }

    /// Allocate a dense grid of cubes through the source, returning the
    /// `GpuMesh` handle.
    ///
    /// `steps` cubes per axis means `steps³` cubes, which is what makes a pool
    /// grow inside a test.
    pub fn allocate_grid_cube_mesh(&self, world: &World, steps: u32) -> GpuMesh {
        let (positions, uvs, colors, indices) = grid_cube(steps);
        self.with_mesh_source(world, |source, world| {
            source.allocate_unlit_mesh(
                world,
                &self.key,
                UnlitMeshDesc {
                    positions: &positions,
                    uvs: Some(&uvs),
                    colors: Some(&colors),
                    indices: Some(&indices),
                    ..Default::default()
                },
            )
        })
    }

    /// Free `mesh` through the source.
    pub fn remove_mesh(&self, world: &World, mesh: GpuMesh) {
        self.with_mesh_source(world, |source, _| source.remove_mesh(mesh));
    }

    /// Run the source's once-per-frame pass over the resource graph.
    ///
    /// Giving up a handle only drops a reference; a test that asserts a node
    /// is gone drives the collection by hand the way a frame's `build_scene`
    /// would.
    pub fn maintain(&self, world: &World) {
        self.with_mesh_source(world, |source, world| source.maintain(world));
    }

    /// Allocate a cube through the source under `key`, carrying the joint
    /// stream and morph displacements the variant reads.
    ///
    /// The key is pure policy: which channels the mesh ends up with is decided
    /// here, from the joints and displacements the caller passes.
    ///
    /// The pose itself is not here: the joints and weights a frame deforms by
    /// live in components of their own, so a test that animates one attaches
    /// [`SkinPose`]/[`MorphWeights`] and the binding components to its
    /// entities.
    pub fn allocate_deformed_cube_mesh(
        &self,
        world: &World,
        key: &UnlitPipelineKey,
        joints: Option<&[[u16; 4]]>,
        weights: Option<&[[f32; 4]]>,
        morph_deltas: Option<MorphDeltas>,
    ) -> GpuMesh {
        let (positions, uvs, colors, indices) = cube();
        self.with_mesh_source(world, |source, world| {
            source.allocate_unlit_mesh(
                world,
                key,
                UnlitMeshDesc {
                    positions: &positions,
                    uvs: Some(&uvs),
                    colors: Some(&colors),
                    indices: Some(&indices),
                    joints,
                    weights,
                    morph_deltas,
                },
            )
        })
    }

    /// Allocate a cube skinned by a [`BendSkin`], returning both the mesh and
    /// the skin that drives it.
    ///
    /// The skin is derived from the very positions the mesh is built from, so
    /// its per-vertex blend cannot drift from the geometry it deforms.
    pub fn allocate_bent_cube_mesh(
        &self,
        world: &World,
        key: &UnlitPipelineKey,
        angle: f32,
    ) -> (GpuMesh, BendSkin) {
        let (positions, uvs, colors, indices) = cube();
        let skin = BendSkin::new(&positions, angle);
        let mesh = self.with_mesh_source(world, |source, world| {
            source.allocate_unlit_mesh(
                world,
                key,
                UnlitMeshDesc {
                    positions: &positions,
                    uvs: Some(&uvs),
                    colors: Some(&colors),
                    indices: Some(&indices),
                    joints: Some(&skin.joints),
                    weights: Some(&skin.weights),
                    morph_deltas: None,
                },
            )
        });
        (mesh, skin)
    }

    /// Allocate a cube morphed by `deltas`, returning the mesh.
    ///
    /// The weights that blend the targets are not part of the mesh: they live
    /// in a [`MorphWeights`] component the test attaches to an entity and names
    /// with a [`MorphBinding`].
    pub fn allocate_morphed_cube_mesh(
        &self,
        world: &World,
        key: &UnlitPipelineKey,
        deltas: MorphDeltas,
    ) -> GpuMesh {
        self.allocate_deformed_cube_mesh(world, key, None, None, Some(deltas))
    }

    /// Allocate an offscreen colour target and a matching depth-stencil target,
    /// register their views in the frame's resource graph, and bind them as the
    /// renderer's render target. Returns the colour texture (for readback).
    pub fn bind_offscreen_target(&self, world: &World, _label: &str) -> wgpu::Texture {
        self.bind_offscreen_target_with(world, 1, true)
    }

    /// Like [`Self::bind_offscreen_target`], but with a chosen sample count and
    /// the depth-stencil attachment optional.
    ///
    /// A source that draws into a color-only target builds its pipelines with
    /// no depth state, so a test of that path must be able to omit the
    /// attachment — `wgpu` rejects the pass otherwise.
    pub fn bind_offscreen_target_with(
        &self,
        world: &World,
        samples: u32,
        with_depth: bool,
    ) -> wgpu::Texture {
        use unlit_wgpu::render_attachments::create_render_target;
        let device = world
            .get::<wgpu::Device>(self.context.device)
            .expect("the context's device")
            .clone();
        let ft = create_render_target(&device, COLOR_FORMAT, WIDTH, HEIGHT, samples);
        let (_, color_view) = self.register_texture(world, ft.color.clone());
        let depth_view = with_depth.then(|| {
            self.graph(world).insert(
                TextureExt::create_view(&ft.depth, &wgpu::TextureViewDescriptor::default()),
                None,
            )
        });
        // A multisampled target resolves through its MSAA view, so the pass
        // needs it bound; without it the draws would go straight to the
        // single-sampled color view.
        let msaa_view = ft.msaa.as_ref().map(|msaa| {
            self.graph(world).insert(
                TextureExt::create_view(msaa, &wgpu::TextureViewDescriptor::default()),
                None,
            )
        });
        self.with_renderer(world, |renderer, world| {
            renderer.set_render_target(world, Some(color_view), depth_view, msaa_view);
        });
        ft.color
    }

    /// Like [`Self::bind_offscreen_target_with`], but at a chosen size.
    ///
    /// A test of how content is fitted to a target has to vary the target's
    /// shape; the frame's default is only one of them.
    pub fn bind_offscreen_target_sized(
        &self,
        world: &World,
        size: (u32, u32),
        samples: u32,
        with_depth: bool,
    ) -> wgpu::Texture {
        use unlit_wgpu::render_attachments::create_render_target;
        let device = world
            .get::<wgpu::Device>(self.context.device)
            .expect("the context's device")
            .clone();
        let ft = create_render_target(&device, COLOR_FORMAT, size.0, size.1, samples);
        let (_, color_view) = self.register_texture(world, ft.color.clone());
        let depth_view = with_depth.then(|| {
            self.graph(world).insert(
                TextureExt::create_view(&ft.depth, &wgpu::TextureViewDescriptor::default()),
                None,
            )
        });
        let msaa_view = ft.msaa.as_ref().map(|msaa| {
            self.graph(world).insert(
                TextureExt::create_view(msaa, &wgpu::TextureViewDescriptor::default()),
                None,
            )
        });
        self.with_renderer(world, |renderer, world| {
            renderer.set_render_target(world, Some(color_view), depth_view, msaa_view);
        });
        ft.color
    }

    /// Bind the offscreen target for `label` and render one frame.
    ///
    /// Returns the colour texture the frame was drawn into, ready to read back.
    pub fn render_to_offscreen(&self, world: &World, label: &str) -> wgpu::Texture {
        let target = self.bind_offscreen_target(world, label);
        self.render(world);
        target
    }

    /// Bind a `size`-pixel offscreen target, render one frame and read it back
    /// as RGBA.
    ///
    /// What a test of the frame's own fitting needs: the target's shape is the
    /// input, so it cannot come from the harness's default size.
    pub fn render_to_offscreen_sized(
        &self,
        ctx: &Ctx,
        world: &World,
        size: (u32, u32),
        _label: &str,
    ) -> Frame {
        let target = self.bind_offscreen_target_sized(world, size, 1, true);
        self.render(world);
        let rgba = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target);
        Frame {
            rgba,
            width: size.0,
            height: size.1,
        }
    }

    /// Render one frame from the world.
    pub fn render(&self, world: &World) {
        self.with_renderer(world, |renderer, world| renderer.render(world));
    }

    /// Render `count` frames.
    ///
    /// A UI test needs two: egui only learns its font metrics on the second,
    /// so the first is drawn and discarded.
    pub fn render_frames(&self, world: &World, count: u32) {
        for _ in 0..count {
            self.render(world);
        }
    }
}

/// Unlit options for the ECS tests: vertex colour + per-instance transform,
/// no texture, no MSAA, with reverse-z depth.
///
/// The per-instance tint an ECS entity may carry rides the same stream as the
/// transform, and one record carries every field, so there is nothing to add
/// for a test that tints: the variant reads the tint because it reads
/// instances at all.
///
/// The options are pure policy: they say nothing about the channels a mesh
/// carries. Those follow the slices `allocate_deformed_cube_mesh` uploads, so a
/// deformed test and a plain one share these options.
pub fn unlit_options(device: &wgpu::Device) -> unlit_wgpu::pipeline::UnlitOptions {
    use unlit_wgpu::render_attachments::default_depth_stencil_format;
    unlit_wgpu::pipeline::UnlitOptions {
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: default_depth_stencil_format(device),
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        color_target: wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        },
        multisample: wgpu::MultisampleState {
            count: 1,
            ..Default::default()
        },
        ..unlit_wgpu::pipeline::UnlitOptions::standard(device)
    }
}

/// The raw channels of one mesh: `(positions, uvs, colors, indices)`.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// A rig of `joint_count` joints, all at rest, returned as joint matrices.
///
/// The identity pose deforms nothing: a vertex's weights sum to one, so the
/// weighted sum of identity matrices is the vertex itself. A test that wants a
/// visible deformation replaces one of them before building a
/// [`SkinPose`].
pub fn rest_pose(joint_count: usize) -> Vec<JointMatrix> {
    vec![glam::Mat4::IDENTITY; joint_count]
}

/// A two-joint skin that bends a mesh about its own base.
///
/// The lower joint is the base and stays at rest; the upper one rotates about
/// the mesh's lowest point, and every vertex is weighted between them by how
/// high it sits, so the mesh bends rather than shears. The weights exercise the
/// four-lane weighted sum the shader computes — a vertex split across both
/// joints is the case a single-joint binding would not catch.
///
/// The joint stream and weights are what a mesh is uploaded with; the matrices
/// are the pose an entity carries, which [`Self::pose`] builds.
pub struct BendSkin {
    /// Four joint indices per vertex: the base and the bending joint.
    pub joints: Vec<[u16; 4]>,
    /// Four weights per vertex, summing to one.
    pub weights: Vec<[f32; 4]>,
    /// The angle the bending joint is rotated by.
    angle: f32,
    /// The height the bending joint pivots about, in the mesh's own space.
    pivot: f32,
}

impl BendSkin {
    /// Skin `positions` to two joints, splitting each vertex by its height.
    ///
    /// The blend runs over the mesh's own vertical extent, so a unit cube's
    /// bottom is fully on the base joint and its top fully on the bending one.
    pub fn new(positions: &[[f32; 3]], angle: f32) -> Self {
        let (min, max) = positions.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(min, max), position| (min.min(position[1]), max.max(position[1])),
        );
        let height = (max - min).max(f32::EPSILON);
        let (joints, weights) = positions
            .iter()
            .map(|position| {
                let t = ((position[1] - min) / height).clamp(0.0, 1.0);
                ([0u16, 1, 0, 0], [1.0 - t, t, 0.0, 0.0])
            })
            .unzip();
        Self {
            joints,
            weights,
            angle,
            pivot: min,
        }
    }

    /// Replace the bending joint's rotation, which pivots about the mesh's
    /// lowest point rather than its origin: a cube centred on the origin would
    /// otherwise swing through the ground instead of bending over it.
    pub fn set_angle(&mut self, angle: f32) {
        self.angle = angle;
    }

    /// The pose the two joints are at, ready to attach to an entity.
    pub fn pose(&self) -> SkinPose {
        let mut matrices = rest_pose(2);
        matrices[1] = glam::Mat4::from_translation(glam::Vec3::Y * self.pivot)
            * glam::Mat4::from_rotation_z(self.angle)
            * glam::Mat4::from_translation(glam::Vec3::Y * -self.pivot);
        SkinPose::new(matrices)
    }
}

/// A unit cube centred at the origin, returned as
/// `(positions, uvs, colors, indices)`.
pub fn cube() -> RawMesh {
    let faces = [
        ([-1.0f32, 0.0, 0.0], [0.0f32, 0.0, 1.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ];

    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();

    for (normal, tangent) in faces {
        let base = positions.len() as u32;
        let normal = glam::Vec3::from(normal);
        let tangent = glam::Vec3::from(tangent);
        let bitangent = normal.cross(tangent);
        for (u, v) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let position = normal + tangent * u + bitangent * v;
            positions.push(position.to_array());
            uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
            let color = (position + 1.0) * 0.5;
            colors.push([color.x, color.y, color.z, 1.0]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // The stream stores colors as `Unorm8x4`, so quantize once here rather
    // than carrying a float copy through the fixture.
    let colors = unlit_wgpu::mesh::quantize_colors(&colors).collect();
    (positions, uvs, colors, indices)
}

/// Camera looking at the origin from (0, 1.2, 3.2).
///
/// Uses reverse-z infinite perspective matching the built-in pipeline's
/// `CompareFunction::Greater` and `depth_clear = 0.0`.
pub fn camera_view(aspect: f32) -> Camera {
    camera_looking_at(
        glam::Vec3::new(0.0, 1.2, 3.2),
        glam::Vec3::new(0.0, 0.2, 0.0),
        aspect,
    )
}

/// A camera at `eye` looking at `target`, with the same reverse-z infinite
/// perspective as [`camera_view`].
pub fn camera_looking_at(eye: glam::Vec3, target: glam::Vec3, aspect: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let view = glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y);
    Camera {
        view_from_world: view,
        clip_from_view: projection,
        active: true,
    }
}

/// A dense grid of cubes as one mesh, returned as
/// `(positions, uvs, colors, indices)`.
///
/// One cube is a couple of hundred bytes, too small to push a pool past its
/// starting capacity; a grid of many of them is what makes a pool grow inside
/// a test.
pub fn grid_cube(steps: u32) -> RawMesh {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();

    for i in 0..steps {
        for j in 0..steps {
            for k in 0..steps {
                let offset = glam::Vec3::new(i as f32 * 3.0, j as f32 * 3.0, k as f32 * 3.0);
                let (mut cube_positions, mut cube_uvs, mut cube_colors, cube_indices) = cube();
                for position in &mut cube_positions {
                    *position = [
                        position[0] * 0.5 + offset.x,
                        position[1] * 0.5 + offset.y,
                        position[2] * 0.5 + offset.z,
                    ];
                }
                let base = positions.len() as u32;
                positions.append(&mut cube_positions);
                uvs.append(&mut cube_uvs);
                colors.append(&mut cube_colors);
                indices.extend(cube_indices.into_iter().map(|index| index + base));
            }
        }
    }

    (positions, uvs, colors, indices)
}
