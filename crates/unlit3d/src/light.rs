//! The frame's lights, and how a frame clusters them.
//!
//! A light is one component -- [`LightSource`](crate::light::LightSource),
//! holding a boxed [`Light`](crate::light::Light) -- so a
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
//! [`Transform`](crate::components::Transform) sits at the origin facing `-Z`.
//!
//! # Intensity is dimensionless
//!
//! A light's `intensity` carries no unit. The shader modulates a surface's base
//! colour by the sum of the lights' colours times their intensities and
//! normalises nothing -- there is no exposure and no energy conservation -- so a
//! caller picks numbers that look right rather than physical values.

use glam::{Affine3A, UVec4, Vec2, Vec3, Vec4};
use unlit_ecs::Scope;
use unlit_wgpu::light::{
    BoundingSphere, DirectionalGpu, LightGpu, LightKind as GpuLightKind, LightingUniform,
    MAX_DIRECTIONAL_LIGHTS, PackedLight,
};

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
    ///
    /// `world_from_local` is the transform [`Self::pack`] was given: a shape
    /// whose size is part of its importance has to be measured where it is
    /// placed, since a transform can scale it.
    fn describe(&self, world_from_local: &Affine3A) -> LightDescribe;
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

/// The colour every lit surface receives before the lights are added.
///
/// A frame sums every ambient light in the source's scope, so a scene that
/// wants the term to change over time spawns, edits or removes the component
/// rather than writing a field on the source. It carries no intensity of its
/// own: the colour is the term.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AmbientLight {
    /// The ambient term, in the linear space the lights' colours are in.
    pub color: Vec4,
}

