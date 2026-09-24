//! `unlit3d` — the upper rendering API for `wgpu_unlit_render`.
//!
//! This crate bridges [`unlit_ecs`] and [`wgpu_unlit_render`]: it provides the
//! component types needed to describe a renderable 3D scene in an ECS world and
//! a [`Renderer`] component that turns that data into GPU draw commands every
//! frame. The two foundational crates stay direct dependencies — their items
//! are reached through their own paths, and the ones most callers need are
//! re-exported by [`prelude`].
//!
//! # Quick start
//!
//! A frame is assembled out of frame sources. [`Renderer`] owns the frame's
//! render target and records the sources it is given; the built-in mesh
//! rendering is one source, [`MeshSource`], and a caller's own pass over the
//! frame is another, with no less privilege. Each source builds its own scene
//! and declares where in the frame it belongs through [`FrameOrder`].
//!
//! The GPU state a frame draws with — the device, the queue and the resource
//! graph — lives in the ECS world as resource components, addressed by a
//! [`RenderContext`]. [`spawn_context`] spawns them and returns their
//! addresses.
//!
//! Each source draws with whatever pipeline families are registered on it. The
//! built-in unlit shader is one: [`MeshSource::register_unlit_family`]
//! registers it, and [`MeshSource::register_family`] registers a caller's own.
//! A family is identified by its key type, the type its entities'
//! [`GpuPipeline`] components carry.
//!
//! A [`GpuPipeline`] carries a key, not a compiled pipeline: a concrete
//! pipeline is resolved per key against the frame's render target and the
//! entity's vertex layout. Every renderable entity must carry one.
//!
//! ```
//! use unlit3d::prelude::*;
//! use wgpu_unlit_render::pipeline::UnlitOptions;
//! use wgpu_unlit_render::resources::ResourceGraph;
//!
//! let (device, queue) =
//!     wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut world = LocalWorld::new();
//!
//! // 1. Spawn the frame's GPU context, the built-in mesh source and the
//! //    frame driver, and register the built-in unlit family.
//! let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
//! let mut mesh_source = MeshSource::new(&world, ctx);
//! mesh_source.register_unlit_family(&world);
//! let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world)));
//! let source = spawn_source(&mut world, mesh_source);
//! let renderer = world.spawn((Resource, Renderer::new(ctx)));
//!
//! // 2. Allocate geometry and a material through the mesh source.
//! let (mesh, material) = world
//!     .with_mut::<Source, _>(source, |source| {
//!         let source = source.as_mut::<MeshSource>().unwrap();
//!         let positions = [[0.0; 3]; 3];
//!         let uvs = [[0.0; 2]; 3];
//!         let colors = [[255u8; 4]; 3];
//!         let indices = [0u32, 1, 2];
//!         let mesh = source.allocate_unlit_mesh(
//!             &world, &key, &positions, Some(&uvs), Some(&colors), Some(&indices));
//!         let device = source.device(&world);
//!         let texture = device.create_texture(&wgpu::TextureDescriptor {
//!             label: Some("example::texture"),
//!             size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
//!             mip_level_count: 1,
//!             sample_count: 1,
//!             dimension: wgpu::TextureDimension::D2,
//!             format: wgpu::TextureFormat::Rgba8UnormSrgb,
//!             usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
//!             view_formats: &[],
//!         });
//!         // The material reads a view of the texture and a sampler, both of
//!         // which the graph keeps for it: only ids cross the boundary.
//!         let view = source.register_texture_and_default_view(&world, texture).1;
//!         let sampler = source.register_sampler(&world, None);
//!         let material = source.allocate_unlit_material(&world, &key, view, sampler);
//!         (mesh, material)
//!     })
//!     .unwrap();
//!
//! // 3. Spawn a renderable entity, carrying the unlit key.
//! world.spawn((
//!     Transform::default(),
//!     mesh,
//!     material.unwrap(),
//!     UnlitPipeline::new(key),
//! ));
//!
//! // 4. Bind a render target from the frame's resource graph and render one
//! //    frame (uses the noop device, so it produces a valid command buffer
//! //    without touching a GPU).
//! let ft = create_render_target(
//!     &world.get::<wgpu::Device>(ctx.device).unwrap(),
//!     wgpu::TextureFormat::Rgba8UnormSrgb,
//!     1280, 720, 1,
//! );
//! let (color_view, depth_view) = world
//!     .with_mut::<Source, _>(source, |source| {
//!         let source = source.as_mut::<MeshSource>().unwrap();
//!         let color_view = source.register_texture_and_default_view(&world, ft.color).1;
//!         let depth_view = MeshSource::graph(&world, ctx)
//!             .insert_strong(
//!                 ft.depth.create_view(&wgpu::TextureViewDescriptor::default()),
//!                 &[],
//!             )
//!             .unwrap();
//!         (color_view, depth_view)
//!     })
//!     .unwrap();
//! world
//!     .with_mut::<Renderer, _>(renderer, |r| {
//!         r.set_render_target(&world, Some(color_view), Some(depth_view), None);
//!         r.render(&world);
//!     })
//!     .unwrap();
//! ```
//!
//! [`unlit_ecs`]: unlit_ecs
//! [`wgpu_unlit_render`]: wgpu_unlit_render

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod bounds;
pub mod components;
pub mod culling;
pub mod input;
pub mod mesh;
pub mod mesh_source;
pub mod pipeline;
pub mod renderer;
pub mod scene;
pub mod source;
#[cfg(feature = "ui")]
pub mod ui;
#[cfg(feature = "winit")]
pub mod winit;

/// The types most callers need for typical usage.
pub mod prelude {
    #[cfg(feature = "ui")]
    pub use crate::ui::{UiPanel, UiSource};
    pub use crate::{
        bounds::{Aabb, FrustumPlanes, Obb},
        components::{
            Camera, GpuMaterial, GpuMesh, GpuPipeline, InstanceColor, RenderLoadOps, Transform,
            UnlitPipeline, ZSortedDrawing,
        },
        culling::is_culled,
        input::{
            ImeEvent, ImeKind, InputEvent, InputState, Key, KeyEvent, Modifiers, OnIme, OnInput,
            OnKey, OnPointer, OnText, OnTouch, PointerButton, PointerButtons, PointerEvent,
            TextEvent, TouchEvent, TouchPhase, WheelUnit, dispatch_input,
        },
        mesh::{MeshDesc, VertexBufferDesc},
        mesh_source::{MeshSource, UnlitPipelineKey},
        pipeline::{
            DrawKey, FamilyContext, FamilyKey, GlobalBinding, GlobalGroupRebuild, PipelineDesc,
            PipelineFactory, PipelineKey, RenderPipelineFactory, RenderResources,
            TrivialSpecializer,
        },
        renderer::Renderer,
        source::{
            FrameOrder, FrameSource, FrameTarget, InputCapture, RenderContext, Source,
            frame_target, set_frame_target, spawn_context, spawn_source, spawn_source_at,
        },
    };
    pub use unlit_ecs::prelude::*;
    pub use wgpu_unlit_render::render_attachments::{
        color_clear, create_render_target, depth_clear, stencil_clear,
    };
}
