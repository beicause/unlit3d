//! Shared harness for unlit3d GPU integration tests.
//!
//! Re-exports general GPU test helpers and adds crate-specific helpers
//! tailored to the `unlit3d` ECS-based rendering API.

pub use wgpu_unlit_test_util::{
    Ctx, Frame, assert_image_snapshot, count_pixels_off_background, read_texture_bytes, texel_bytes,
};

use core::ops::DerefMut;
use unlit_ecs::{Entity, LocalWorld, Resource};
use unlit3d::prelude::*;
use wgpu_unlit_render::resources::{Resource as GraphResource, ResourceGraph, ResourceId};

/// Test constants matching what `wgpu_unlit_render`'s own tests use.
pub const WIDTH: u32 = 256;
pub const HEIGHT: u32 = 192;
pub const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The frame's GPU context, its mesh source and its frame driver, spawned into
/// a world the caller owns.
///
/// Keeping the handles apart from the [`LocalWorld`] is what lets a test spawn
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
    pub fn new(world: &mut LocalWorld, ctx: &Ctx) -> Self {
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
    pub fn frame_only(world: &mut LocalWorld, ctx: &Ctx) -> Self {
        let context = spawn_context(
            world,
            ctx.device.clone(),
            ctx.queue.clone(),
            ResourceGraph::new(),
        );
        let key = UnlitPipelineKey::new(unlit_options(&ctx.device));
        let renderer = world.spawn((Resource, Renderer::new(context)));
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
        world: &LocalWorld,
        f: impl FnOnce(&mut MeshSource, &LocalWorld) -> R,
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
    pub fn with_renderer<R>(
        &self,
        world: &LocalWorld,
        f: impl FnOnce(&mut Renderer, &LocalWorld) -> R,
    ) -> R {
        world
            .with_mut::<Renderer, _>(self.renderer, |renderer| f(renderer, world))
            .expect("the renderer entity exists")
    }

    /// The frame's resource graph.
    pub fn graph<'w>(&self, world: &'w LocalWorld) -> impl DerefMut<Target = ResourceGraph> + 'w {
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
        world: &LocalWorld,
        texture: wgpu::Texture,
    ) -> (ResourceId, ResourceId) {
        let mut graph = self.graph(world);
        let texture_id = graph
            .insert_strong(GraphResource::Texture(texture), &[])
            .expect("a texture has no dependencies");
        let view = graph
            .get_texture(texture_id)
            .expect("the texture was just inserted")
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_id = graph
            .insert_strong(
                GraphResource::TextureView {
                    view,
                    format: COLOR_FORMAT,
                },
                &[texture_id],
            )
            .expect("the view depends on its texture");
        (texture_id, view_id)
    }

    /// Allocate a cube mesh through the source.
    pub fn allocate_cube_mesh(&self, world: &LocalWorld) -> GpuMesh {
        self.allocate_offset_cube_mesh(world, glam::Vec3::ZERO)
    }

    /// Allocate a cube mesh whose vertices are offset by `offset` in mesh
    /// space, returning the `GpuMesh` handle.
    ///
    /// The offset is baked into the vertices, so two cubes allocated from
    /// different offsets draw differently even at the same transform — which is
    /// what tells a mesh apart from the one whose pool range it sits next to.
    pub fn allocate_offset_cube_mesh(&self, world: &LocalWorld, offset: glam::Vec3) -> GpuMesh {
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
                &positions,
                Some(&uvs),
                Some(&colors),
                Some(&indices),
            )
        })
    }

    /// Allocate a dense grid of cubes through the source, returning the
    /// `GpuMesh` handle.
    ///
    /// `steps` cubes per axis means `steps³` cubes, which is what makes a pool
    /// grow inside a test.
    pub fn allocate_grid_cube_mesh(&self, world: &LocalWorld, steps: u32) -> GpuMesh {
        let (positions, uvs, colors, indices) = grid_cube(steps);
        self.with_mesh_source(world, |source, world| {
            source.allocate_unlit_mesh(
                world,
                &self.key,
                &positions,
                Some(&uvs),
                Some(&colors),
                Some(&indices),
            )
        })
    }

    /// Free `mesh` through the source.
    pub fn remove_mesh(&self, world: &LocalWorld, mesh: GpuMesh) {
        self.with_mesh_source(world, |source, world| source.remove_mesh(world, mesh));
    }

    /// Allocate an offscreen colour target and a matching depth-stencil target,
    /// register their views in the frame's resource graph, and bind them as the
    /// renderer's render target. Returns the colour texture (for readback).
    pub fn bind_offscreen_target(&self, world: &LocalWorld, _label: &str) -> wgpu::Texture {
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
        world: &LocalWorld,
        samples: u32,
        with_depth: bool,
    ) -> wgpu::Texture {
        use wgpu_unlit_render::render_attachments::create_render_target;
        let device = world
            .get::<wgpu::Device>(self.context.device)
            .expect("the context's device")
            .clone();
        let ft = create_render_target(&device, COLOR_FORMAT, WIDTH, HEIGHT, samples);
        let (_, color_view) = self.register_texture(world, ft.color.clone());
        let depth_view = with_depth.then(|| {
            self.graph(world)
                .insert_strong(
                    GraphResource::TextureView {
                        view: ft
                            .depth
                            .create_view(&wgpu::TextureViewDescriptor::default()),
                        format: wgpu_unlit_render::render_attachments::default_depth_stencil_format(
                            &device,
                        ),
                    },
                    &[],
                )
                .expect("depth view has no dependencies")
        });
        // A multisampled target resolves through its MSAA view, so the pass
        // needs it bound; without it the draws would go straight to the
        // single-sampled color view.
        let msaa_view = ft.msaa.as_ref().map(|msaa| {
            self.graph(world)
                .insert_strong(
                    GraphResource::TextureView {
                        view: msaa.create_view(&wgpu::TextureViewDescriptor::default()),
                        format: COLOR_FORMAT,
                    },
                    &[],
                )
                .expect("msaa view has no dependencies")
        });
        self.with_renderer(world, |renderer, world| {
            renderer.set_render_target(world, Some(color_view), depth_view, msaa_view);
        });
        ft.color
    }

    /// Bind the offscreen target for `label` and render one frame.
    ///
    /// Returns the colour texture the frame was drawn into, ready to read back.
    pub fn render_to_offscreen(&self, world: &LocalWorld, label: &str) -> wgpu::Texture {
        let target = self.bind_offscreen_target(world, label);
        self.render(world);
        target
    }

    /// Render one frame from the world.
    pub fn render(&self, world: &LocalWorld) {
        self.with_renderer(world, |renderer, world| renderer.render(world));
    }

    /// Render `count` frames.
    ///
    /// A UI test needs two: egui only learns its font metrics on the second,
    /// so the first is drawn and discarded.
    pub fn render_frames(&self, world: &LocalWorld, count: u32) {
        for _ in 0..count {
            self.render(world);
        }
    }
}

/// Unlit options for the ECS tests: vertex colour + instance, no texture,
/// no MSAA, with reverse-z depth.
fn unlit_options(device: &wgpu::Device) -> wgpu_unlit_render::pipeline::UnlitOptions {
    use wgpu_unlit_render::pipeline::UnlitFlags;
    use wgpu_unlit_render::render_attachments::default_depth_stencil_format;
    wgpu_unlit_render::pipeline::UnlitOptions {
        flags: UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_COLOR | UnlitFlags::VERTEX_INSTANCE,
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
    }
}

/// The raw channels of one mesh: `(positions, uvs, colors, indices)`.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

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
    let colors = wgpu_unlit_render::mesh::quantize_colors(&colors).collect();
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
        clip_from_world: projection * view,
        position: eye,
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
