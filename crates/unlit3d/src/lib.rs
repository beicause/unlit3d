//! `unlit3d` — the upper rendering API for `wgpu_unlit_render`.
//!
//! This crate bridges [`unlit_ecs`](https://docs.rs/unlit_ecs) and
//! [`wgpu_unlit_render`](https://docs.rs/wgpu_unlit_render): it provides the
//! component types needed to describe a renderable 3D scene in an ECS world
//! and a [`Renderer`] component that turns that data into GPU draw commands
//! every frame.
//!
//! # Quick start
//!
//! The renderer draws with whatever pipelines you register, in registration
//! order. The built-in unlit shader is one of them:
//! [`Renderer::with_unlit`] registers it for you, and
//! [`Renderer::register_pipeline`] takes a [`PipelineDesc`] for a custom one.
//!
//! ```
//! use unlit3d::prelude::*;
//! use unlit_ecs::LocalWorld;
//! use wgpu_unlit_render::pipeline::UnlitOptions;
//!
//! let (device, queue) =
//!     wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut world = LocalWorld::new();
//!
//! // 1. Spawn the renderer (a resource entity) with the unlit pipeline.
//! let options = UnlitOptions::standard(&device);
//! let renderer = world.spawn((
//!     unlit_ecs::Resource,
//!     Renderer::with_unlit(device, queue, options, 1280, 720),
//! ));
//!
//! // 2. Allocate geometry and a material through the renderer.
//! let mesh = world.with_mut::<Renderer, _>(renderer, |r| {
//!     let positions = [[0.0; 3]; 3];
//!     let uvs = [[0.0; 2]; 3];
//!     let colors = [[255u8; 4]; 3];
//!     let indices = [0u32, 1, 2];
//!     let stream = wgpu_unlit_render::mesh::MeshUvColorStream {
//!         flags: wgpu_unlit_render::mesh::UvColorFlags::UV
//!             | wgpu_unlit_render::mesh::UvColorFlags::COLOR,
//!     };
//!     r.allocate_unlit_mesh(stream, &positions, Some(&uvs), Some(&colors), Some(&indices))
//! });
//! let material = world.with_mut::<Renderer, _>(renderer, |r| {
//!     let texture = r.device.create_texture(&wgpu::TextureDescriptor {
//!         label: None,
//!         size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
//!         mip_level_count: 1,
//!         sample_count: 1,
//!         dimension: wgpu::TextureDimension::D2,
//!         format: wgpu::TextureFormat::Rgba8UnormSrgb,
//!         usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
//!         view_formats: &[],
//!     });
//!     // The material reads a view of the texture and a sampler, both of
//!     // which the renderer keeps for it: only ids cross the boundary.
//!     let view = r.register_texture(texture);
//!     let sampler = r.register_sampler(None);
//!     r.allocate_unlit_material(view, sampler)
//! });
//!
//! // 3. Spawn a renderable entity.
//! world.spawn((
//!     Transform::default(),
//!     mesh,
//!     material.unwrap(),
//!     BoundingSphere { center: glam::Vec3::ZERO, radius: 1.0 },
//! ));
//!
//! // 4. Render one frame (uses noop device, produces a valid command buffer).
//! world.with_mut::<Renderer, _>(renderer, |r| {
//!     r.render(&world, None);
//! });
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod components;
pub mod mesh;
pub mod pipeline;
pub mod renderer;

pub use components::*;
pub use mesh::{MeshDesc, VertexBufferDesc};
pub use pipeline::{
    GlobalBinding, GlobalGroupRebuild, PipelineBinding, PipelineDesc, RenderResources,
};
pub use renderer::Renderer;

/// Convenience re-exports for typical usage.
pub mod prelude {
    pub use crate::{
        GlobalBinding, GlobalGroupRebuild, MeshDesc, PipelineDesc, RenderResources, Renderer,
        VertexBufferDesc,
        components::{
            BoundingSphere, Camera, GpuMaterial, GpuMesh, GpuPipeline, InstanceData, Transform,
            Transparent,
        },
    };
}
