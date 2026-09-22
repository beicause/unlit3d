//! Bounding volumes and frustum planes for CPU culling.
//!
//! A mesh carries its local-space [`Aabb`]; the renderer turns that into an
//! [`Obb`] with the entity's model matrix and tests it against the camera
//! [`FrustumPlanes`] before drawing. The culling itself lives in
//! [crate::culling].

use glam::{Affine3A, Vec3};

/// An axis-aligned bounding box in a mesh's local space.
///
/// A mesh uploads one of these with its [`MeshDesc`](crate::MeshDesc); it is
/// both the box the renderer culls against and the range the built-in
/// pipeline's position compression decodes from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Aabb {
    /// Center of the box.
    pub center: Vec3,
    /// Half-extents along each local axis.
    pub half_extents: Vec3,
}

impl Aabb {
    /// A degenerate box at the origin.
    pub const ZERO: Self = Self {
        center: Vec3::ZERO,
        half_extents: Vec3::ZERO,
    };

    /// A box from its center and half-extents.
    pub const fn new(center: Vec3, half_extents: Vec3) -> Self {
        Self {
            center,
            half_extents,
        }
    }

    /// The box spanning `min` to `max`, inclusive.
    pub fn from_min_max(min: Vec3, max: Vec3) -> Self {
        Self {
            center: (min + max) * 0.5,
            half_extents: (max - min) * 0.5,
        }
    }

    /// The corner with the smallest coordinate on every axis.
    pub fn min(&self) -> Vec3 {
        self.center - self.half_extents
    }

    /// The corner with the largest coordinate on every axis.
    pub fn max(&self) -> Vec3 {
        self.center + self.half_extents
    }

    /// Place this box in world space with `world_from_local`.
    ///
    /// A non-uniform scale makes the result an oriented box: the local axes
    /// keep their direction but the extents scale with them, which is exactly
    /// the box the transformed corners bound.
    pub fn transformed(&self, world_from_local: &Affine3A) -> Obb {
        let linear = world_from_local.matrix3;
        let mut axes = [Vec3::ZERO; 3];
        let mut half_extents = Vec3::ZERO;
        for axis in 0..3 {
            let transformed = linear.col(axis);
            half_extents[axis] = self.half_extents[axis] * transformed.length();
            axes[axis] = transformed.normalize_or_zero().into();
        }
        Obb {
            center: world_from_local.transform_point3(self.center),
            half_extents,
            axes,
        }
    }
}

/// An oriented bounding box in world space.
///
/// Built from an [`Aabb`] and a model matrix; the culler tests it against the
/// camera frustum before drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obb {
    /// Center of the box in world space.
    pub center: Vec3,
    /// Half-extents along each axis.
    pub half_extents: Vec3,
    /// World-space, unit-length axes, one per local axis.
    pub axes: [Vec3; 3],
}

impl Obb {
    /// Place `aabb` in world space with `world_from_local`.
    pub fn from_aabb(aabb: Aabb, world_from_local: &Affine3A) -> Self {
        aabb.transformed(world_from_local)
    }
}

/// Six frustum planes derived from a clip-space matrix.
///
/// Planes are extracted with the Gribb-Hartmann method and normalized, so
/// testing a point against a plane is testing a signed distance.
#[derive(Clone, Copy, Debug)]
pub struct FrustumPlanes {
    planes: [glam::Vec4; 6],
}

impl FrustumPlanes {
    /// Extract frustum planes from a clip-from-world matrix.
    ///
    /// The extraction is the generic `row3 +/- row` form, so it follows
    /// whatever depth convention the matrix encodes instead of assuming one.
    pub fn from_clip_from_world(clip_from_world: glam::Mat4) -> Self {
        let m = clip_from_world;
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        Self {
            planes: [
                Self::normalize_plane(r3 + r0),
                Self::normalize_plane(r3 - r0),
                Self::normalize_plane(r3 - r1),
                Self::normalize_plane(r3 + r1),
                Self::normalize_plane(r3 + r2),
                Self::normalize_plane(r3 - r2),
            ],
        }
    }

