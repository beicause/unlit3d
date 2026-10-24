#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

pub mod attachments;
mod behaviour;
pub mod blit;
pub mod bounds;
pub mod components;
pub mod culling;
pub mod frame;
#[cfg(feature = "gltf")]
pub mod gltf;
pub mod input;
/// The rendering tools a world answers through the Model Context Protocol
/// server in [`unlit_ecs::mcp`].
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
pub mod mcp;
pub mod mesh;
pub mod mesh_source;
mod order;
pub mod pass;
pub mod pipeline;
/// Reflection support for the components that cross a JSON boundary.
///
/// The proxies here are what a reflected component's glam and ECS fields go
/// through, and are reusable by a caller's own reflected components.
#[cfg(feature = "reflect")]
pub mod reflect;
/// The handles a value takes when it leaves a world: a snapshot or a stream.
pub mod relay;
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
        attachments::FrameAttachments,
        blit::BlitSource,
        bounds::{Aabb, FrustumPlanes, Obb},
        components::{
            Camera, GpuMaterial, GpuMesh, GpuRenderPipeline, InstanceVisibility, MeshParts,
            MorphBinding, MorphWeights, RenderLoadOps, SkinBinding, SkinPose, Transform,
            ZSortedDrawing,
        },
        culling::is_culled,
        frame::{
            FrameBehaviour, FrameBehaviourOrder, FrameClock, FrameSkip, OnFrame, dispatch_frame,
        },
        input::{
            ImeEvent, ImeKind, InputEvent, InputEvents, InputFeed, InputState, InputWriter, Key,
            KeyEvent, Modifiers, MouseButton, MouseButtons, MouseEvent, OnIme, OnInput, OnKey,
            OnMouse, OnPointer, OnText, OnTouch, PointerAction, PointerContact, PointerEvent,
            PointerKind, TextEvent, TouchEvent, TouchPhase, WheelUnit, dispatch_input,
            distribute_input, refresh_input, subscribe_input,
        },
        mesh::{JointMatrix, MeshDesc, MorphDeltas, VertexBufferDesc},
        mesh_source::MeshSource,
        pass::{FramePass, FramePassOrder, Pass, PassInfo},
        pipeline::{
            FamilyContext, GlobalResources, InstanceContext, InstanceData, InstanceStreamDesc,
            Rebuild, RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineKey,
        },
        relay::{Snapshot, SnapshotReader, Stream, StreamReader},
        renderer::Renderer,
        source::{
            FrameSource, FrameSourceOrder, MaintainScope, OpenUrl, OpenUrls, RenderContext, Source,
            drain_open_urls, spawn_context,
        },
    };
    pub use unlit_ecs::prelude::*;
    pub use unlit_ecs::{Command, Commands};
    pub use unlit_wgpu::capabilities::{DeviceCapabilities, DeviceTier};
    pub use unlit_wgpu::render_attachments::{
        color_clear, create_color_target, create_depth_target, create_msaa_target,
        create_render_target, depth_clear, stencil_clear,
    };
    pub use unlit_wgpu::scene::{ScissorRect, ViewportRect};
}