impl AmbientLight {
    /// An ambient term of the given colour.
    pub const fn new(color: Vec4) -> Self {
        Self { color }
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

    fn describe(&self, _world_from_local: &Affine3A) -> LightDescribe {
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

    fn describe(&self, _world_from_local: &Affine3A) -> LightDescribe {
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
    /// wants a hard edge sets both to the same angle, and the light then shines
    /// at full intensity inside the cone and not at all outside it. An inner
    /// angle wider than this one is narrowed to it, which is the same hard edge
    /// at the outer angle.
    pub outer_angle: f32,
}

impl Light for SpotLight {
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>) {
        // An inner cone wider than the outer one describes no ramp: it would
        // make the light brightest where it should reach nothing. Narrowing it
        // to the outer one is the closest cone the angles do describe, and is
        // the hard edge a caller writing the two the same way gets.
        let inner = (self.inner_angle.min(self.outer_angle) * 0.5).cos();
        let outer = (self.outer_angle * 0.5).cos();
        let gap = inner - outer;
        sink.gpu.position_radius = world_from_local.translation.extend(self.range);
        sink.gpu.direction_outer = world_from_local
            .transform_vector3(Vec3::NEG_Z)
            .normalize_or_zero()
            .extend(outer);
        sink.gpu.color_intensity = color_intensity(self.color, self.intensity);
        // A gap too narrow to divide by is stored as zero rather than as an
        // enormous reciprocal: the shader reads that as `the cone is hard` and
        // steps at the outer cosine instead of ramping between the two. The
        // cosines also round towards each other, so the gap can land below zero
        // for two angles that differ by less than the rounding; the mark has to
        // cover that side too, or the shader reads a negative one as `negative
        // is narrow` without the step.
        sink.gpu.cones = Vec4::new(
            if gap < MIN_CONE_GAP { 0.0 } else { 1.0 / gap },
            0.0,
            0.0,
            0.0,
        );
        sink.gpu.kind.x = GpuLightKind::Spot as u32;
    }

    fn describe(&self, _world_from_local: &Affine3A) -> LightDescribe {
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
/// The shading is the polygon's own form factor, which already carries the
/// inverse-square falloff of every point of it, so a patch wider than its range
/// still lights the corners it covers and the light has no attenuation term of
/// its own. The vertices are in the light's own space and lie in one plane; the
/// frame transforms them by the light's transform and shades with the polygon's
/// form factor, so the light is its shape rather than a bounding rectangle. A
/// quad light shines out of the side its vertices wind towards: the frame
/// reverses the winding when it faces away from the transform's `-Z`, so a
/// caller states a polygon and not a handedness.
///
/// A triangle states itself by repeating a corner. The frame drops corners that
/// coincide, so the polygon is shaded with as many corners as it has distinct
/// ones, and three of them are a triangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuadLight {
    /// The light's linear colour; alpha is unused.
    pub color: Vec4,
    /// The light's dimensionless intensity.
    pub intensity: f32,
    /// How far the light reaches, in world units.
    pub range: f32,
    /// The polygon's corners in order, in the light's own space.
    ///
    /// A quad uses all four; a triangle repeats one of them, because a convex
    /// polygon needs three corners and the array has room for a fourth that a
    /// horizon clip would produce.
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

/// Corners closer together than this are one corner, which is how a triangle
/// stated as a quad collapses to three.
const MERGED_CORNER_SQUARED: f32 = 1.0e-8;

impl Light for QuadLight {
    fn pack(&self, world_from_local: &Affine3A, sink: LightSink<'_>) {
        // Drop a corner that sits on its predecessor, which leaves as many
        // corners as the polygon has distinct ones: three for a triangle stated
        // as a quad, four for a quad. A polygon left with fewer than three has
        // no area, and the shader draws nothing for it. The corners are matched
        // in the light's own space, where the caller stated them, so a scaled
        // light keeps the corners it was given.
        let mut live = [Vec3::ZERO; 4];
        let mut count = 0;
        for corner in self.vertices {
            if count > 0 && (live[count - 1] - corner).length_squared() <= MERGED_CORNER_SQUARED {
                continue;
            }
            live[count] = corner;
            count += 1;
        }
        if count > 1 && (live[0] - live[count - 1]).length_squared() <= MERGED_CORNER_SQUARED {
            count -= 1;
        }
        let offset = sink.vertices.len() as u32;
        sink.gpu.color_intensity = color_intensity(self.color, self.intensity);
        if count < 3 {
            // A corner or a line: no area to integrate over, so the light names
            // no corners and the shader draws nothing for it. The record still
            // carries a position, so culling has somewhere to put it.
            let anchor = world_from_local.transform_point3(self.vertices[0]);
            sink.gpu.position_radius = anchor.extend(self.range.max(0.0));
            sink.gpu.kind = UVec4::new(GpuLightKind::Quad as u32, offset, 0, 0);
            return;
        }
        let mut world = live.map(|corner| world_from_local.transform_point3(corner));
        // The winding decides which side of the polygon shines, so it has to be
        // read where the polygon lands: an affine transform with a negative
        // determinant mirrors it, which turns the same order of corners into the
        // other side. Reading it in the light's own space would leave such a
        // light shining into the surface it was aimed at.
        let aim = world_from_local.transform_vector3(Vec3::NEG_Z);
        let normal = (world[1] - world[0]).cross(world[2] - world[0]);
        if normal.dot(aim) < 0.0 {
            world[..count].reverse();
        }
        let center = world[..count].iter().copied().sum::<Vec3>() / count as f32;
        // The light reaches its polygon wherever the polygon lies, so the radius
        // that culls it has to hold the corners and not just the range: a patch
        // wider than the range still lights its far edge.
        let extent = world[..count]
            .iter()
            .map(|corner| corner.distance(center))
            .fold(0.0_f32, f32::max);
        for corner in &world[..count] {
            sink.vertices.push(corner.extend(1.0));
        }
        sink.gpu.position_radius = center.extend(self.range.max(0.0) + extent);
        sink.gpu.kind = UVec4::new(GpuLightKind::Quad as u32, offset, count as u32, 0);
    }

    fn describe(&self, world_from_local: &Affine3A) -> LightDescribe {
        // The area that decides the light's importance is the area it covers in
        // the world: a transform can scale the polygon, and a light scaled up
        // lights more surface and so ranks above one scaled down.
        let world = self
            .vertices
            .map(|corner| world_from_local.transform_point3(corner));
        LightDescribe {
            kind: LightKind::Quad,
            color: self.color,
            intensity: self.intensity,
            range: Some(self.range),
            shape: Some(Vec2::splat(polygon_area(&world))),
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

/// Pack every light the frame can see: the bounded ones into `packed`, every
/// polygon corner into `vertices`, and the lights at infinity into
/// `lighting`.
///
/// `eye` is where the frame is seen from, which only ranks the lights: a cell
/// that runs out of room keeps the lights reaching it strongest, and a light is
/// strongest near the camera. The ranking also settles the order the lights are
/// drawn in, so it decides the cluster's indices too.
///
/// A directional light is not clustered: it reaches every fragment, so writing
/// it into every cell would spend one of each cell's slots to say nothing. It
/// goes into the frame-wide uniform instead, where at most
/// [`MAX_DIRECTIONAL_LIGHTS`] fit; the strongest are the ones kept. An unused
/// slot is left at zero, which is what makes it contribute nothing.
pub(crate) fn collect_lights(
    scope: &Scope<'_>,
    eye: Vec3,
    packed: &mut Vec<PackedLight>,
    vertices: &mut Vec<Vec4>,
    lighting: &mut LightingUniform,
) {
    packed.clear();
    vertices.clear();
    lighting.directionals = [DirectionalGpu::default(); MAX_DIRECTIONAL_LIGHTS];
    // The ambient term is a component like a light, so several of them add up
    // and a frame with none leaves the term at zero.
    lighting.ambient = Vec4::ZERO;
    for ambient in scope.query::<&AmbientLight>() {
        lighting.ambient += ambient.color;
    }
    for (source, transform) in scope.query::<(&LightSource, Option<&Transform>)>() {
        let world_from_local = transform
            .map(|transform| transform.compute_matrix())
            .unwrap_or(Affine3A::IDENTITY);
        let describe = source.0.describe(&world_from_local);
        // A light that shines nothing is dropped rather than ranked last: a
        // cell that ran out of room would otherwise spill a light that
        // contributes nothing in place of one that does. The record is read
        // first, because packing a quad light appends its corners to the pool
        // every light shares, and a record the frame discards has no corners to
        // point at.
        if describe.intensity <= 0.0 {
            continue;
        }
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
        if gpu.kind.x == GpuLightKind::Directional as u32 {
            // A light at infinity has no bounds to cluster by: every fragment
            // sees it, so it belongs to the frame rather than to a cell.
            keep_directional(&mut lighting.directionals, gpu);
            continue;
        }
        let bounds = BoundingSphere {
            center: gpu.position_radius.truncate(),
            radius: gpu.position_radius.w,
        };
        // A light near the camera lights more of the picture than one far away,
        // and a larger patch more than a smaller one, so the ranking is roughly
        // the illumination the light delivers where the frame is seen from.
        let area = match describe.kind {
            LightKind::Quad => describe.shape.map_or(1.0, |shape| shape.x.max(0.0)),
            _ => 1.0,
        };
        let distance = (bounds.center - eye).length().max(1.0);
        let importance = describe.intensity * area / (distance * distance);
        packed.push(PackedLight {
            gpu,
            bounds,
            importance,
        });
    }
}

/// Put a directional light into the frame's slots, strongest first.
///
/// The slots are ordered by intensity because the shader reads all of them and
/// cannot tell which are unused: an empty slot is one left at zero, so the
/// ordering is only what makes a frame that keeps more lights than it has room
/// for keep the strongest ones.
fn keep_directional(slots: &mut [DirectionalGpu; MAX_DIRECTIONAL_LIGHTS], gpu: LightGpu) {
    let light = DirectionalGpu {
        direction: gpu.direction_outer,
        color_intensity: gpu.color_intensity,
    };
    // An empty slot holds no colour at all, so nothing it does can change the
    // frame -- not even being displaced by a light that shines nothing. A light
    // darker than the weakest kept one is dropped from either side, keeping the
    // slot count the frame's lights, not the frame's slots.
    if light.color_intensity.w <= 0.0 {
        return;
    }
    // Every slot already filled ranks higher than the light, and every empty
    // one ranks below it, because an empty slot's intensity is zero.
    let at = slots.partition_point(|slot| slot.color_intensity.w > light.color_intensity.w);
    if at == MAX_DIRECTIONAL_LIGHTS {
        return;
    }
    for slot in (at..MAX_DIRECTIONAL_LIGHTS - 1).rev() {
        slots[slot + 1] = slots[slot];
    }
    slots[at] = light;
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
        assert_eq!(gpu.direction_outer.w, outer);
        assert!((gpu.cones.x - 1.0 / (inner - outer)).abs() < 1e-6);
        assert_eq!(gpu.position_radius.w, 4.0);
    }

    #[test]
    fn a_spot_light_whose_inner_cone_is_wider_keeps_its_hard_edge() {
        let light = SpotLight {
            color: Vec4::ONE,
            intensity: 1.0,
            range: 4.0,
            inner_angle: 1.0,
            outer_angle: 0.4,
        };
        let (gpu, _) = pack(&light, Affine3A::IDENTITY);
        // The ramp these angles describe would be brightest where the light
        // reaches nothing. The inner cone is narrowed to the outer one, so the
        // light steps at it -- the hard edge, not a silent inversion.
        assert_eq!(gpu.cones.x, 0.0);
        assert_eq!(gpu.direction_outer.w, (0.2_f32).cos());
        assert_eq!(gpu.color_intensity.w, 1.0);
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
        // Zero is the shader's `the cone is hard` mark. The record keeps its
        // colour, intensity and outer cosine, so the light still shines: only
        // the ramp between the two cones is gone.
        assert_eq!(gpu.cones.x, 0.0);
        assert_eq!(gpu.color_intensity.w, 1.0);
        assert_eq!(gpu.direction_outer.w, (0.25_f32).cos());
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
    fn a_quad_light_is_wound_in_the_world_it_lands_in() {
        let quad = QuadLight::rect(Vec4::ONE, 1.0, 8.0, 2.0, 2.0);
        // A mirror turns the same order of corners into the other side, so a
        // frame that read the winding in the light's own space would keep a
        // polygon facing away from its aim and light nothing at all.
        let mirror = Affine3A::from_scale(Vec3::new(-1.0, 1.0, 1.0));
        let (gpu, vertices) = pack(&quad, mirror);
        assert_eq!(gpu.kind.z, 4);
        let normal = (vertices[1] - vertices[0])
            .truncate()
            .cross((vertices[2] - vertices[0]).truncate());
        assert!(
            normal.normalize().dot(Vec3::NEG_Z) > 0.0,
            "the winding still faces the mirrored aim"
        );
    }

    #[test]
    fn a_scaled_quad_light_weighs_the_area_it_covers() {
        let quad = QuadLight::rect(Vec4::ONE, 1.0, 8.0, 2.0, 2.0);
        let unscaled = quad.describe(&Affine3A::IDENTITY);
        let scaled = quad.describe(&Affine3A::from_scale(Vec3::splat(2.0)));
        assert_eq!(unscaled.shape, Some(Vec2::splat(4.0)));
        assert_eq!(scaled.shape, Some(Vec2::splat(16.0)));
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
    fn a_triangle_states_itself_by_repeating_a_corner() {
        let triangle = QuadLight {
            color: Vec4::ONE,
            intensity: 1.0,
            range: 8.0,
            vertices: [
                Vec3::new(-1.0, -1.0, 0.0),
                Vec3::new(1.0, -1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
        };
        let (gpu, vertices) = pack(&triangle, Affine3A::IDENTITY);
        assert_eq!(gpu.kind.z, 3, "the repeated corner is one corner");
        assert_eq!(vertices.len(), 3);
        assert_eq!(gpu.kind.y, 0);
    }

    #[test]
    fn a_quad_light_with_every_corner_on_one_spot_has_no_area() {
        let degenerate = QuadLight {
            color: Vec4::ONE,
            intensity: 1.0,
            range: 8.0,
            vertices: [Vec3::ZERO; 4],
        };
        let (gpu, vertices) = pack(&degenerate, Affine3A::IDENTITY);
        assert_eq!(gpu.kind.z, 0);
        assert!(vertices.is_empty());
        assert!(gpu.position_radius.truncate().is_finite());
    }

    #[test]
    fn a_directional_light_that_shines_nothing_keeps_the_other_slots() {
        let mut slots = [DirectionalGpu::default(); MAX_DIRECTIONAL_LIGHTS];
        for intensity in 1..=MAX_DIRECTIONAL_LIGHTS as u32 {
            let light = DirectionalLight {
                color: Vec4::ONE,
                intensity: intensity as f32,
            };
            let (gpu, _) = pack(&light, Affine3A::IDENTITY);
            keep_directional(&mut slots, gpu);
        }
        // A spawn-and-fade cycle can leave a light at zero intensity, and a
        // slot it took would be a light the frame no longer draws with.
        let dark = DirectionalLight {
            color: Vec4::ONE,
            intensity: 0.0,
        };
        let (gpu, _) = pack(&dark, Affine3A::IDENTITY);
        keep_directional(&mut slots, gpu);
        assert_eq!(
            slots.map(|slot| slot.color_intensity.w),
            [4.0, 3.0, 2.0, 1.0],
            "the frame keeps the lights it had"
        );
        assert!(
            slots
                .iter()
                .all(|slot| slot.color_intensity.truncate() != Vec3::ZERO)
        );
    }

    #[test]
    fn a_quad_light_that_shines_nothing_leaves_no_corners() {
        use unlit_ecs::{World, bundle};

        let mut world = World::new();
        world
            .root_scope_mut()
            .spawn(bundle![LightSource::new(QuadLight::rect(
                Vec4::ONE,
                0.0,
                4.0,
                2.0,
                2.0
            ))]);
        let mut packed = Vec::new();
        let mut vertices = Vec::new();
        let mut lighting = LightingUniform {
            directionals: [DirectionalGpu::default(); MAX_DIRECTIONAL_LIGHTS],
            ambient: Vec4::ZERO,
        };
        collect_lights(
            &world.root_scope(),
            Vec3::ZERO,
            &mut packed,
            &mut vertices,
            &mut lighting,
        );
        // Packing a quad light appends its corners to the pool every light
        // shares, so a light the frame drops has to be dropped before it packs.
        assert!(packed.is_empty());
        assert!(vertices.is_empty());
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
