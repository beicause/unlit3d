//! Component types for the ECS-based scene description.
//!
//! These components live in an [`unlit_ecs`] world and are read by the
//! [`MeshSource`](crate::mesh_source::MeshSource) each frame to build the
//! draw commands.

use arrayvec::ArrayVec;
use unlit_ecs::Entity;
use wgpu_unlit_render::mesh::JointMatrix;
use wgpu_unlit_render::offset_allocator::Allocation;
use wgpu_unlit_render::render_attachments::{color_clear, depth_clear, stencil_clear};
use wgpu_unlit_render::resources::ResourceId;
use wgpu_unlit_render::scene::MAX_VERTEX_BUFFERS;
use wgpu_unlit_render::specialize::VertexBufferLayoutDesc;

use crate::bounds::Aabb;
use crate::mesh_source::UnlitPipelineKey;

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
/// attachment; the frame's internal attachment set may be depth-only.
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
    /// Defaults to [depth_clear], a clear to the frame's reverse-z
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

/// A handle to a mesh stored in the source's GPU resource graph.
///
/// Created by [`MeshSource::allocate_mesh`](crate::mesh_source::MeshSource::allocate_mesh) or
/// [`MeshSource::allocate_unlit_mesh`](crate::mesh_source::MeshSource::allocate_unlit_mesh).
/// The mesh is ready to draw immediately and the handle stays valid until
/// [`MeshSource::remove_mesh`](crate::mesh_source::MeshSource::remove_mesh) is called with it.
#[derive(Clone, Debug)]
pub struct GpuMesh {
    /// The mesh's virtual root node in the resource graph.
    ///
    /// It holds no GPU resource of its own and is the mesh's only lifetime
    /// entry point: the vertex and index buffers, the mesh bind group and the
    /// mesh-info uniform are all weak nodes registered under it, so
    /// [`MeshSource::remove_mesh`](crate::mesh_source::MeshSource::remove_mesh) frees the whole
    /// mesh by removing this one node and collecting the parts it leaves
    /// behind. The other ids below are for the draw path, which reads the
    /// parts directly.
    pub root: ResourceId,

    /// Vertex buffers, each tagged with its slot index, in slot order.
    pub vertex_buffers: ArrayVec<(u32, ResourceId), MAX_VERTEX_BUFFERS>,

    /// The vertex layout of the draw, slot by slot.
    ///
    /// A pipeline family specializes on this: two meshes whose layouts imply
    /// different pipeline descriptors resolve to different compiled pipelines.
    /// It is owned here so a key can name it without going back to the
    /// [`MeshDesc`](crate::mesh::MeshDesc) it was uploaded from. It may name a slot
    /// whose buffer the draw binds from the renderer rather than the mesh —
    /// the per-instance buffer, for one.
    pub vertex_layout: ArrayVec<(u32, VertexBufferLayoutDesc), MAX_VERTEX_BUFFERS>,

    /// Index buffer, if the mesh is indexed.
    pub index_buffer: Option<(ResourceId, wgpu::IndexFormat)>,

    /// Number of indices (indexed draw) or vertices (non-indexed draw).
    pub count: u32,

    /// Where the mesh's range starts inside the buffer it lives in.
    ///
    /// A draw binds the whole buffer, so what picks the mesh's slice out of it
    /// is the draw's range: this is the first index for an indexed draw, and
    /// the first vertex for a non-indexed one. A mesh that owns its buffer
    /// whole starts at `0`, so both kinds of mesh draw the same way.
    pub first: u32,

    /// Where the mesh's vertices start, in elements, for an indexed draw.
    ///
    /// An indexed draw adds this to every index it reads, which is how the
    /// indices find their vertices when the vertex stream does not start at the
    /// beginning of its buffer. Zero for a mesh that owns its buffers whole.
    pub base_vertex: u32,

    /// Whether to issue an indexed draw.
    pub indexed: bool,

    /// The mesh's local-space bounding box, used for CPU frustum culling.
    pub aabb: Aabb,

    /// The mesh's vertex range in the pool it was allocated from, to hand back
    /// when the mesh is removed.
    ///
    /// `None` when the mesh owns its vertex buffers whole.
    pub(crate) vertex_allocation: Option<Allocation>,

    /// The mesh's index range in the pool it was allocated from, to hand back
    /// when the mesh is removed.
    ///
    /// `None` when the mesh owns its index buffer whole.
    pub(crate) index_allocation: Option<Allocation>,

    /// Index of the mesh's entry in the source's mesh-metadata array.
    ///
    /// Every mesh owns an entry, whether or not the pipeline that draws it
    /// reads one.
    pub metadata_index: u32,

    /// Resource id of the mesh bind group, bound at
    /// [`MESH_GROUP`](wgpu_unlit_render::pipeline::MESH_GROUP).
    ///
    /// `None` when the mesh was uploaded without one.
    pub bind_group_id: Option<ResourceId>,

