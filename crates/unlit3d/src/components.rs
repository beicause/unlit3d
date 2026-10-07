//! Component types for the ECS-based scene description.
//!
//! These components live in an [`unlit_ecs`] world and are read by the
//! [`MeshSource`](crate::mesh_source::MeshSource) each frame to build the
//! draw commands.

use std::rc::Rc;

use arrayvec::ArrayVec;
use unlit_ecs::Entity;
use unlit_wgpu::mesh::JointMatrix;
use unlit_wgpu::offset_allocator::Allocation;
use unlit_wgpu::render_attachments::{color_clear, depth_clear, stencil_clear};
use unlit_wgpu::resources::{ResourceId, Virtual};
use unlit_wgpu::scene::MAX_VERTEX_BUFFERS;
use unlit_wgpu::specialize::VertexLayout;

use crate::bounds::Aabb;
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

/// Camera state: the view and projection transforms it draws through.
///
/// The renderer queries the first **active** entity that carries this
/// component to derive the camera uniforms and perform frustum culling. If no
/// entity has an active [`Camera`], the frame is cleared and nothing is drawn.
///
/// The view and projection are kept apart rather than pre-multiplied into one
/// clip-from-world matrix: a shader that works in view space, and the eye
/// position the frame uniform carries, are both derivable from
/// [`Camera::view_from_world`], while the combined matrix a vertex stage wants
/// is one multiplication away. Storing only the product would lose the view
/// matrix, which cannot be recovered from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// View matrix (world to view).
    pub view_from_world: glam::Mat4,
    /// Projection matrix (view to clip).
    pub clip_from_view: glam::Mat4,
    /// Whether the renderer may draw through this camera.
    ///
    /// An inactive camera is kept out of the frame entirely: the renderer
    /// skips it when choosing which camera to draw through, so a world can
    /// hold several cameras — the ones a caller switches between — and pick
    /// one per frame by toggling this flag rather than by respawning.
    pub active: bool,
}

impl Camera {
    /// The combined projection x view matrix (world to clip).
    #[must_use]
    pub fn clip_from_world(&self) -> glam::Mat4 {
        self.clip_from_view * self.view_from_world
    }

    /// The inverse view matrix (view to world).
    #[must_use]
    pub fn world_from_view(&self) -> glam::Mat4 {
        self.view_from_world.inverse()
    }

    /// The world-space eye position.
    ///
    /// The eye is the translation of the view-to-world matrix, so it is
    /// derived rather than stored: keeping it as a field would be a second
    /// source of truth for something the view matrix already says.
    #[must_use]
    pub fn position(&self) -> glam::Vec3 {
        self.world_from_view().col(3).truncate()
    }

    /// The depth of a world-space point along this camera's view axis.
    ///
    /// This is the point's view-space depth, not its euclidean distance: a
    /// point straight ahead is nearer than one the same euclidean distance away
    /// but off to the side. The renderer sorts [`ZSortedDrawing`] entities by
    /// it, so what decides their order is how deep into the scene they sit
    /// rather than how far they are from the eye at an angle.
    ///
    /// The depth comes straight out of the view matrix: it is the negated view
    /// space z, since a right-handed view space looks down its own `-z`. That
    /// holds for any projection — perspective, orthographic, off-axis or with
    /// an infinite far plane — because the projection decides view-to-clip, not
    /// how deep along the axis a point is.
    #[must_use]
    pub fn view_depth(&self, world_point: glam::Vec3) -> f32 {
        -self.view_from_world.transform_point3(world_point).z
    }
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

/// The parts of a [`GpuMesh`] that only drawing reads.
///
/// These are resolved once per distinct mesh per frame while assembling the
/// draw list, never once per entity: no per-entity walk on the frame path
/// touches them. Keeping them out of [`GpuMesh`] is what keeps a mesh column
/// cheap to walk, since the culling and resolve walks visit every entity.
///
/// Cloning a [`GpuMesh`] shares these parts rather than copying them, which is
/// what makes one mesh cheap to give to many entities.
#[derive(Clone, Debug)]
pub struct MeshParts {
    /// The mesh's virtual root node in the resource graph.
    ///
    /// It holds no GPU resource of its own and is the mesh's only lifetime
    /// entry point: the vertex and index buffers and the bind group a caller
    /// supplied are all nodes this one depends on, so dropping the last
    /// [`GpuMesh`] naming the root leaves the root — and with it the parts
    /// nothing else holds — for the next `maintain` to collect.
    pub root: ResourceId<Virtual>,

    /// Vertex buffers, each tagged with its slot index, in slot order.
    pub vertex_buffers: ArrayVec<(u32, ResourceId<wgpu::Buffer>), MAX_VERTEX_BUFFERS>,

