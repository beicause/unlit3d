//! The frame's lights, as a scene spawns them.
//!
//! A light is one component -- [`LightSource`], holding a boxed [`Light`] -- so a
//! scene keeps every kind it has in one column and the frame collects them all
//! with one query. Which kind a light is decides which lanes of
//! [`unlit_wgpu::light::LightGpu`] it fills and which term the fragment adds; the
//! clustering, the cell records and the upload are shared by every kind.
//!
//! # The transform decides where a light points
//!
//! Every light shines along its own transform's `-Z`, so a directional or spot
//! light is aimed by rotating its entity rather than by a direction field, and a
//! quad light's polygon lies in its entity's `XY` plane. A light with no
//! [`Transform`] sits at the origin facing `-Z`.
//!
//! # Intensity is dimensionless
//!
//! A light's `intensity` carries no unit. The shader modulates a surface's base
//! colour by the sum of the lights' colours times their intensities and
//! normalises nothing -- there is no exposure and no energy conservation -- so a
//! caller picks numbers that look right rather than physical values.

use glam::{Affine3A, UVec4, Vec2, Vec3, Vec4};
use unlit_ecs::Scope;
use unlit_wgpu::light::{BoundingSphere, LightGpu, LightKind as GpuLightKind, PackedLight};

use crate::components::Transform;

/// A light's shape, as a description rather than the shader cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LightKind {
    /// A light at infinity: one direction, no falloff, no bounds.
    Directional,
    /// A light at a point, fading with distance inside its range.
    Point,
    /// A point light narrowed to a cone.
    Spot,
    /// A convex planar polygon radiating from one side of its plane.
    Quad,
}

/// What a light is, for a bridge that reports the frame's lights rather than
/// shades with them.
///
/// The record a [`LightGpu`] holds is what the shader reads and is deliberately
/// lane-shaped; this is the same light in the terms a caller spawned it, so a
/// report names kinds and shapes instead of lane numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightDescribe {
    /// Which kind of light this is.
    pub kind: LightKind,
    /// The light's linear colour; alpha is unused.
    pub color: Vec4,
    /// The light's dimensionless intensity.
    pub intensity: f32,
    /// How far the light reaches, for the kinds that fade; `None` for a
    /// directional light, which reaches everywhere.
    pub range: Option<f32>,
    /// The light's own shape: a spot light's inner and outer half-angles, or a
    /// quad light's area. `None` for a point or directional light.
    pub shape: Option<Vec2>,
}

/// A light a scene can hold.
///
/// Implemented by the four built-in kinds and by any kind a caller adds: the
/// frame only ever calls [`Self::pack`] and [`Self::describe`], so a new shape of
/// light is a new implementation rather than a new component, and every light
/// still shares one query, one array and one cluster pass.
pub trait Light: 'static {
    /// Write this light's record into `sink`, given the transform placing it.
    ///
    /// The sink starts as an empty record, so an implementation fills the lanes
    /// its kind uses and leaves the rest at zero. A light with vertices appends
    /// them to the sink's pool and names the slice it took.
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>);
    /// Describe this light in the terms it was spawned in.
    fn describe(&self) -> LightDescribe;
}

/// Where a [`Light::pack`] writes: its record, and the frame's shared polygon
/// vertex pool.
pub struct LightSink<'a> {
    /// The record to fill.
    pub gpu: &'a mut LightGpu,
    /// The pool every light's polygon vertices share, appended to in world
    /// space.
    pub vertices: &'a mut Vec<Vec4>,
}

/// A light in a scene, whatever kind it is.
///
/// One component type for every kind, because the frame collects them all the
/// same way: a scene adds a light by spawning this, and the frame's one query
/// finds directional, point, spot and quad lights together.
pub struct LightSource(pub Box<dyn Light>);

impl LightSource {
    /// A light source holding `light`.
    pub fn new(light: impl Light) -> Self {
        Self(Box::new(light))
    }
}

/// A light at infinity, shining along its transform's `-Z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalLight {
    /// The light's linear colour; alpha is unused.
    pub color: Vec4,
    /// The light's dimensionless intensity.
    pub intensity: f32,
}

