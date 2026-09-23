//! Component types for the ECS-based scene description.
//!
//! These components live in an [`unlit_ecs`] world and are read by the
//! [`Renderer`](crate::Renderer) each frame to build the draw commands.

use wgpu_unlit_render::render_attachments::{color_clear, depth_clear, stencil_clear};
use wgpu_unlit_render::resources::ResourceId;
use wgpu_unlit_render::specialize::VertexBufferLayoutDesc;

use crate::bounds::Aabb;
use crate::renderer::UnlitPipelineKey;

/// World-space transform (translation, rotation, scale).
///
/// The renderer reads this component to position each entity in world space.
/// A renderable entity without one is placed at the origin with an identity
/// transform, so it is still drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Transform {
    /// Translation in world space.
    pub translation: glam::Vec3,
    /// Rotation, expressed as a unit quaternion.
    pub rotation: glam::Quat,
    /// Scale along each local axis.
    pub scale: glam::Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: glam::Vec3::ZERO,
            rotation: glam::Quat::IDENTITY,
            scale: glam::Vec3::ONE,
        }
    }
}

impl Transform {
    /// The affine matrix this transform represents, used as the model
    /// matrix.
    #[must_use]
    pub fn compute_matrix(&self) -> glam::Affine3A {
        glam::Affine3A::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

/// Camera state: the combined projection x view matrix and the eye position.
///
/// The renderer queries the first entity that carries this component to
/// derive the camera uniforms and perform frustum culling. If no entity has
/// a [`Camera`] component, the frame is cleared and nothing is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// World-to-clip matrix (projection x view).
    pub clip_from_world: glam::Mat4,
    /// World-space eye position.
    pub position: glam::Vec3,
}

/// The load ops a frame's pass is opened with.
///
/// The renderer opens one pass per frame over its attachments; this component
/// decides what that pass loads and clears. It may sit on any entity — the
/// renderer draws with the first one it finds — and a frame whose world has
/// none is opened with [`RenderLoadOps::default`].
///
/// A load op only applies to an attachment the pass actually has. A frame
/// that draws into the caller's own target gets both a color and a depth
/// attachment; the renderer's internal attachment set may be depth-only.
/// Clearing an attachment the pass does not have is therefore not an error —
/// the op is simply unused — so one component fits either target.
///
/// ```
/// use unlit3d::components::RenderLoadOps;
/// use unlit3d::prelude::depth_clear;
///
/// let ops = RenderLoadOps {
///     color: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
///     ..Default::default()
/// };
/// assert_eq!(ops.depth, depth_clear());
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderLoadOps {
    /// The color attachment's load op.
    ///
    /// Defaults to [color_clear]; [wgpu::LoadOp::Load] draws on top of
    /// what the target already holds.
    pub color: wgpu::LoadOp<wgpu::Color>,
    /// The depth attachment's load op.
    ///
    /// Defaults to [depth_clear], a clear to the renderer's reverse-z
    /// far plane rather than an arbitrary zero: a depth attachment means the
    /// same thing however the frame is configured.
    pub depth: wgpu::LoadOp<f32>,
    /// The stencil attachment's load op.
    ///
    /// The built-in pipelines write no stencil, so the pass discards it
    /// either way; this only says what a pass that reads stencil beforehand
    /// starts from.
    pub stencil: wgpu::LoadOp<u32>,
}

impl Default for RenderLoadOps {
    fn default() -> Self {
        Self {
            color: color_clear(),
            depth: depth_clear(),
            stencil: stencil_clear(),
        }
    }
}

/// A handle to a mesh stored in the renderer's GPU resource graph.
///
/// Created by [`Renderer::allocate_mesh`](crate::Renderer::allocate_mesh) or
/// [`Renderer::allocate_unlit_mesh`](crate::Renderer::allocate_unlit_mesh).
/// The mesh is ready to draw immediately and the handle stays valid until
/// [`Renderer::remove_mesh`](crate::Renderer::remove_mesh) is called with it.
#[derive(Clone, Debug)]
pub struct GpuMesh {
    /// The mesh's virtual root node in the resource graph.
    ///
    /// It holds no GPU resource of its own and is the mesh's only lifetime
    /// entry point: the vertex and index buffers, the mesh bind group and the
    /// mesh-info uniform are all weak nodes registered under it, so
    /// [`Renderer::remove_mesh`](crate::Renderer::remove_mesh) frees the whole
    /// mesh by removing this one node and collecting the parts it leaves
    /// behind. The other ids below are for the draw path, which reads the
    /// parts directly.
    pub root: ResourceId,

    /// Vertex buffers, each tagged with its slot index, in slot order.
    pub vertex_buffers: Vec<(u32, ResourceId)>,

    /// The vertex layout of the draw, slot by slot.
    ///
    /// A pipeline family specializes on this: two meshes whose layouts imply
    /// different pipeline descriptors resolve to different compiled pipelines.
    /// It is owned here so a key can name it without going back to the
    /// [`MeshDesc`](crate::MeshDesc) it was uploaded from. It may name a slot
    /// whose buffer the draw binds from the renderer rather than the mesh —
    /// the per-instance buffer, for one.
    pub vertex_layout: Vec<(u32, VertexBufferLayoutDesc)>,

    /// Index buffer, if the mesh is indexed.
    pub index_buffer: Option<(ResourceId, wgpu::IndexFormat)>,