    /// Index buffer, if the mesh is indexed.
    pub index_buffer: Option<(ResourceId<wgpu::Buffer>, wgpu::IndexFormat)>,

    /// Resource id of the mesh bind group, bound at
    /// [`MESH_GROUP`](unlit_wgpu::pipeline::MESH_GROUP).
    ///
    /// `None` when the mesh was uploaded without one.
    pub bind_group_id: Option<ResourceId<wgpu::BindGroup>>,

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

    /// The mesh's morph displacements' range in the frame-wide pool, to hand
    /// back when the mesh is removed.
    ///
    /// `None` when the mesh carries no morph targets. The displacements are the
    /// mesh's own, but they live in one array the whole frame shares, so the
    /// mesh holds a range rather than a resource.
    pub(crate) morph_deltas_allocation: Option<Allocation>,

    /// Index of the mesh's entry in the source's mesh-metadata array.
    ///
    /// Every mesh owns an entry, whether or not the pipeline that draws it
    /// reads one.
    pub metadata_index: u32,
}

/// A handle to a mesh stored in the source's GPU resource graph.
///
/// Created by [`MeshSource::allocate_mesh`](crate::mesh_source::MeshSource::allocate_mesh) or
/// [`MeshSourceUnlitExt::allocate_unlit_mesh`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_mesh).
/// The mesh is ready to draw immediately, and it lives as long as some
/// [`GpuMesh`] names it: dropping the last handle makes it collectable by the
/// next `maintain`, after which the pool ranges it held are handed back with
/// [`MeshSource::remove_mesh`](crate::mesh_source::MeshSource::remove_mesh).
///
/// The fields here are the ones a per-entity walk reads while culling and
/// resolving the visible set. Everything only drawing reads lives in
/// [`MeshParts`] behind the [`Self::parts`] handle.
///
/// The frame path borrows a mesh rather than copying it, so what a mesh's size
/// costs is cache traffic, not a value being moved: a column is contiguous, and
/// walking it pulls whole cache lines through the cache while only the cull and
/// resolve fields are used. A mesh that carried its buffers inline would drag
/// them through every walk for the sake of a few bytes of bounds. Keep new
/// fields on the side of that line that reads them: a per-entity field belongs
/// inline, a per-mesh one belongs in [`MeshParts`].
#[derive(Clone, Debug)]
pub struct GpuMesh {
    /// The buffers and allocations the draw names, shared between every clone.
    ///
    /// Shared rather than owned because one mesh is usually given to many
    /// entities: a clone is then a handle copy, where an owned copy would
    /// duplicate every buffer id per entity and give each its own set of
    /// resources to free. The parts are immutable once the mesh is uploaded, so
    /// sharing them costs no synchronization on the frame path.
    pub parts: Rc<MeshParts>,

    /// The vertex layout of the draw, slot by slot.
    ///
    /// A pipeline family specializes on this: two meshes whose layouts imply
    /// different pipeline descriptors resolve to different compiled pipelines.
    /// It is owned here so a key can name it without going back to the
    /// [`MeshDesc`](crate::mesh::MeshDesc) it was uploaded from. It may name a slot
    /// whose buffer the draw binds from the renderer rather than the mesh —
    /// the per-instance buffer, for one.
    ///
    /// Held as a shared handle rather than inline: the draw path keys on it
    /// once per visible entity per frame, and a handle makes that a pointer
    /// copy and one precomputed hash instead of a deep clone and a walk over
    /// every attribute.
    pub vertex_layout: VertexLayout,

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
/// [`GpuRenderPipeline`]: the pose itself lives on the entity this names, so two
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
/// It carries a [RenderPipelineKey](crate::pipeline::RenderPipelineKey) rather than a compiled pipeline: which concrete
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
pub struct GpuRenderPipeline<Key> {
    /// The entity's pipeline key.
    pub(crate) key: Key,
}

impl<Key> GpuRenderPipeline<Key> {
    /// A pipeline component carrying `key`.
    pub fn new(key: Key) -> Self {
        Self { key }
    }

