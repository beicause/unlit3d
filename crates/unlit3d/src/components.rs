//! Component types for the ECS-based scene description.
//!
//! These components live in an [`unlit_ecs`] world and are read by the
//! [`Renderer`](crate::Renderer) each frame to build the draw commands.

use wgpu_unlit_render::resources::ResourceId;

/// World-space transform (translation, rotation, scale).
///
/// The renderer reads this component to position each entity in world space.
/// When present alongside an [`InstanceData`] component, the renderer uses
/// the [`InstanceData`] matrix instead — the two are independent.
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
    /// The 4x4 affine matrix this transform represents, suitable for use as a
    /// model matrix.
    #[must_use]
    pub fn compute_matrix(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
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

/// A handle to a mesh stored in the renderer's GPU resource graph.
///
/// Created by [`Renderer::upload_mesh`](crate::Renderer::upload_mesh). The
/// mesh is ready to draw immediately; the handle stays valid for the lifetime
/// of the renderer (or until the mesh is explicitly removed from the graph).
#[derive(Clone, Debug)]
pub struct GpuMesh {
    /// Resource id of the mesh bind group (index [`MESH_GROUP`]).
    ///
    /// [`MESH_GROUP`]: wgpu_unlit_render::pipeline::MESH_GROUP
    pub bind_group_id: ResourceId,

    /// Vertex buffers, each tagged with its slot index.
    pub vertex_buffers: Vec<(u32, ResourceId)>,

    /// Index buffer, if the mesh is indexed.
    pub index_buffer: Option<(ResourceId, wgpu::IndexFormat)>,

    /// Number of indices (indexed draw) or vertices (non-indexed draw).
    pub count: u32,

    /// Whether to issue an indexed draw.
    pub indexed: bool,
}

/// A handle to a material (base-color texture + sampler) in the renderer's
/// GPU resource graph.
///
/// Created by [`Renderer::upload_texture`](crate::Renderer::upload_texture).
#[derive(Clone, Debug)]
pub struct GpuMaterial {
    /// Resource id of the material bind group (index [`MATERIAL_GROUP`]).
    ///
    /// [`MATERIAL_GROUP`]: wgpu_unlit_render::pipeline::MATERIAL_GROUP
    pub bind_group_id: ResourceId,
}

/// Bounding sphere used for frustum culling.
///
/// The renderer derives this from the mesh AABB at upload time. Entities
/// without this component are always drawn (no culling).
#[derive(Clone, Debug, PartialEq)]
pub struct BoundingSphere {
    /// Center of the bounding sphere, in local (pre-transform) space.
    pub center: glam::Vec3,

    /// Radius of the bounding sphere.
    pub radius: f32,
}

/// Per-instance transform override and base colour.
///
/// When this component is present, the renderer uses its matrix and colour
/// instead of deriving them from [`Transform`]. The matrix is the full affine
/// model matrix (scale, rotation, translation).
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceData {
    /// Affine model matrix.
    pub matrix: glam::Affine3A,
    /// Base colour (RGBA, unpremultiplied).
    pub base_color: glam::Vec4,
}

impl Default for InstanceData {
    fn default() -> Self {
        Self {
            matrix: glam::Affine3A::IDENTITY,
            base_color: glam::Vec4::new(1.0, 1.0, 1.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_transform_is_identity() {
        let t = Transform::default();
        assert_eq!(t.compute_matrix(), glam::Mat4::IDENTITY);
    }

    #[test]
    fn transform_matrix_combines_components() {
        let t = Transform {
            translation: glam::Vec3::new(1.0, 2.0, 3.0),
            rotation: glam::Quat::from_rotation_y(1.5),
            scale: glam::Vec3::splat(2.0),
        };
        let expected = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::splat(2.0),
            glam::Quat::from_rotation_y(1.5),
            glam::Vec3::new(1.0, 2.0, 3.0),
        );
        assert_eq!(t.compute_matrix(), expected);
    }

    #[test]
    fn default_instance_data_is_identity_and_white() {
        let d = InstanceData::default();
        assert_eq!(d.matrix, glam::Affine3A::IDENTITY);
        assert_eq!(d.base_color, glam::Vec4::new(1.0, 1.0, 1.0, 1.0));
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
