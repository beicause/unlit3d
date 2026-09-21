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
//! ```
//! use unlit3d::prelude::*;
//! use unlit_ecs::LocalWorld;
//! use wgpu_unlit_render::pipeline::UnlitOptions;
//!
//! let (device, queue) =
//!     wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut world = LocalWorld::new();
//!
//! // 1. Spawn the renderer (a resource entity).
//! let renderer = world.spawn((
//!     unlit_ecs::Resource,
//!     Renderer::new(
//!         device,
//!         queue,
//!         UnlitOptions::standard(&wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0),
//!         1280,
//!         720,
//!     ),
//! ));
//!
//! // 2. Upload geometry and a texture through the renderer.
//! let mesh = world.with_mut::<Renderer, _>(renderer, |r| {
//!     let positions = [[0.0; 3]; 3];
//!     let uvs = [[0.0; 2]; 3];
//!     let colors = [[1.0; 4]; 3];
//!     let indices = [0u32, 1, 2];
//!     r.upload_mesh(&positions, &uvs, &colors, &indices)
//! });
//! let material = world.with_mut::<Renderer, _>(renderer, |r| {
//!     let rgba8 = vec![255u8; 256 * 256 * 4];
//!     r.upload_texture(
//!         &rgba8,
//!         256,
//!         256,
//!         wgpu::TextureFormat::Rgba8UnormSrgb,
//!     )
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
pub mod renderer;

pub use components::*;
pub use renderer::Renderer;

/// Convenience re-exports for typical usage.
pub mod prelude {
    pub use crate::{
        Renderer,
        components::{BoundingSphere, Camera, GpuMaterial, GpuMesh, InstanceData, Transform},
    };
}
