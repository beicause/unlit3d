//! Proxies for the components whose fields cannot be reflected directly.
//!
//! A reflected component's own fields are its JSON shape, but a field whose
//! type is a GPU handle, a [wgpu::LoadOp] or a private field cannot be
//! reflected. Such a component marks the field (or the whole type) opaque and
//! names a proxy from here: a plain struct over values that *can* be reflected,
//! with a conversion in each direction to the real value.
//!
//! A proxy whose `TryFrom` fails is how a read-only component reports itself:
//! the failure's message is what a write answers with, so the reason is declared
//! next to the type it applies to.

use facet::Facet;
use glam::{Mat4, Quat, Vec3, Vec4};
use unlit_ecs::Entity;

use crate::bounds::Aabb;
use crate::components::{GpuMaterial, GpuMesh, GpuRenderPipeline};
use crate::input::{InputEvent, InputState};

/// A [wgpu::LoadOp] over an `f32` as either a clear to a value or a load.
///
/// A load op is one of two things, so it is written as one: a `clear` field
/// carries what to clear to, and a load has no field at all.
#[derive(Facet)]
pub struct DepthLoadOpProxy {
    /// The value to clear to, if this op clears.
    #[facet(skip_serializing_if = Option::is_none)]
    pub clear: Option<f32>,
}

impl From<&wgpu::LoadOp<f32>> for DepthLoadOpProxy {
    fn from(op: &wgpu::LoadOp<f32>) -> Self {
        Self {
            clear: match op {
                wgpu::LoadOp::Clear(value) => Some(*value),
                wgpu::LoadOp::Load => None,
                _ => None,
            },
        }
    }
}

impl From<DepthLoadOpProxy> for wgpu::LoadOp<f32> {
    fn from(proxy: DepthLoadOpProxy) -> Self {
        match proxy.clear {
            Some(value) => Self::Clear(value),
            None => Self::Load,
        }
    }
}

/// A [wgpu::LoadOp] over a `u32` as either a clear to a value or a load.
#[derive(Facet)]
pub struct StencilLoadOpProxy {
    /// The value to clear to, if this op clears.
    #[facet(skip_serializing_if = Option::is_none)]
    pub clear: Option<u32>,
}

impl From<&wgpu::LoadOp<u32>> for StencilLoadOpProxy {
    fn from(op: &wgpu::LoadOp<u32>) -> Self {
        Self {
            clear: match op {
                wgpu::LoadOp::Clear(value) => Some(*value),
                wgpu::LoadOp::Load => None,
                _ => None,
            },
        }
    }
}

impl From<StencilLoadOpProxy> for wgpu::LoadOp<u32> {
    fn from(proxy: StencilLoadOpProxy) -> Self {
        match proxy.clear {
            Some(value) => Self::Clear(value),
            None => Self::Load,
        }
    }
}

/// A [wgpu::LoadOp] over a color as its four channels.
#[derive(Facet)]
pub struct ColorLoadOpProxy {
    /// The color to clear to, if this op clears.
    #[facet(skip_serializing_if = Option::is_none)]
    pub clear: Option<[f64; 4]>,
}

impl From<&wgpu::LoadOp<wgpu::Color>> for ColorLoadOpProxy {
    fn from(op: &wgpu::LoadOp<wgpu::Color>) -> Self {
        Self {
            clear: match op {
                wgpu::LoadOp::Clear(color) => Some([color.r, color.g, color.b, color.a]),
                wgpu::LoadOp::Load => None,
                _ => None,
            },
        }
    }
}

impl From<ColorLoadOpProxy> for wgpu::LoadOp<wgpu::Color> {
    fn from(proxy: ColorLoadOpProxy) -> Self {
        match proxy.clear {
            Some([r, g, b, a]) => Self::Clear(wgpu::Color { r, g, b, a }),
            None => Self::Load,
        }
    }
}

