#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

/// The renderer this crate is built on, re-exported.
///
/// This crate's own signatures name its types — [`unlit_wgpu::resources::ResourceGraph`],
/// [`unlit_wgpu::capabilities::DeviceCapabilities`], the scene and pipeline types — so a
/// caller that builds its own sources and pipelines reaches them here rather than
/// depending on a second copy of the crate.
pub use unlit_wgpu;
/// The wgpu underneath, re-exported.
///
/// The same argument as [`unlit_wgpu`]: types like [`wgpu::Device`] and
/// [`wgpu::SamplerDescriptor`] cross this crate's boundary, so both sides have to
/// name one wgpu.
pub use unlit_wgpu::wgpu;

pub mod attachments;
mod behaviour;
pub mod blit;
pub mod bounds;
pub mod components;
pub mod culling;
pub mod frame;
/// The frame's shared uniform records: one camera, one globals and one view
/// buffer per distinct record, uploaded once however many draws read them.
pub mod global_uniforms;
#[cfg(feature = "gltf")]
#[cfg_attr(docsrs, doc(cfg(feature = "gltf")))]
pub mod gltf;
pub mod input;
/// The frame's lights, and the clustering a lit frame does of them.
pub mod light;
/// Screen-space line segments drawn as coverage.
///
/// A line is not a mesh: its width is a pixel count, so its quad is derived in
/// the vertex stage from the projected ends rather than from a vertex buffer.
pub mod line;
/// The built-in lit pipeline: the shader, the family it registers and the
/// meshes it draws.
pub mod lit;
/// The rendering tools a world answers through the Model Context Protocol
/// server in [`unlit_ecs::mcp`].
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "mcp", not(target_arch = "wasm32")))))]
pub mod mcp;
pub mod mesh;
pub mod mesh_source;
pub mod offscreen;
mod order;
pub mod pass;
pub mod pipeline;
/// Reflection support for the components that cross a JSON boundary.
///
/// The proxies here are what a reflected component's glam and ECS fields go
/// through, and are reusable by a caller's own reflected components.
#[cfg(feature = "reflect")]
#[cfg_attr(docsrs, doc(cfg(feature = "reflect")))]
pub mod reflect;
/// The handles a value takes when it leaves a world: a snapshot or a stream.
pub mod relay;
pub mod renderer;
pub mod scene;
pub mod source;
#[cfg(feature = "ui")]
#[cfg_attr(docsrs, doc(cfg(feature = "ui")))]
pub mod ui;
#[cfg(feature = "winit")]
#[cfg_attr(docsrs, doc(cfg(feature = "winit")))]
pub mod winit;

/// The types most callers need for typical usage.
pub mod prelude {
    pub use crate::light::{
        AmbientLight, DirectionalLight, Light, LightDescribe, LightKind, LightSink, LightSource,
        PointLight, QuadLight, SpotLight,
    };
    pub use crate::lit::{
        InstanceColor, InstanceCutoff, InstanceHullWidth, LitInstance, LitMeshDesc, LitPipelineKey,
    };
    #[cfg(feature = "ui")]
    pub use crate::ui::{UiPanel, UiSource};
    #[cfg(feature = "winit")]
    pub use crate::winit::SurfaceOptions;
    #[cfg(feature = "winit")]
    pub use crate::winit::presenter::{
        HostPump, PresentationPath, Presenter, build_presenter, present_frames, pump_presented,
        pump_presented_with, resize_presented, setup_presenter,
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
        global_uniforms::{CameraKey, FrameUniforms, GlobalUniforms, UniformGroups, ViewKey},
        input::{
            ImeEvent, ImeKind, InputEvent, InputEvents, InputFeed, InputState, InputWriter, Key,
            KeyEvent, Modifiers, MouseButton, MouseButtons, MouseEvent, OnIme, OnInput, OnKey,
            OnMouse, OnPointer, OnText, OnTouch, PointerAction, PointerContact, PointerEvent,
            PointerKind, TextEvent, TouchEvent, TouchPhase, WheelUnit, dispatch_input,
            distribute_input, refresh_input, subscribe_input,
        },
        line::{Line, LineSegment, LineSource},
        mesh::{JointMatrix, MeshDesc, MorphDeltas, VertexBufferDesc},
        mesh_source::MeshSource,
        offscreen::{IntermediateSize, PersistentTarget, TargetConfig},
        pass::{FramePass, FramePassOrder, Pass, PassInfo},
        relay::{Snapshot, SnapshotReader, Stream, StreamReader},
        renderer::Renderer,
        source::{
            FrameSource, FrameSourceOrder, MaintainScope, OpenUrl, OpenUrls, RenderContext, Source,
            WrongSourceType, drain_open_urls, spawn_context,
        },
    };
    pub use unlit_ecs::prelude::*;
    pub use unlit_ecs::{Command, Commands};
    pub use unlit_wgpu::capabilities::{DeviceCapabilities, DeviceTier};
    // The GPU a frame draws with, and whether it is still alive.
    pub use unlit_wgpu::context::{DeviceHealth, DeviceLost, RenderDevice};
    // The grid a lit source clusters its lights into, which is a field of
    // `MeshSource` a caller sets.
    pub use unlit_wgpu::light::ClusterConfig;
    pub use unlit_wgpu::render_attachments::{
        color_clear, create_color_target, create_depth_target, create_msaa_target,
        create_render_target, depth_clear, stencil_clear,
    };
    pub use unlit_wgpu::scene::{ScissorRect, ViewportRect};
}