impl Light for DirectionalLight {
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>) {
        sink.gpu.direction_outer = world_from_local
            .transform_vector3(Vec3::NEG_Z)
            .normalize_or_zero()
            .extend(0.0);
        sink.gpu.color_intensity = color_intensity(self.color, self.intensity);
        sink.gpu.kind.x = GpuLightKind::Directional as u32;
    }

    fn describe(&self) -> LightDescribe {
        LightDescribe {
            kind: LightKind::Directional,
            color: self.color,
            intensity: self.intensity,
            range: None,
            shape: None,
        }
    }
}

/// A light at a point, fading to nothing at its range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointLight {
    /// The light's linear colour; alpha is unused.
    pub color: Vec4,
    /// The light's dimensionless intensity.
    pub intensity: f32,
    /// How far the light reaches, in world units.
    pub range: f32,
}

impl Light for PointLight {
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>) {
        sink.gpu.position_radius = world_from_local.translation.extend(self.range);
        sink.gpu.color_intensity = color_intensity(self.color, self.intensity);
        sink.gpu.kind.x = GpuLightKind::Point as u32;
    }

    fn describe(&self) -> LightDescribe {
        LightDescribe {
            kind: LightKind::Point,
            color: self.color,
            intensity: self.intensity,
            range: Some(self.range),
            shape: None,
        }
    }
}

/// A [`PointLight`] narrowed to a cone along the transform's `-Z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotLight {
    /// The light's linear colour; alpha is unused.
    pub color: Vec4,
    /// The light's dimensionless intensity.
    pub intensity: f32,
    /// How far the light reaches, in world units.
    pub range: f32,
    /// The cone's full angle of constant brightness, in radians.
    pub inner_angle: f32,
    /// The cone's full angle where the light reaches nothing, in radians.
    ///
    /// Brightness falls off from the inner to the outer angle; a caller that
    /// wants a hard edge sets both to the same angle.
    pub outer_angle: f32,
}

impl Light for SpotLight {
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>) {
        let inner = (self.inner_angle * 0.5).cos();
        let outer = (self.outer_angle * 0.5).cos();
        let gap = inner - outer;
        sink.gpu.position_radius = world_from_local.translation.extend(self.range);
        sink.gpu.direction_outer = world_from_local
            .transform_vector3(Vec3::NEG_Z)
            .normalize_or_zero()
            .extend(outer);
        sink.gpu.color_intensity = color_intensity(self.color, self.intensity);
        sink.gpu.cones = Vec4::new(
            inner,
            if gap.abs() < MIN_CONE_GAP {
                0.0
            } else {
                1.0 / gap
            },
            0.0,
            0.0,
        );
        sink.gpu.kind.x = GpuLightKind::Spot as u32;
    }

    fn describe(&self) -> LightDescribe {
        LightDescribe {
            kind: LightKind::Spot,
            color: self.color,
            intensity: self.intensity,
            range: Some(self.range),
            shape: Some(Vec2::new(self.inner_angle, self.outer_angle)),
        }
    }
}

/// A convex planar polygon radiating from one side of its plane.
///
/// The light fades with distance from `range` like a point light and adds the
/// polygon's own form factor, so a patch wider than its range still lights the
/// corners it covers. The vertices are in the light's own space and lie in one plane; the frame
/// transforms them by the light's transform and shades with the polygon's form
/// factor, so the light is its shape rather than a bounding rectangle. A quad
/// light shines out of the side its vertices wind towards: the frame reverses
/// the winding when it faces away from the transform's `-Z`, so a caller states
/// a polygon and not a handedness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuadLight {
    /// The light's linear colour; alpha is unused.
    pub color: Vec4,
    /// The light's dimensionless intensity.
    pub intensity: f32,
    /// How far the light reaches, in world units.
    pub range: f32,
    /// The polygon's corners, in the light's own space.
    pub vertices: [Vec3; 4],
}

impl QuadLight {
    /// A quad light on a `width` by `height` rectangle centered on its
    /// transform, shining along the transform's `-Z`.
    pub fn rect(color: Vec4, intensity: f32, range: f32, width: f32, height: f32) -> Self {
        let half_width = width * 0.5;
        let half_height = height * 0.5;
        Self {
            color,
            intensity,
            range,
            vertices: [
                Vec3::new(-half_width, -half_height, 0.0),
                Vec3::new(half_width, -half_height, 0.0),
                Vec3::new(half_width, half_height, 0.0),
                Vec3::new(-half_width, half_height, 0.0),
            ],
        }
    }
}

