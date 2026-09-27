//! One benchmark frame: the world it draws and the mesh source that draws it.
//!
//! The source is driven directly rather than through a [`Renderer`], so a
//! measurement is of the 3D path's own frame work — culling, per-entity
//! resolution, handle collection and scene assembly — and not of the pass the
//! driver would open around it. The device is the noop backend, so no driver is
//! in the numbers either.
//!
//! Every entity in a world shares one mesh and one pipeline, which is the shape
//! a per-entity cost is read from: an entity adds a cull test, a draw key
//! lookup and a handle set, not a mesh.

use glam::{Quat, Vec3};
use unlit_wgpu::pipeline::{UnlitFlags, UnlitOptions};
use unlit_wgpu::resources::ResourceGraph;
use unlit_wgpu::specialize::SurfaceKey;
use unlit3d::prelude::*;

/// The render target every benchmark frame is specialized for.
pub const SURFACE: SurfaceKey = SurfaceKey {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_stencil_format: None,
    sample_count: 1,
};

/// A world of cubes and the source that draws them.
pub struct Frame {
    world: LocalWorld,
    context: RenderContext,
    source: MeshSource,
}

impl Frame {
    /// A frame in which every entity is inside the camera's frustum.
    pub fn visible(count: u32) -> Self {
        Self::new(count, 1.0)
    }

    /// A frame in which no entity is inside the camera's frustum: what the
    /// culling walk costs on its own.
    pub fn culled(count: u32) -> Self {
        Self::new(count, 100_000.0)
    }

    fn new(count: u32, spread: f32) -> Self {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let mut world = LocalWorld::new();
        let context = spawn_context(&mut world, device, queue, ResourceGraph::new());
        let mut source = MeshSource::new(&world, context);

        // The smallest variant the built-in shader has: no material group, no
        // depth, one vertex stream. That keeps a benchmark's per-entity work on
        // the frame path rather than on binding a fuller material.
        let key = UnlitPipelineKey::new(UnlitOptions {
            flags: UnlitFlags::VERTEX_POSITION
                | UnlitFlags::VERTEX_COLOR
                | UnlitFlags::VERTEX_INSTANCE,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            color_target: wgpu::ColorTargetState {
                format: SURFACE.color_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            },
            multisample: wgpu::MultisampleState {
                count: 1,
                ..Default::default()
            },
        });
        source.register_unlit_family(&world);
        let mesh = source.allocate_unlit_mesh(
            &world,
            &key,
            UnlitMeshDesc {
                positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                colors: Some(&[[255; 4], [0, 255, 0, 255], [0, 0, 255, 255]]),
                indices: Some(&[0u32, 1, 2]),
                ..Default::default()
            },
        );

        let side = (count as f32).cbrt().ceil() as u32;
        for index in 0..count {
            let x = index % side;
            let y = (index / side) % side;
            let z = index / (side * side);
            let cell = Vec3::new(x as f32, y as f32, z as f32) - Vec3::splat(side as f32 * 0.5);
            world.spawn((
                Transform {
                    translation: cell * spread,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::ONE,
                },
                mesh.clone(),
                UnlitPipeline::new(key.clone()),
            ));
        }

        // A camera that sees the whole grid, with a far plane past any spread
        // the benchmarks use.
        let eye = Vec3::new(0.0, 0.0, side as f32 * 1.5 + 20.0);
        let view = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y);
        world.spawn((Camera {
            clip_from_world: glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100_000.0)
                * view,
            position: eye,
        },));

        set_frame_target(
            &world,
            FrameTarget {
                surface: SURFACE,
                width: 256,
                height: 192,
            },
        );

        Self {
            world,
            context,
            source,
        }
    }

    /// Build one frame, returning how many draws it produced.
    pub fn build(&mut self) -> usize {
        profiling::scope!("frame");
        let device = self
            .world
            .get::<wgpu::Device>(self.context.device)
            .expect("the context's device")
            .clone();
        let mut encoder = device.create_command_encoder(&Default::default());
        self.source
            .build_scene(&self.world, self.context, &mut encoder);
        self.source.scene().draws.len()
    }
}
