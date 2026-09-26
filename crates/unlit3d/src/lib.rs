#![doc = include_str!("../README.md")]
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
            Camera, GpuMaterial, GpuMesh, GpuPipeline, InstanceColor, MorphBinding, MorphWeights,
            RenderLoadOps, SkinBinding, SkinPose, Transform, UnlitPipeline, ZSortedDrawing,
        },
        culling::is_culled,
        input::{
            ImeEvent, ImeKind, InputEvent, InputState, Key, KeyEvent, Modifiers, MouseButton,
            MouseButtons, MouseEvent, OnIme, OnInput, OnKey, OnMouse, OnPointer, OnText, OnTouch,
            PointerAction, PointerContact, PointerEvent, PointerKind, TextEvent, TouchEvent,
            TouchPhase, WheelUnit, dispatch_input,
        },
        mesh::{
            JointMatrix, MeshDesc, MorphDeltas, UnlitMeshDesc, UnlitMorphTarget, VertexBufferDesc,
        },
        mesh_source::{MeshSource, UnlitPipelineKey},
        pipeline::{
            DrawKey, FamilyContext, FamilyKey, GlobalBinding, GlobalGroupRebuild, PipelineDesc,
            PipelineFactory, PipelineKey, RenderPipelineFactory, RenderResources,
            TrivialSpecializer,
        },
        renderer::Renderer,
        source::{
            FrameOrder, FrameSource, FrameTarget, InputCapture, RenderContext, Source,
            despawn_source, frame_target, set_frame_target, spawn_context, spawn_source,
            spawn_source_at,
        },
    };
    pub use unlit_ecs::prelude::*;
    pub use unlit_wgpu::render_attachments::{
        color_clear, create_render_target, depth_clear, stencil_clear,
    };
}