    /// The entity's pipeline key.
    pub(crate) fn key(&self) -> &Key {
        &self.key
    }
}

/// A handle to a material bind group in the source's GPU resource graph.
///
/// Created by
/// [`MeshSource::allocate_material`](crate::mesh_source::MeshSource::allocate_material) for a
/// caller's own layout, or by
/// [`MeshSourceUnlitExt::allocate_unlit_material`](crate::unlit::MeshSourceUnlitExt::allocate_unlit_material)
/// for the built-in shader's base-color texture and sampler. The bind group
/// lives as long as some handle names it; dropping the last one makes it
/// collectable by the next `maintain`.
#[derive(Clone, Debug)]
pub struct GpuMaterial {
    /// Resource id of the material bind group (index [`MATERIAL_GROUP`]).
    ///
    /// [`MATERIAL_GROUP`]: unlit_wgpu::pipeline::MATERIAL_GROUP
    pub bind_group_id: ResourceId<wgpu::BindGroup>,
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
/// sorted back-to-front by their view-axis depth, which is what a blended
/// draw needs to composite correctly. Entities without it are drawn in
/// pipeline-registration order.
///
/// The marker only orders draws: it selects no pipeline and changes no blend
/// state, so the pipeline an entity resolves to has to blend on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZSortedDrawing;

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
    fn camera_fields_round_trip() {
        let eye = glam::Vec3::new(0.0, 5.0, 10.0);
        let view = glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y);
        let c = Camera {
            view_from_world: view,
            clip_from_view: glam::Mat4::IDENTITY,
            active: true,
        };
        assert_eq!(c.position(), eye);
        assert_eq!(c.clip_from_world(), view);
        assert!(c.active);
    }

    /// A camera at (0, 0, 5) looking down -Z, with a standard perspective.
    fn test_camera() -> Camera {
        let projection = glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0);
        let view = glam::camera::rh::view::look_at_mat4(
            glam::Vec3::new(0.0, 0.0, 5.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        );
        Camera {
            view_from_world: view,
            clip_from_view: projection,
            active: true,
        }
    }

    #[test]
    fn view_depth_grows_away_from_the_camera() {
        let camera = test_camera();
        assert!((camera.view_depth(glam::Vec3::ZERO) - 5.0).abs() < 1e-4);
        assert!((camera.view_depth(glam::Vec3::new(0.0, 0.0, 4.0)) - 1.0).abs() < 1e-4);
    }

    /// Depth is measured along the view axis, so moving a point straight to the
    /// side leaves it at the same depth even though it gets farther from the
    /// eye.
    #[test]
    fn view_depth_ignores_lateral_offset() {
        let camera = test_camera();
        let ahead = camera.view_depth(glam::Vec3::new(0.0, 0.0, 1.0));
        let sideways = camera.view_depth(glam::Vec3::new(3.0, 0.0, 1.0));
        assert!(
            (ahead - sideways).abs() < 1e-4,
            "a lateral offset changed the depth: {ahead} against {sideways}",
        );
    }

    /// A camera at (0, 0, 5) looking down -Z, with an orthographic projection.
    ///
    /// An orthographic projection has no perspective divide, so a depth read
    /// off the clip-space w alone would be constant.
    fn test_orthographic_camera() -> Camera {
        let projection =
            glam::camera::rh::proj::directx::orthographic(-1.0, 1.0, -1.0, 1.0, 0.1, 100.0);
        let view = glam::camera::rh::view::look_at_mat4(
            glam::Vec3::new(0.0, 0.0, 5.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        );
        Camera {
            view_from_world: view,
            clip_from_view: projection,
            active: true,
        }
    }

    /// The depth follows the view axis under an orthographic projection too,
    /// where the perspective divide carries none.
    #[test]
    fn orthographic_view_depth_grows_away_from_the_camera() {
        let camera = test_orthographic_camera();
        assert!((camera.view_depth(glam::Vec3::ZERO) - 5.0).abs() < 1e-4);
        assert!((camera.view_depth(glam::Vec3::new(0.0, 0.0, 4.0)) - 1.0).abs() < 1e-4);
    }

    /// And it is still measured along the view axis rather than to the eye.
    #[test]
    fn orthographic_view_depth_ignores_lateral_offset() {
        let camera = test_orthographic_camera();
        let ahead = camera.view_depth(glam::Vec3::new(0.0, 0.0, 1.0));
        let sideways = camera.view_depth(glam::Vec3::new(3.0, 0.0, 1.0));
        assert!(
            (ahead - sideways).abs() < 1e-4,
            "a lateral offset changed the depth: {ahead} against {sideways}",
        );
    }

    /// The derived view axis follows a camera that is not axis-aligned, and the
    /// eye position is the origin the depth is measured from.
    #[test]
    fn view_depth_follows_an_oblique_camera() {
        let eye = glam::Vec3::new(3.0, 4.0, 5.0);
        let view = glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y);
        let projection =
            glam::camera::rh::proj::directx::perspective_infinite_reverse(1.0, 1.0, 0.1);
        let camera = Camera {
            view_from_world: view,
            clip_from_view: projection,
            active: true,
        };
        // The forward axis is the eye-to-target direction, so the depth of the
        // target is the eye-to-target distance.
        let expected = eye.length();
        assert!(
            (camera.view_depth(glam::Vec3::ZERO) - expected).abs() < 1e-3,
            "the depth of the target is not the eye distance: {} against {expected}",
            camera.view_depth(glam::Vec3::ZERO),
        );
        // A point one unit along the eye-to-target direction is one unit deeper.
        let forward = (-eye).normalize();
        let one_deeper = camera.view_depth(forward) - camera.view_depth(glam::Vec3::ZERO);
        assert!(
            (one_deeper - 1.0).abs() < 1e-3,
            "one unit along the view axis is not one unit deeper: {one_deeper}",
        );
    }