    fn normalize_plane(row: glam::Vec4) -> glam::Vec4 {
        let len = Vec3::new(row.x, row.y, row.z).length();
        if len > 1e-10 { row / len } else { row }
    }

    /// Whether `obb` is at least partly inside the frustum.
    ///
    /// A box is outside a plane when its center is beyond it by more than the
    /// box's own extent along the plane normal: the support radius is the sum
    /// of each axis's projected half-extent.
    pub fn test_obb(&self, obb: &Obb) -> bool {
        self.planes.iter().all(|&plane| {
            let normal = Vec3::new(plane.x, plane.y, plane.z);
            let radius = obb
                .axes
                .iter()
                .zip(obb.half_extents.to_array())
                .map(|(axis, half_extent)| normal.dot(*axis).abs() * half_extent)
                .sum::<f32>();
            plane.dot(obb.center.extend(1.0)) >= -radius
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_min_max_round_trips() {
        let aabb = Aabb::from_min_max(Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(aabb.center, Vec3::ZERO);
        assert_eq!(aabb.half_extents, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(aabb.min(), Vec3::new(-1.0, -2.0, -3.0));
        assert_eq!(aabb.max(), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn translation_moves_the_obb_center_only() {
        let aabb = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        let obb = aabb.transformed(&Affine3A::from_translation(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(obb.center, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(obb.half_extents, Vec3::splat(1.0));
        assert_eq!(obb.axes, [Vec3::X, Vec3::Y, Vec3::Z]);
    }

    #[test]
    fn rotation_turns_the_axes() {
        let aabb = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        let rotation = Affine3A::from_rotation_z(core::f32::consts::FRAC_PI_2);
        let obb = aabb.transformed(&rotation);
        assert!((obb.axes[0] - Vec3::Y).length() < 1e-6);
        assert!((obb.axes[1] + Vec3::X).length() < 1e-6);
        assert_eq!(obb.half_extents, Vec3::splat(1.0));
    }

    #[test]
    fn non_uniform_scale_stretches_the_extents() {
        let aabb = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        let obb = aabb.transformed(&Affine3A::from_scale(Vec3::new(2.0, 3.0, 4.0)));
        assert_eq!(obb.half_extents, Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(obb.axes, [Vec3::X, Vec3::Y, Vec3::Z]);
    }

    /// An axis-aligned unit box at the origin.
    fn unit_obb() -> Obb {
        Obb::from_aabb(Aabb::new(Vec3::ZERO, Vec3::splat(0.5)), &Affine3A::IDENTITY)
    }

    fn test_perspective() -> glam::Mat4 {
        glam::camera::rh::proj::opengl::perspective(1.0, 1.0, 0.1, 100.0)
    }

    #[test]
    fn frustum_planes_from_identity_accepts_origin() {
        let planes = FrustumPlanes::from_clip_from_world(glam::Mat4::IDENTITY);
        assert!(planes.test_obb(&unit_obb()));
    }

    #[test]
    fn frustum_culls_obb_behind_the_camera() {
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let clip_from_world = test_perspective() * view;
        let planes = FrustumPlanes::from_clip_from_world(clip_from_world);
        assert!(planes.test_obb(&unit_obb()));
        let far = |z| {
            Obb::from_aabb(
                Aabb::new(Vec3::new(0.0, 0.0, z), Vec3::splat(0.5)),
                &Affine3A::IDENTITY,
            )
        };
        assert!(!planes.test_obb(&far(20.0)));
    }

    #[test]
    fn frustum_culls_obb_outside_left_plane() {
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let clip_from_world = test_perspective() * view;
        let planes = FrustumPlanes::from_clip_from_world(clip_from_world);
        let left = Obb::from_aabb(
            Aabb::new(Vec3::new(-10.0, 0.0, 0.0), Vec3::splat(0.1)),
            &Affine3A::IDENTITY,
        );
        assert!(!planes.test_obb(&left));
    }
}