/// A [GpuMesh] as the fields a reader can see.
#[derive(Facet)]
pub struct GpuMeshProxy {
    /// Vertices the mesh draws.
    #[facet(default)]
    pub count: u32,
    /// First vertex.
    #[facet(default)]
    pub first: u32,
    /// Base vertex added to each index.
    #[facet(default)]
    pub base_vertex: u32,
    /// Whether the mesh draws from an index buffer.
    #[facet(default)]
    pub indexed: bool,
    /// How many morph targets the mesh has.
    #[facet(default)]
    pub morph_targets: u32,
    /// Whether the mesh is skinned.
    #[facet(default)]
    pub skinned: bool,
    /// The mesh's bounds.
    #[facet(default)]
    pub aabb: AabbProxy,
}

/// An [Aabb] as its centre and half extents.
///
/// The proxy is container-level, so a patch that names no field still has to
/// build one: every field is defaultable, and this one is too.
#[derive(Facet, Default)]
pub struct AabbProxy {
    /// Centre in local space.
    #[facet(default)]
    pub center: [f32; 3],
    /// Half extents along each axis.
    #[facet(default)]
    pub half_extents: [f32; 3],
}

impl From<&Aabb> for AabbProxy {
    fn from(aabb: &Aabb) -> Self {
        Self {
            center: aabb.center.to_array(),
            half_extents: aabb.half_extents.to_array(),
        }
    }
}

impl From<&GpuMesh> for GpuMeshProxy {
    fn from(mesh: &GpuMesh) -> Self {
        Self {
            count: mesh.count,
            first: mesh.first,
            base_vertex: mesh.base_vertex,
            indexed: mesh.indexed,
            morph_targets: mesh.morph_targets,
            skinned: mesh.skinned,
            aabb: AabbProxy::from(&mesh.aabb),
        }
    }
}

impl TryFrom<GpuMeshProxy> for GpuMesh {
    type Error = String;

    fn try_from(_proxy: GpuMeshProxy) -> Result<Self, Self::Error> {
        Err("GpuMesh is read-only; allocate one with allocate_unlit_mesh".to_string())
    }
}

/// A [GpuMaterial] as the bind group it names.
#[derive(Facet)]
pub struct GpuMaterialProxy {
    /// Index of the material's bind group in the resource graph.
    #[facet(default)]
    pub bind_group: u32,
}

impl From<&GpuMaterial> for GpuMaterialProxy {
    fn from(material: &GpuMaterial) -> Self {
        Self {
            bind_group: material.bind_group_id.index() as u32,
        }
    }
}

impl TryFrom<GpuMaterialProxy> for GpuMaterial {
    type Error = String;

    fn try_from(_proxy: GpuMaterialProxy) -> Result<Self, Self::Error> {
        Err("GpuMaterial is read-only; allocate one with allocate_unlit_material".to_string())
    }
}