impl Light for QuadLight {
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>) {
        let mut vertices = self.vertices;
        let normal = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
        if normal.dot(Vec3::NEG_Z) < 0.0 {
            vertices.reverse();
        }
        let world = vertices.map(|vertex| world_from_local.transform_point3(vertex));
        let center = (world[0] + world[1] + world[2] + world[3]) * 0.25;
        // The light reaches its polygon wherever the polygon lies, so the radius
        // that culls it has to hold the corners and not just the range: a patch
        // wider than the range still lights its far edge.
        let extent = world
            .iter()
            .map(|vertex| vertex.distance(center))
            .fold(0.0_f32, f32::max);
        let offset = sink.vertices.len() as u32;
        for vertex in world {
            sink.vertices.push(vertex.extend(1.0));
        }
        sink.gpu.position_radius = center.extend(self.range.max(0.0) + extent);
        sink.gpu.color_intensity = color_intensity(self.color, self.intensity);
        sink.gpu.kind = UVec4::new(GpuLightKind::Quad as u32, offset, 4, 0);
    }

    fn describe(&self) -> LightDescribe {
        LightDescribe {
            kind: LightKind::Quad,
            color: self.color,
            intensity: self.intensity,
            range: Some(self.range),
            shape: Some(Vec2::splat(polygon_area(&self.vertices))),
        }
    }
}

/// The smallest gap between a spot light's inner and outer cone cosines the
/// falloff divides by, so a cone with no soft edge does not divide by zero.
const MIN_CONE_GAP: f32 = 1.0e-4;

/// The colour and intensity lanes a record stores.
fn color_intensity(color: Vec4, intensity: f32) -> Vec4 {
    Vec4::new(color.x, color.y, color.z, intensity)
}

/// The area of a planar polygon, from its corners in order.
fn polygon_area(vertices: &[Vec3]) -> f32 {
    let mut sum = Vec3::ZERO;
    for index in 0..vertices.len() {
        sum += vertices[index].cross(vertices[(index + 1) % vertices.len()]);
    }
    sum.length() * 0.5
}

