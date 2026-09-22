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
//! The renderer draws with whatever pipeline families you register. The
//! built-in unlit shader is one of them: [`Renderer::register_unlit_family`]
//! registers it, and [`Renderer::register_family`] registers a caller's own.
//! A family is identified by its key type, the type its entities' [`GpuPipeline`]
//! components carry.
//!
//! A [`GpuPipeline`] carries a key, not a compiled pipeline: a concrete
//! pipeline is resolved per key against the frame's render target and the
//! entity's vertex layout. Every renderable entity must carry one.
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
//! // 1. Spawn the renderer (a resource entity) and register the built-in
//! //    unlit family with it.
//! let renderer = world.spawn((
//!     unlit_ecs::Resource,
//!     Renderer::new(device, queue, 1280, 720),
//! ));
//! world
//!     .with_mut::<Renderer, _>(renderer, |r| r.register_unlit_family())
//!     .unwrap();
//!
//! // 2. Allocate geometry and a material through the renderer.
//! let key = world
//!     .with_mut::<Renderer, _>(renderer, |r| {
//!         UnlitPipelineKey::new(UnlitOptions::standard(&r.device))
//!     })
//!     .unwrap();
//! let mesh = world.with_mut::<Renderer, _>(renderer, |r| {
//!     let positions = [[0.0; 3]; 3];
//!     let uvs = [[0.0; 2]; 3];
//!     let colors = [[255u8; 4]; 3];
//!     let indices = [0u32, 1, 2];
//!     r.allocate_unlit_mesh(&key, &positions, Some(&uvs), Some(&colors), Some(&indices))
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
//!     r.allocate_unlit_material(&key, view, sampler)
//! });
//!
//! // 3. Spawn a renderable entity, carrying the unlit key.
//! world.spawn((
//!     Transform::default(),
//!     mesh,
//!     material.unwrap(),
//!     UnlitPipeline::new(key),
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
    DrawKey, FamilyContext, FamilyKey, GlobalBinding, GlobalGroupRebuild, PipelineBinding,
    PipelineDesc, PipelineFactory, PipelineKey, RenderPipelineFactory, RenderResources,
    TrivialSpecializer,
};
pub use renderer::{Renderer, UnlitPipelineKey};

/// Convenience re-exports for typical usage.
pub mod prelude {
    pub use crate::{
        FamilyKey, GlobalBinding, GlobalGroupRebuild, MeshDesc, PipelineDesc, PipelineKey,
        RenderPipelineFactory, RenderResources, Renderer, TrivialSpecializer, UnlitPipelineKey,
        VertexBufferDesc,
        components::{
            BoundingSphere, Camera, GpuMaterial, GpuMesh, GpuPipeline, InstanceData, Transform,
            Transparent, UnlitPipeline,
        },
    };
}