    /// A camera at `eye` looking at `target`, with `projection`.
    fn camera_with(projection: glam::Mat4, eye: glam::Vec3, target: glam::Vec3) -> Camera {
        Camera {
            view_from_world: glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y),
            clip_from_view: projection,
            active: true,
        }
    }

    /// The depth agrees with the view matrix's own view axis, for every
    /// projection.
    ///
    /// The cross product of the combined matrix's first two rows is not that
    /// axis: an off-axis frustum's projection centre is shifted, so those rows
    /// mix in the camera's right and up directions and the cross product leaves
    /// the axis. The view matrix's z row does not.
    #[test]
    fn view_depth_matches_the_view_axis_for_every_projection() {
        let eye = glam::Vec3::new(3.0, 4.0, 5.0);
        let target = glam::Vec3::new(-1.0, 0.5, 1.0);
        let view = glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y);
        // A view matrix's z row is the camera's backward axis.
        let forward = -view.row(2).truncate().normalize();
        let projections = [
            (
                "perspective",
                glam::camera::rh::proj::opengl::perspective(1.0, 1.5, 0.1, 100.0),
            ),
            (
                "infinite reverse",
                glam::camera::rh::proj::directx::perspective_infinite_reverse(1.0, 1.5, 0.1),
            ),
            (
                "orthographic",
                glam::camera::rh::proj::directx::orthographic(-2.0, 2.0, -1.0, 1.0, 0.1, 100.0),
            ),
            // An off-axis frustum is not symmetric about the view axis.
            (
                "off-axis frustum",
                glam::camera::rh::proj::opengl::frustum(-0.4, 1.6, -0.3, 0.9, 0.1, 100.0),
            ),
            (
                "off-axis orthographic",
                glam::camera::rh::proj::opengl::orthographic(-0.5, 2.0, -0.4, 1.2, 0.1, 100.0),
            ),
        ];
        for (name, projection) in projections {
            let camera = camera_with(projection, eye, target);
            // A shifted perspective frustum mixes the camera's right and up
            // directions into its first two rows, which is what leaves the
            // cross product off the axis. This pins down that the method does
            // not lean on it.
            if name == "off-axis frustum" {
                let combined = camera.clip_from_world();
                let cross = combined
                    .row(0)
                    .truncate()
                    .cross(combined.row(1).truncate())
                    .normalize();
                assert!(
                    cross.dot(forward).abs() < 1.0 - 1e-3,
                    "the cross product was on the view axis, so this case tests nothing",
                );
            }
            for point in [
                target,
                eye + forward * 4.0,
                eye + forward * 9.0 + glam::Vec3::new(2.0, -1.0, 0.0),
            ] {
                let expected = forward.dot(point - eye);
                let actual = camera.view_depth(point);
                assert!(
                    (actual - expected).abs() < 1e-3,
                    "{name}: view_depth({point:?}) was {actual}, expected {expected}",
                );
            }
        }
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    /// The culling and resolve walks visit every entity, so a mesh column is
    /// pulled through the cache whole and its per-entity size is a cost the
    /// whole scene pays. This fails if a field no per-entity walk reads is
    /// added back inline.
    ///
    /// The unit is the column cell, not the mesh: a cell is a `RefCell`, whose
    /// borrow counter costs a word on top of the mesh. The bound is relative
    /// rather than a byte count, so it stays meaningful on any target: what
    /// matters is that the fields every walk reads stay a fraction of the mesh,
    /// with the per-mesh bulk in [`MeshParts`].
    #[test]
    fn a_mesh_cell_stays_small_beside_the_parts_it_excludes() {
        let cell = size_of::<core::cell::RefCell<GpuMesh>>();
        let parts = size_of::<MeshParts>();
        assert!(
            cell * 2 <= parts,
            "a mesh cell is {cell} bytes against {parts} of parts; every per-entity \
             walk pays for the cell, so move per-mesh fields into MeshParts",
        );
    }
}