/// Pack every light the frame can see into `packed`, and every polygon corner
/// into `vertices`.
///
/// `eye` is where the frame is seen from, which only ranks the lights: a cell
/// that runs out of room keeps the lights reaching it strongest, and a light is
/// strongest near the camera. The ranking also settles the order the lights are
/// drawn in, so it decides the cluster's indices too.
pub(crate) fn collect_lights(
    scope: &Scope<'_>,
    eye: Vec3,
    packed: &mut Vec<PackedLight>,
    vertices: &mut Vec<Vec4>,
) {
    packed.clear();
    vertices.clear();
    for (source, transform) in scope.query::<(&LightSource, Option<&Transform>)>() {
        let world_from_local = transform
            .map(|transform| transform.compute_matrix())
            .unwrap_or(Affine3A::IDENTITY);
        let mut gpu = LightGpu {
            position_radius: Vec4::ZERO,
            direction_outer: Vec4::ZERO,
            color_intensity: Vec4::ZERO,
            cones: Vec4::ZERO,
            kind: UVec4::ZERO,
        };
        source.0.pack(
            &world_from_local,
            LightSink {
                gpu: &mut gpu,
                vertices,
            },
        );
        let describe = source.0.describe();
        let directional = gpu.kind.x == GpuLightKind::Directional as u32;
        let bounds = (!directional).then(|| BoundingSphere {
            center: gpu.position_radius.truncate(),
            radius: gpu.position_radius.w,
        });
        // A light near the camera lights more of the picture than one far away,
        // and a larger patch more than a smaller one, so the ranking is roughly
        // the illumination the light delivers where the frame is seen from.
        let area = match describe.kind {
            LightKind::Quad => describe.shape.map_or(1.0, |shape| shape.x.max(0.0)),
            _ => 1.0,
        };
        let distance = bounds
            .map_or(1.0, |bounds| (bounds.center - eye).length())
            .max(1.0);
        let importance = if directional {
            describe.intensity
        } else {
            describe.intensity * area / (distance * distance)
        };
        packed.push(PackedLight {
            gpu,
            bounds,
            importance,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack a light as the frame would, returning its record and the vertices
    /// it appended.
    fn pack(light: &impl Light, transform: Affine3A) -> (LightGpu, Vec<Vec4>) {
        let mut gpu = LightGpu {
            position_radius: Vec4::ZERO,
            direction_outer: Vec4::ZERO,
            color_intensity: Vec4::ZERO,
            cones: Vec4::ZERO,
            kind: UVec4::ZERO,
        };
        let mut vertices = Vec::new();
        light.pack(
            &transform,
            LightSink {
                gpu: &mut gpu,
                vertices: &mut vertices,
            },
        );
        (gpu, vertices)
    }

    #[test]
    fn a_light_shines_along_its_transforms_negative_z() {
        let light = DirectionalLight {
            color: Vec4::ONE,
            intensity: 2.0,
        };
        let (gpu, _) = pack(&light, Affine3A::IDENTITY);
        assert_eq!(gpu.direction_outer.truncate(), Vec3::NEG_Z);
        assert_eq!(gpu.color_intensity.w, 2.0);
        assert_eq!(gpu.kind.x, GpuLightKind::Directional as u32);

        // A quarter turn about Y aims the light along -X.
        let (turned, _) = pack(
            &light,
            Affine3A::from_rotation_y(core::f32::consts::FRAC_PI_2),
        );
        assert!((turned.direction_outer.truncate() - Vec3::NEG_X).length() < 1e-6);
    }

    #[test]
    fn a_spot_light_stores_the_reciprocal_of_its_cone_gap() {
        let light = SpotLight {
            color: Vec4::ONE,
            intensity: 1.0,
            range: 4.0,
            inner_angle: 0.2,
            outer_angle: 0.6,
        };
        let (gpu, _) = pack(&light, Affine3A::IDENTITY);
        let inner = (0.1_f32).cos();
        let outer = (0.3_f32).cos();
        assert_eq!(gpu.cones.x, inner);
        assert_eq!(gpu.direction_outer.w, outer);
        assert!((gpu.cones.y - 1.0 / (inner - outer)).abs() < 1e-6);
        assert_eq!(gpu.position_radius.w, 4.0);
    }

    #[test]
    fn a_hard_edged_spot_light_does_not_divide_by_zero() {
        let light = SpotLight {
            color: Vec4::ONE,
            intensity: 1.0,
            range: 4.0,
            inner_angle: 0.5,
            outer_angle: 0.5,
        };
        let (gpu, _) = pack(&light, Affine3A::IDENTITY);
        assert_eq!(gpu.cones.y, 0.0);
    }

    #[test]
    fn a_quad_light_takes_the_winding_that_faces_its_aim() {
        let quad = QuadLight {
            color: Vec4::ONE,
            intensity: 1.0,
            range: 8.0,
            vertices: [
                Vec3::new(-1.0, -1.0, 0.0),
                Vec3::new(1.0, -1.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(-1.0, 1.0, 0.0),
            ],
        };
        let (gpu, vertices) = pack(&quad, Affine3A::IDENTITY);
        assert_eq!(gpu.kind.x, GpuLightKind::Quad as u32);
        assert_eq!(gpu.kind.y, 0, "the first light starts the vertex pool");
        assert_eq!(gpu.kind.z, 4);
        assert_eq!(vertices.len(), 4);
        let normal = (vertices[1] - vertices[0])
            .truncate()
            .cross((vertices[2] - vertices[0]).truncate());
        assert!(
            normal.normalize().dot(Vec3::NEG_Z) > 0.0,
            "the frame reverses a winding that faces away from the aim"
        );
    }

    #[test]
    fn a_quad_light_places_its_corners_in_the_world() {
        let quad = QuadLight::rect(Vec4::ONE, 1.0, 8.0, 2.0, 2.0);
        let transform = Affine3A::from_translation(Vec3::new(3.0, 0.0, 0.0));
        let (gpu, vertices) = pack(&quad, transform);
        assert_eq!(gpu.position_radius.truncate(), Vec3::new(3.0, 0.0, 0.0));
        assert!(vertices.iter().all(|vertex| vertex.x >= 2.0 - 1e-6));
    }

    #[test]
    fn a_polygon_area_follows_its_corners() {
        let square = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 3.0, 0.0),
            Vec3::new(0.0, 3.0, 0.0),
        ];
        assert_eq!(polygon_area(&square), 6.0);
    }
}