    /// How many morph targets the mesh carries, zero when it has none.
    ///
    /// A morph target's displacement is storage data rather than a vertex
    /// attribute, so this — not the vertex layout — is what tells a draw's key
    /// that the mesh reads morph positions. It is also the number of weights
    /// the [`MorphWeights`] entity a mesh references has to hold.
    pub morph_targets: u32,

    /// Whether the mesh's position stream carries joint indices and weights.
    ///
    /// A skinned draw reads the joint matrices a [`SkinPose`] entity holds, so
    /// a mesh with this set has to say which entity that is with a
    /// [`SkinBinding`].
    pub skinned: bool,
}

/// The joint matrices one pose is drawn with, as animating code updates them.
///
/// This is a component: put it on an entity of its own and let the renderer
/// upload it, so animating a skeleton is one component write and no GPU call.
/// A mesh is drawn by this pose when its own entity names that entity with a
/// [`SkinBinding`]. One pose entity may back any number of meshes, and a mesh
/// may be given its own pose entity to deform independently.
///
/// The matrices are the joint's animated transform times the inverse of the
/// transform it was bound in, applied in the mesh's own space. A vertex is
/// deformed by the weighted sum of the matrices its own joint indices name, so
/// the pose has to hold at least as many matrices as the highest index any of
/// its meshes uses; allocation cannot check that, and reading past the end of
/// the uploaded pose is a GPU out-of-bounds access.
///
/// Only meshes whose variant reads joints see this: one that reads none ignores
/// the pose it was bound to, exactly as it ignores a UV slice it does not
/// declare.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkinPose {
    /// One matrix per joint, in the order the mesh's joint indices address
    /// them.
    pub matrices: Vec<JointMatrix>,
}

impl SkinPose {
    /// A pose of `matrices`, one per joint.
    pub fn new(matrices: impl Into<Vec<JointMatrix>>) -> Self {
        Self {
            matrices: matrices.into(),
        }
    }
}

/// The morph-target weights one pose blends with, as animating code updates
/// them.
///
/// Like [`SkinPose`] this is a component of its own entity, referenced by the
/// meshes drawn with it through a [`MorphBinding`]; a mesh without one requires
/// no weights. The weights apply to the targets in order, so the pose has to
/// hold exactly as many weights as each of its meshes has targets —
/// [`GpuMesh::morph_targets`] — and the renderer checks that every frame rather
/// than reading weights that do not exist.
///
/// A weight of zero skips its target's displacement entirely, which is how a
/// pose blends between targets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphWeights {
    /// One weight per morph target, in target order.
    pub weights: Vec<f32>,
}

impl MorphWeights {
    /// A pose blending `weights`, one per target.
    pub fn new(weights: impl Into<Vec<f32>>) -> Self {
        Self {
            weights: weights.into(),
        }
    }
}

/// Names the [`SkinPose`] entity a mesh is drawn with.
///
/// Put this on the mesh's own entity, alongside its [`GpuMesh`] and
/// [`GpuPipeline`]: the pose itself lives on the entity this names, so two
/// meshes may share one pose by naming the same entity, or deform independently
/// by naming different ones.
///
/// A mesh whose vertex stream carries joints needs this; one whose stream does
/// not ignores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkinBinding {
    /// The entity carrying the [`SkinPose`] this mesh is drawn with.
    pub pose: Entity,
}

impl SkinBinding {
    /// Bind a mesh to the pose on `pose`.
    pub const fn new(pose: Entity) -> Self {
        Self { pose }
    }
}

/// Names the [`MorphWeights`] entity a mesh is drawn with.
///
/// The counterpart of [`SkinBinding`] for morph targets: the weights live on
/// the entity this names, so several meshes can share one blended pose.
///
/// A mesh with morph targets needs this; one with none ignores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MorphBinding {
    /// The entity carrying the [`MorphWeights`] this mesh is drawn with.
    pub weights: Entity,
}

impl MorphBinding {
    /// Bind a mesh to the weights on `weights`.
    pub const fn new(weights: Entity) -> Self {
        Self { weights }
    }
}

/// The per-entity request for one family's variant.
///
/// It carries a [PipelineKey](crate::pipeline::PipelineKey) rather than a compiled pipeline: which concrete
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

/// A handle to a material bind group in the source's GPU resource graph.
///
/// Created by
/// [`MeshSource::allocate_material`](crate::mesh_source::MeshSource::allocate_material) for a
/// caller's own layout, or by
/// [`MeshSource::allocate_unlit_material`](crate::mesh_source::MeshSource::allocate_unlit_material)
/// for the built-in shader's base-color texture and sampler. The handle stays
/// valid until [`MeshSource::remove_material`](crate::mesh_source::MeshSource::remove_material)
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