    /// Number of indices (indexed draw) or vertices (non-indexed draw).
    pub count: u32,

    /// Whether to issue an indexed draw.
    pub indexed: bool,

    /// The mesh's local-space bounding box, used for CPU frustum culling.
    pub aabb: Aabb,

    /// Index of the mesh's entry in the renderer's mesh-metadata array.
    ///
    /// Every mesh owns an entry, whether or not the pipeline that draws it
    /// reads one.
    pub metadata_index: u32,

    /// Resource id of the mesh bind group, bound at
    /// [`MESH_GROUP`](wgpu_unlit_render::pipeline::MESH_GROUP).
    ///
    /// `None` when the mesh was uploaded without one.
    pub bind_group_id: Option<ResourceId>,
}

/// The per-entity request for one family's variant.
///
/// It carries a [PipelineKey] rather than a compiled pipeline: which concrete
/// pipeline an entity needs depends on the frame's render target and on the
/// entity's vertex layout, neither of which is known when the component is
/// created. The family the key belongs to is found from the key's type, and
/// the concrete pipeline is resolved against the frame and the mesh when the
/// entity is drawn.
///
/// Every renderable entity carries one: an entity without this component is
/// not drawn at all. The key selects the family, so an entity whose key type
/// no registered family uses is silently skipped.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GpuPipeline<Key> {
    /// The entity's pipeline key.
    pub(crate) key: Key,
}

impl<Key> GpuPipeline<Key> {
    /// A pipeline component carrying `key`.
    pub fn new(key: Key) -> Self {
        Self { key }
    }

    /// The entity's pipeline key.
    pub(crate) fn key(&self) -> &Key {
        &self.key
    }
}

/// The built-in unlit pipeline component.
///
/// It carries an [UnlitPipelineKey], which names the built-in unlit family and
/// supplies the options the entity's variants are specialized from.
pub type UnlitPipeline = GpuPipeline<UnlitPipelineKey>;

/// A handle to a material bind group in the renderer's GPU resource graph.
///
/// Created by
/// [`Renderer::allocate_material`](crate::Renderer::allocate_material) for a
/// caller's own layout, or by
/// [`Renderer::allocate_unlit_material`](crate::Renderer::allocate_unlit_material)
/// for the built-in shader's base-color texture and sampler. The handle stays
/// valid until [`Renderer::remove_material`](crate::Renderer::remove_material)
/// is called with it.
#[derive(Clone, Debug)]
pub struct GpuMaterial {
    /// Resource id of the material bind group (index [`MATERIAL_GROUP`]).
    ///
    /// [`MATERIAL_GROUP`]: wgpu_unlit_render::pipeline::MATERIAL_GROUP
    pub bind_group_id: ResourceId,
}

impl GpuMaterial {
    /// A stable key that groups draws sharing this material.
    ///
    /// The renderer sorts by it so consecutive draws bind the same material
    /// bind group. Equal keys mean the same material, so [`Ord`] on the
    /// underlying graph index is all the ordering the sort needs.
    pub fn sort_key(&self) -> u64 {
        self.bind_group_id.index() as u64
    }
}

/// Marker for entities whose draw order decides how they look.
///
/// Entities with this component are drawn after the ones without it and
/// sorted back-to-front by camera distance, which is what a blended draw
/// needs to composite correctly. Entities without it are drawn in
/// pipeline-registration order.
///
/// The marker only orders draws: it selects no pipeline and changes no blend
/// state, so the pipeline an entity resolves to has to blend on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZSortedDrawing;

/// Per-instance color, multiplying the base color.
///
/// The renderer packs it into the per-instance vertex stream next to the model
/// matrix, so two entities sharing a mesh can still be tinted differently.
/// Entities without this component are drawn white.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceColor {
    /// Base color (RGBA, unpremultiplied).
    pub color: glam::Vec4,
}

impl Default for InstanceColor {
    fn default() -> Self {
        Self {
            color: glam::Vec4::ONE,
        }
    }
}

impl InstanceColor {
    /// A color component carrying `color`.
    pub const fn new(color: glam::Vec4) -> Self {
        Self { color }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_transform_is_identity() {
        let t = Transform::default();
        assert_eq!(t.compute_matrix(), glam::Affine3A::IDENTITY);
    }

    #[test]
    fn transform_matrix_combines_components() {
        let t = Transform {
            translation: glam::Vec3::new(1.0, 2.0, 3.0),
            rotation: glam::Quat::from_rotation_y(1.5),
            scale: glam::Vec3::splat(2.0),
        };
        let expected = glam::Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(2.0),
            glam::Quat::from_rotation_y(1.5),
            glam::Vec3::new(1.0, 2.0, 3.0),
        );
        assert_eq!(t.compute_matrix(), expected);
    }

    #[test]
    fn default_instance_color_is_white() {
        assert_eq!(InstanceColor::default().color, glam::Vec4::ONE);
    }

    #[test]
    fn camera_fields_round_trip() {
        let c = Camera {
            clip_from_world: glam::camera::rh::view::look_at_mat4(
                glam::Vec3::new(0.0, 5.0, 10.0),
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            ),
            position: glam::Vec3::new(0.0, 5.0, 10.0),
        };
        assert_eq!(c.position, glam::Vec3::new(0.0, 5.0, 10.0));
    }
}
