#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod blit;
pub mod bounds;
pub mod components;
pub mod culling;
#[cfg(feature = "gltf")]
pub mod gltf;
pub mod input;
pub mod mesh;
pub mod mesh_source;
pub mod pipeline;
/// Reflection support for the components that cross a JSON boundary.
///
/// The proxies here are what a reflected component's glam and ECS fields go
/// through, and are reusable by a caller's own reflected components.
#[cfg(feature = "reflect")]
pub mod reflect;
pub mod renderer;
pub mod scene;
pub mod source;
#[cfg(feature = "ui")]
pub mod ui;
pub mod unlit;
#[cfg(feature = "winit")]
pub mod winit;

/// The types most callers need for typical usage.
pub mod prelude {
    #[cfg(feature = "ui")]
    pub use crate::ui::{UiPanel, UiSource};
    pub use crate::unlit::{
        InstanceColor, InstanceCutoff, MeshSourceUnlitExt, UnlitInstance, UnlitMeshDesc,
        UnlitPipeline, UnlitPipelineKey,
    };
    pub use crate::{
        blit::{BlitSource, BlitTexture},
        bounds::{Aabb, FrustumPlanes, Obb},
        components::{
            Camera, GpuMaterial, GpuMesh, GpuRenderPipeline, MeshParts, MorphBinding, MorphWeights,
            RenderLoadOps, SkinBinding, SkinPose, Transform, ZSortedDrawing,
        },
        culling::is_culled,
        input::{
            ImeEvent, ImeKind, InputEvent, InputState, Key, KeyEvent, Modifiers, MouseButton,
            MouseButtons, MouseEvent, OnIme, OnInput, OnKey, OnMouse, OnPointer, OnText, OnTouch,
            PointerAction, PointerContact, PointerEvent, PointerKind, TextEvent, TouchEvent,
            TouchPhase, WheelUnit, dispatch_input,
        },
        mesh::{JointMatrix, MeshDesc, MorphDeltas, VertexBufferDesc},
        mesh_source::MeshSource,
        pipeline::{
            FamilyContext, GlobalResources, InstanceContext, InstanceData, InstanceStreamDesc,
            Rebuild, RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineKey,
        },
        renderer::Renderer,
        source::{
            FrameOrder, FrameSource, FrameTarget, FrameViewport, InputCapture, MaintainScope,
            RenderContext, Source, WorldSourceExt, frame_target, frame_viewport, set_frame_target,
            set_frame_viewport, spawn_context,
        },
    };
    pub use unlit_ecs::prelude::*;
    pub use unlit_wgpu::capabilities::{DeviceCapabilities, DeviceTier};
    pub use unlit_wgpu::render_attachments::{
        color_clear, create_render_target, depth_clear, stencil_clear,
    };
    pub use unlit_wgpu::scene::{ScissorRect, ViewportRect, full_viewport};
}