/// A [GpuRenderPipeline] as the family its key belongs to.
#[derive(Facet)]
#[facet(transparent)]
pub struct GpuRenderPipelineProxy(#[facet(default)] pub String);

impl<Key> From<&GpuRenderPipeline<Key>> for GpuRenderPipelineProxy {
    fn from(_pipeline: &GpuRenderPipeline<Key>) -> Self {
        Self("unlit".to_string())
    }
}

impl<Key> TryFrom<GpuRenderPipelineProxy> for GpuRenderPipeline<Key> {
    type Error = String;

    fn try_from(_proxy: GpuRenderPipelineProxy) -> Result<Self, Self::Error> {
        Err("a pipeline is read-only; it is created with a mesh".to_string())
    }
}

/// An [InputState] as a reader sees it: the state and the frame's events.
///
/// The state alone cannot say what happened this frame — which key went down,
/// where the wheel turned — so the events that arrived since they were last
/// cleared are part of what a reader reads. They are read-only like the rest
/// of the state: a writer sends events with
/// [InputState::push](crate::input::InputState::push) instead.
#[derive(Facet, Default)]
pub struct InputStateProxy {
    /// The events that arrived since they were last cleared.
    #[facet(default)]
    pub events: Vec<InputEvent>,
    /// Where the pointer is, or `None` while it is outside the window.
    #[facet(default)]
    pub pointer: Option<[f32; 2]>,
    /// The mouse position, or `None` while it is outside the window.
    #[facet(default)]
    pub cursor: Option<[f32; 2]>,
    /// Whether any pointer is down.
    #[facet(default)]
    pub pointer_down: bool,
    /// The mouse buttons currently held, as a bit set.
    #[facet(default)]
    pub buttons: u8,
    /// Whether the window has focus.
    #[facet(default)]
    pub focused: bool,
    /// The window's size in physical pixels.
    #[facet(default)]
    pub size_px: [u32; 2],
    /// The window's physical pixels per logical point.
    #[facet(default)]
    pub scale_factor: f32,
    /// The touches currently active, as `(id, position)` pairs.
    #[facet(default)]
    pub touches: Vec<(u64, [f32; 2])>,
}

impl From<&InputState> for InputStateProxy {
    fn from(state: &InputState) -> Self {
        Self {
            events: state.events().to_vec(),
            pointer: state.pointer,
            cursor: state.cursor,
            pointer_down: state.pointer_down,
            buttons: state.buttons.bits(),
            focused: state.focused,
            size_px: [state.size_px.0, state.size_px.1],
            scale_factor: state.scale_factor,
            touches: state.touches.clone(),
        }
    }
}

impl TryFrom<InputStateProxy> for InputState {
    type Error = String;

    fn try_from(_proxy: InputStateProxy) -> Result<Self, Self::Error> {
        Err("InputState is read-only; send events with send_input".to_string())
    }
}

/// A [glam::Vec3] as three floats.
#[derive(Facet)]
#[facet(transparent)]
pub struct Vec3Proxy(pub [f32; 3]);

impl From<Vec3Proxy> for Vec3 {
    fn from(proxy: Vec3Proxy) -> Self {
        Vec3::from_array(proxy.0)
    }
}

impl From<&Vec3> for Vec3Proxy {
    fn from(value: &Vec3) -> Self {
        Self(value.to_array())
    }
}

/// A [glam::Vec4] as four floats.
#[derive(Facet)]
#[facet(transparent)]
pub struct Vec4Proxy(pub [f32; 4]);

impl From<Vec4Proxy> for Vec4 {
    fn from(proxy: Vec4Proxy) -> Self {
        Vec4::from_array(proxy.0)
    }
}

impl From<&Vec4> for Vec4Proxy {
    fn from(value: &Vec4) -> Self {
        Self(value.to_array())
    }
}

/// A [glam::Quat] as its `x, y, z, w` components.
#[derive(Facet)]
#[facet(transparent)]
pub struct QuatProxy(pub [f32; 4]);

impl From<QuatProxy> for Quat {
    fn from(proxy: QuatProxy) -> Self {
        Quat::from_array(proxy.0)
    }
}

impl From<&Quat> for QuatProxy {
    fn from(value: &Quat) -> Self {
        Self(value.to_array())
    }
}

/// A [glam::Mat4] as its sixteen columns, column-major.
#[derive(Facet)]
#[facet(transparent)]
pub struct Mat4Proxy(pub [f32; 16]);

impl From<Mat4Proxy> for Mat4 {
    fn from(proxy: Mat4Proxy) -> Self {
        Mat4::from_cols_array(&proxy.0)
    }
}

impl From<&Mat4> for Mat4Proxy {
    fn from(value: &Mat4) -> Self {
        Self(value.to_cols_array())
    }
}

/// A [Vec] of [glam::Mat4], each as its sixteen columns.
#[derive(Facet)]
#[facet(transparent)]
pub struct Mat4VecProxy(pub Vec<[f32; 16]>);

impl From<Mat4VecProxy> for Vec<Mat4> {
    fn from(proxy: Mat4VecProxy) -> Self {
        proxy.0.iter().map(Mat4::from_cols_array).collect()
    }
}

impl From<&Vec<Mat4>> for Mat4VecProxy {
    fn from(value: &Vec<Mat4>) -> Self {
        Self(value.iter().map(Mat4::to_cols_array).collect())
    }
}

/// An [Entity] as its [`Entity::to_bits`](unlit_ecs::Entity::to_bits) integer.
#[derive(Facet)]
#[facet(transparent)]
pub struct EntityProxy(pub u64);

impl From<EntityProxy> for Entity {
    fn from(proxy: EntityProxy) -> Self {
        Entity::from_raw(proxy.0 as u32, (proxy.0 >> 32) as u32)
    }
}

impl From<&Entity> for EntityProxy {
    fn from(value: &Entity) -> Self {
        Self(value.to_bits())
    }
}
