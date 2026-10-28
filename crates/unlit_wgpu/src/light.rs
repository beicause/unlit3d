//! The frame's lights, packed for the shaders and clustered on the CPU.
//!
//! A forward renderer shades a fragment from the lights that can reach it, and
//! forward clustered rendering answers "which lights reach this fragment" by
//! cutting the view into a grid of cells — tiles in screen space, logarithmic
//! slices in depth — and giving every cell the indices of the lights that touch
//! it. A fragment then reads exactly one cell.
//!
//! The clustering here is plain CPU work over the packed records, producing the
//! `u32` cell array and the [`LightGpu`] array a draw binds. It needs no compute
//! shader, which is what lets the same code serve WebGL2: a device with no
//! compute passes emits the very same bytes. The arrays are uploaded like every
//! other frame-wide array — a storage buffer where the device has those, texels
//! where it does not.
//!
//! # The grid
//!
//! Screen space is cut into cells of about [`ClusterConfig::cell_pixels`] pixels,
//! and depth into [`ClusterConfig::z_slices`] slices. Perspective slices are
//! equal in `ln(depth)` after the first slice's fixed boundary; orthographic
//! ones are linear. Both mappings are monotonic and shared by the CPU assignment and the
//! fragment lookup; that shared mapping is the one correctness requirement,
//! because it is what keeps a light and the fragments it lights in one cell.
//!
//! # The cell record
//!
//! A cell is [`CELL_WORDS`] `u32`s: the count of lights in it, then their
//! indices, then padding to a whole texel. The indices point into the frame's
//! [`LightGpu`] array, so a cell costs no light data of its own.
//!
//! Lights are ranked by [`PackedLight::importance`] before assignment, so a cell
//! that overflows drops its weakest light and the first entries of every cell
//! are its strongest.

use glam::{Mat4, UVec4, Vec2, Vec3, Vec4};

/// How many `u32`s one cell occupies: its count, its light indices, and the
/// padding that rounds the record to a whole `Rgba32Float` texel.
pub const CELL_WORDS: usize = 16;

/// How many lights one cell holds: the record minus its count word.
pub const LIGHTS_PER_CELL: usize = CELL_WORDS - 1;

/// The vertices a quad light keeps after the horizon clip: four corners plus at
/// most one per clipped edge.
pub const MAX_POLYGON_VERTICES: usize = 5;

/// One light as the shaders read it.
///
/// Mirrors `light.wesl::LightGpu`; the layout is checked at compile time against
/// the WGSL storage-buffer alignment rules.
///
/// Every kind of light shares the record, so one array holds the frame's
/// directional, point, spot and quad lights and a cell needs no more than an
/// index. Which lanes matter is decided by [`Self::kind`], and the lanes a kind
/// does not use are zero.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
    const_shader_layout::ShaderLayout,
)]
pub struct LightGpu {
    /// `xyz` = world-space position, which a directional light leaves at the
    /// origin; `w` = the radius clustering culls by, which is also the light's
    /// range.
    ///
    /// For a point, spot or quad light the radius bounds every fragment the light
    /// can reach, so a cell whose sphere it touches is given the light. A
    /// directional light reaches every cell and leaves the radius at zero.
    pub position_radius: Vec4,
    /// `xyz` = the direction the light shines in, world space; `w` = the cosine
    /// of a spot light's outer half-angle.
    ///
    /// Every light shines along the `-Z` of its own transform, so this is that
    /// transform applied to the axis.
    pub direction_outer: Vec4,
    /// `rgb` = the light's linear colour; `w` = its dimensionless intensity.
    ///
    /// Intensity carries no unit: the shader modulates a surface's base colour by
    /// colour times intensity and normalises nothing, so a caller picks numbers
    /// that look right rather than physical values.
    pub color_intensity: Vec4,
    /// `x` = the cosine of a spot light's inner half-angle; `y` = the reciprocal
    /// of the gap between the inner and outer cosines, which makes the falloff a
    /// single multiply. `zw` are zero for every other kind.
    pub cones: Vec4,
    /// `x` = [`LightKind`] as `u32`; `y` = the first of the light's polygon
    /// vertices in the frame's vertex pool; `z` = how many vertices it has;
    /// `w` is zero.
    ///
    /// The polygon vertices live in one frame-wide `vec4` array rather than in
    /// the record, because how many a light owns varies while every record is the
    /// same size.
    pub kind: UVec4,
}

/// Which of the light record's lanes a light uses.
///
/// The discriminants are what the shader compares [`LightGpu::kind`]`x` against,
/// so they are part of the shader interface rather than an implementation
/// detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum LightKind {
    /// A light at infinity: one direction, no falloff, no bounds.
    Directional = 0,
    /// A light at a point, fading with distance inside its range.
    Point = 1,
    /// A point light narrowed to a cone.
    Spot = 2,
    /// A convex planar polygon radiating from one side of its plane.
    Quad = 3,
}

/// The sphere clustering culls a light by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundingSphere {
    /// World-space center.
    pub center: Vec3,
    /// How far from the center the light can still be seen.
    pub radius: f32,
}

/// A light packed for the shader, with the facts clustering needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PackedLight {
    /// The record the shader reads.
    pub gpu: LightGpu,
    /// The world-space sphere every fragment the light reaches lies in;
    /// `None` for a light with no bounds, which reaches every cell.
    pub bounds: Option<BoundingSphere>,
    /// What a full cell ranks by: higher survives.
    pub importance: f32,
}

/// How a frame's view is divided into cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterConfig {
    /// The target pixels one cell covers along x and y.
    pub cell_pixels: u32,
    /// How many slices the view's depth is cut into.
    pub z_slices: u32,
    /// The most cells the grid may hold, whatever the target size.
    ///
    /// A finer grid costs only memory and CPU time — a fragment reads one cell
    /// however fine the grid is — but the whole grid is uploaded every frame, so
    /// it is bounded.
    pub max_clusters: u32,
    /// The end of the first depth slice.
    ///
    /// This is a scene-scale choice, not the camera's near plane. The first
    /// slice absorbs everything nearer; the remaining slices divide the rest
    /// of the range logarithmically under a perspective projection.
    pub first_slice_depth: f32,
    /// The farthest depth the slices span.
    pub far: f32,
}

impl ClusterConfig {
    /// Cells of about 128 pixels, 8 depth slices, at most 4096 cells. The first
    /// slice ends at 5 world units and the range ends at 1000.
    ///
    /// Both ends are fixed so the grid does not depend on which lights happen
    /// to be present: a light-only range cannot match the visible surfaces it
    /// did not measure. A light nearer than the first slice's end lands in that
    /// slice, which is where a fragment that near looks it up too.
    pub const DEFAULT: Self = Self {
        cell_pixels: 128,
        z_slices: 8,
        max_clusters: 4096,
        first_slice_depth: 5.0,
        far: 1000.0,
    };

    /// The grid this configuration gives a `size`-pixel target.
    ///
    /// The x and y counts come from the target size, then shrink together until
    /// the grid fits [`Self::max_clusters`], so the cells stay roughly square and
    /// the grid follows the target's aspect. Depth keeps its slice count, which
    /// is what a caller sets for the depth range it cares about.
    pub fn dims(&self, size: (u32, u32)) -> [u32; 3] {
        let cell_pixels = self.cell_pixels.max(1);
        let max_clusters = self.max_clusters.max(1);
        let z = self.z_slices.max(1).min(max_clusters);
        let budget = (max_clusters / z).max(1);
        let mut x = size.0.div_ceil(cell_pixels).max(1);
        let mut y = size.1.div_ceil(cell_pixels).max(1);
        while u64::from(x) * u64::from(y) > u64::from(budget) {
            if x >= y && x > 1 {
                x -= 1;
            } else if y > 1 {
                y -= 1;
            } else {
                break;
            }
        }
        [x, y, z]
    }

    /// The uniform the frame's shaders read for a `size`-pixel target.
    pub fn uniform(&self, size: (u32, u32), ambient: Vec4, perspective: bool) -> ClusterUniform {
        let dims = self.dims(size);
        let first = self.first_slice_depth.max(f32::MIN_POSITIVE);
        let far = self.far.max(first);
        let depth = if perspective {
            let ln_first = first.ln();
            Vec4::new(
                1.0 / (far.ln() - ln_first).max(f32::MIN_POSITIVE),
                ln_first,
                (dims[2] - 1) as f32,
                first,
            )
        } else {
            Vec4::new(
                1.0 / (far - first).max(f32::MIN_POSITIVE),
                first,
                dims[2] as f32,
                first,
            )
        };
        ClusterUniform {
            dims: UVec4::new(dims[0], dims[1], dims[2], LIGHTS_PER_CELL as u32),
            depth,
            ambient,
            perspective: UVec4::new(u32::from(perspective), 0, 0, 0),
        }
    }
}

/// The clustering state a frame's shaders read.
///
/// Mirrors `light.wesl::ClusterUniform`; the layout is checked at compile time
/// against the WGSL uniform address-space rules.
///
/// The grid dimensions arrive here rather than as shader constants because the
/// grid follows the target's pixel size, which changes at run time.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
    const_shader_layout::ShaderLayoutCompat,
)]
pub struct ClusterUniform {
    /// `xyz` = the grid's dimensions; `w` = [`LIGHTS_PER_CELL`], the most lights
    /// one cell holds.
    pub dims: UVec4,
    /// Perspective: `x` = `1 / ln(far / first)`, `y` = `ln(first)`, `z` = one
    /// less than the slice count, `w` = `first`. Orthographic: `x` = the
    /// reciprocal linear range, `y` = `first`, `z` = the slice count.
    pub depth: Vec4,
    /// `rgb` = the linear colour every surface receives with no light on it;
    /// `w` is zero.
    pub ambient: Vec4,
    /// `x` is nonzero for a perspective projection, zero for an orthographic
    /// one. The remaining lanes reserve the uniform's 16-byte tail padding.
    pub perspective: UVec4,
}

/// One frame's clustered light assignment, in storage the caller reuses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LightClusters {
    /// The cell records, `dims.x * dims.y * dims.z * CELL_WORDS` long.
    pub cells: Vec<u32>,
    /// The grid the records were written for.
    pub dims: [u32; 3],
}

impl LightClusters {
    /// Assign `lights` to the cells of `cfg`'s grid for this frame's camera.
    ///
    /// `clip_from_view` and `view_from_world` are the camera's two transforms:
    /// depth and the screen extent come from them, so both are needed. `size` is
    /// the target's pixel size.
    ///
    /// `lights` is ranked by importance in place, so the caller passes them in
    /// any order and the weakest are what a full cell drops.
    pub fn update(
        &mut self,
        cfg: &ClusterConfig,
        clip_from_view: &Mat4,
        view_from_world: &Mat4,
        size: (u32, u32),
        lights: &mut [PackedLight],
    ) {
        let dims = cfg.dims(size);
        let cells = dims[0] as usize * dims[1] as usize * dims[2] as usize;
        self.dims = dims;
        self.cells.clear();
        self.cells.resize(cells * CELL_WORDS, 0);

        // Strongest first, so a full cell keeps what matters; the stable sort
        // leaves the caller's order among equals, which keeps a snapshot
        // deterministic when two lights rank the same.
        lights.sort_by(|a, b| b.importance.total_cmp(&a.importance));

        let perspective = clip_from_view.row(3).truncate() != Vec3::ZERO;
        let uniform = cfg.uniform(size, Vec4::ZERO, perspective);
        let last_slice = (dims[2] - 1) as f32;
        let slice_of = |depth: f32| -> u32 { depth_slice(depth, &uniform).min(last_slice as u32) };
        let full = ((0, 0), (dims[0] - 1, dims[1] - 1));

        for (index, light) in lights.iter().enumerate() {
            let index = index as u32;
            let Some(bounds) = light.bounds else {
                for cell in 0..cells {
                    insert(&mut self.cells, cell, index);
                }
                continue;
            };
            let center = view_from_world.transform_point3(bounds.center);
            let depth = -center.z;
            let radius = bounds.radius.max(0.0);
            // A fragment beyond either end of the configured range still reads
            // the nearest slice. Culling a light at that same end would leave
            // the fragment looking in a cell the light was never assigned to.
            if depth + radius <= 0.0 {
                continue;
            }
            let z0 = slice_of(depth - radius);
            let z1 = slice_of(depth + radius);
            let Some((lo, hi)) = screen_cells(
                clip_from_view,
                perspective,
                center,
                depth,
                radius,
                dims,
                full,
            ) else {
                continue;
            };
            for z in z0..=z1 {
                for y in lo.1..=hi.1 {
                    let row = (z * dims[1] + y) * dims[0];
                    for x in lo.0..=hi.0 {
                        insert(&mut self.cells, (row + x) as usize, index);
                    }
                }
            }
        }
    }
}

/// The depth slice containing `depth`, mirrored by `cluster_depth_slice` in
/// the built-in shader.
///
/// Slice zero absorbs everything before the first boundary. Under a perspective
/// projection the remaining slices are equal in logarithmic depth; under an
/// orthographic one they are equal in linear depth.
pub fn depth_slice(depth: f32, cluster: &ClusterUniform) -> u32 {
    let last = cluster.dims.z - 1;
    if depth < cluster.depth.w {
        return 0;
    }
    let coordinate = if cluster.perspective.x != 0 {
        (depth.ln() - cluster.depth.y) * cluster.depth.x * cluster.depth.z + 1.0
    } else {
        (depth - cluster.depth.y) * cluster.depth.x * cluster.depth.z
    };
    coordinate.floor().clamp(0.0, last as f32) as u32
}

/// Append `index` to the cell at `cell`, dropping it when the cell is full.
fn insert(cells: &mut [u32], cell: usize, index: u32) {
    let record = &mut cells[cell * CELL_WORDS..(cell + 1) * CELL_WORDS];
    let count = record[0] as usize;
    if count < LIGHTS_PER_CELL {
        record[count + 1] = index;
        record[0] += 1;
    }
}

/// The x and y cells a light's sphere covers, as inclusive ranges.
///
/// A perspective sphere is bounded by its two tangent lines on each screen
/// axis. Those tangents are asymmetric around the projected center for an
/// off-axis sphere, so a radius around that center can miss its outer edge.
/// An orthographic projection has no such asymmetry, and is simply scaled.
fn screen_cells(
    clip_from_view: &Mat4,
    perspective: bool,
    center: Vec3,
    depth: f32,
    radius: f32,
    dims: [u32; 3],
    full: ((u32, u32), (u32, u32)),
) -> Option<((u32, u32), (u32, u32))> {
    let clip_center = *clip_from_view * center.extend(1.0);
    let w = clip_center.w;
    if !w.is_finite() || w <= 0.0 {
        return Some(full);
    }
    let ndc_center = Vec2::new(clip_center.x / w, clip_center.y / w);
    let scale = Vec2::new(clip_from_view.x_axis.x.abs(), clip_from_view.y_axis.y.abs());
    let (low, high) = if perspective {
        // The eye is inside the sphere, so no tangents bound what it covers.
        if depth <= radius {
            return Some(full);
        }
        // The two tangents from the eye to the sphere are the extreme
        // directions on each screen axis: `(axis * depth -+ radius * s) /
        // (depth^2 - radius^2)`, where `s` is the tangent length. The pair is
        // already ordered, because the radius term is non-negative, so it does
        // not depend on which side of the axis the center is on -- a sign taken
        // from the axis would swap the ends for every light left of or below
        // the camera and leave its cells empty.
        let depth_squared = depth * depth - radius * radius;
        let tangent = |axis: f32, projection_scale: f32| {
            let radial_part = (axis * axis + depth_squared).sqrt() * radius;
            let center_part = axis * depth;
            Vec2::new(center_part - radial_part, center_part + radial_part)
                * (projection_scale / depth_squared)
        };
        let x = tangent(center.x, scale.x);
        let y = tangent(center.y, scale.y);
        (Vec2::new(x.x, y.x), Vec2::new(x.y, y.y))
    } else {
        let ndc_radius = scale * radius;
        (ndc_center - ndc_radius, ndc_center + ndc_radius)
    };
    if !low.is_finite() || !high.is_finite() {
        return Some(full);
    }
    if high.x < -1.0 || low.x > 1.0 || high.y < -1.0 || low.y > 1.0 {
        return None;
    }
    let cell = |ndc: f32, count: u32| -> u32 {
        let scaled = (ndc * 0.5 + 0.5) * count as f32;
        scaled.floor().clamp(0.0, (count - 1) as f32) as u32
    };
    Some((
        (cell(low.x, dims[0]), cell(low.y, dims[1])),
        (cell(high.x, dims[0]), cell(high.y, dims[1])),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(position: Vec3, radius: f32, importance: f32) -> PackedLight {
        let gpu = LightGpu {
            position_radius: position.extend(radius),
            direction_outer: Vec4::ZERO,
            color_intensity: Vec4::ONE,
            cones: Vec4::ZERO,
            kind: UVec4::new(LightKind::Point as u32, 0, 0, 0),
        };
        PackedLight {
            gpu,
            bounds: Some(BoundingSphere {
                center: position,
                radius,
            }),
            importance,
        }
    }

    fn view(distance: f32) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z * distance, Vec3::Y)
    }

    #[test]
    fn dims_follow_the_target_size() {
        let cfg = ClusterConfig::DEFAULT;
        assert_eq!(cfg.dims((1280, 720)), [10, 6, 8]);
        assert_eq!(cfg.dims((1, 1)), [1, 1, 8]);
        assert_eq!(cfg.dims((0, 0)), [1, 1, 8]);
    }

    #[test]
    fn dims_respect_the_cluster_budget() {
        let cfg = ClusterConfig {
            max_clusters: 64,
            ..ClusterConfig::DEFAULT
        };
        let dims = cfg.dims((7680, 4320));
        assert_eq!(dims[2], 8);
        assert!(dims[0] * dims[1] * dims[2] <= 64, "{dims:?}");
    }

    #[test]
    fn the_uniform_and_the_assignment_agree_on_the_depth_slice() {
        let cfg = ClusterConfig::DEFAULT;
        let dims = cfg.dims((1280, 720));
        let uniform = cfg.uniform((1280, 720), Vec4::ZERO, true);
        let first = cfg.first_slice_depth;
        assert_eq!(depth_slice(0.0, &uniform), 0);
        assert_eq!(depth_slice(first * 0.5, &uniform), 0);
        assert_eq!(depth_slice(first, &uniform), 1);
        assert_eq!(depth_slice(cfg.far, &uniform), dims[2] - 1);
        // After the absorbing first slice, the remaining seven are equal in
        // log space. Probe just inside each boundary so one ulp in the multiply
        // cannot decide which side of an exact edge the assertion samples.
        for slice in 2..dims[2] {
            let fraction = (slice - 1) as f32 / (dims[2] - 1) as f32;
            let edge = first * (cfg.far / first).ln().mul_add(fraction, 0.0).exp();
            assert_eq!(depth_slice(edge * (1.0 + 1e-4), &uniform), slice);
        }

        let orthographic = cfg.uniform((1280, 720), Vec4::ZERO, false);
        for slice in 1..dims[2] {
            let edge = first + (cfg.far - first) * slice as f32 / dims[2] as f32;
            assert_eq!(depth_slice(edge + 1e-3, &orthographic), slice);
        }
    }

    #[test]
    fn a_point_light_lands_only_in_the_cells_it_reaches() {
        let cfg = ClusterConfig::DEFAULT;
        let size = (1280, 720);
        let dims = cfg.dims(size);
        let mut clusters = LightClusters::default();
        let mut lights = [point(Vec3::new(0.0, 0.0, -20.0), 1.0, 1.0)];
        clusters.update(
            &cfg,
            &glam::camera::rh::proj::directx::perspective(1.0, 16.0 / 9.0, 0.1, 1000.0),
            &view(20.0),
            size,
            &mut lights,
        );

        let total = dims[0] as usize * dims[1] as usize * dims[2] as usize;
        let mut filled = 0;
        for cell in 0..total {
            let record = &clusters.cells[cell * CELL_WORDS..(cell + 1) * CELL_WORDS];
            assert!(record[0] <= LIGHTS_PER_CELL as u32);
            if record[0] == 1 {
                assert_eq!(record[1], 0);
                filled += 1;
            } else {
                assert_eq!(record[0], 0);
            }
        }
        // A one-unit sphere at the center of the screen is a small patch, not
        // the whole grid.
        assert!(filled > 0, "the light was culled from every cell");
        assert!(filled < total / 4, "{filled} of {total} cells");
        assert_eq!(clusters.dims, dims);
    }

    #[test]
    fn a_near_light_reaches_the_first_slice_and_an_offscreen_light_reaches_none() {
        let cfg = ClusterConfig::DEFAULT;
        let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 1000.0);
        let mut near = [point(Vec3::new(0.0, 0.0, -0.25), 0.1, 1.0)];
        let mut clusters = LightClusters::default();
        clusters.update(&cfg, &projection, &Mat4::IDENTITY, (128, 128), &mut near);
        assert!(
            clusters
                .cells
                .as_chunks::<CELL_WORDS>()
                .0
                .iter()
                .any(|cell| cell[0] == 1)
        );

        let mut offscreen = [point(Vec3::new(100.0, 0.0, -20.0), 1.0, 1.0)];
        clusters.update(
            &cfg,
            &projection,
            &Mat4::IDENTITY,
            (128, 128),
            &mut offscreen,
        );
        assert!(
            clusters
                .cells
                .as_chunks::<CELL_WORDS>()
                .0
                .iter()
                .all(|cell| cell[0] == 0)
        );
    }

    #[test]
    fn a_centered_sphere_keeps_its_circular_projection() {
        let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 1000.0);
        let (lo, hi) = screen_cells(
            &projection,
            true,
            Vec3::new(0.0, 0.0, -10.0),
            10.0,
            1.0,
            [8, 8, 8],
            ((0, 0), (7, 7)),
        )
        .expect("the sphere overlaps the screen");
        // The exact tangents are NDC +/-1/sqrt(100-1), just inside cells 3/4.
        assert_eq!((lo.0, hi.0), (3, 4));
        assert_eq!((lo.1, hi.1), (3, 4));
    }

    #[test]
    fn a_sphere_left_of_the_axis_keeps_its_cells() {
        let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 1000.0);
        let dims = [8, 8, 8];
        let full = ((0, 0), (7, 7));
        // The mirror of the off-axis case: the tangents stay ordered, so the
        // cells are the mirrored pair rather than an empty range.
        let (lo, hi) = screen_cells(
            &projection,
            true,
            Vec3::new(-5.0, 0.0, -10.0),
            10.0,
            1.0,
            dims,
            full,
        )
        .expect("the tangent bounds overlap the screen");
        assert_eq!((lo.0, hi.0), (0, 1));
        assert!(lo.1 <= hi.1);
    }

    #[test]
    fn an_off_axis_sphere_keeps_its_outer_tangent_cell() {
        let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 1000.0);
        let dims = [8, 8, 8];
        let full = ((0, 0), (7, 7));
        let center = Vec3::new(5.0, 0.0, -10.0);
        let (lo, hi) = screen_cells(&projection, true, center, 10.0, 1.0, dims, full)
            .expect("the tangent bounds overlap the screen");
        // The exact tangents reach NDC x = 0.3900 and 0.6150; the old disc
        // around the center's NDC x = 0.5 ended at 0.6005 and missed cell 7.
        assert_eq!(lo.0, 6);
        assert_eq!(hi.0, 7);
        assert!(hi.1 >= lo.1);
    }

    #[test]
    fn a_directional_light_lands_in_every_cell() {
        let cfg = ClusterConfig {
            max_clusters: 32,
            ..ClusterConfig::DEFAULT
        };
        let size = (128, 128);
        let dims = cfg.dims(size);
        let mut clusters = LightClusters::default();
        let mut lights = [PackedLight {
            gpu: LightGpu {
                position_radius: Vec4::ZERO,
                direction_outer: Vec3::NEG_Z.extend(0.0),
                color_intensity: Vec4::ONE,
                cones: Vec4::ZERO,
                kind: UVec4::new(LightKind::Directional as u32, 0, 0, 0),
            },
            bounds: None,
            importance: 1.0,
        }];
        clusters.update(
            &cfg,
            &glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 1000.0),
            &view(10.0),
            size,
            &mut lights,
        );
        let total = dims[0] as usize * dims[1] as usize * dims[2] as usize;
        assert_eq!(clusters.cells.len(), total * CELL_WORDS);
        for cell in 0..total {
            assert_eq!(clusters.cells[cell * CELL_WORDS], 1, "cell {cell}");
        }
    }

    #[test]
    fn a_full_cell_keeps_the_stronger_lights() {
        let mut cells = vec![0u32; CELL_WORDS];
        for index in 0..LIGHTS_PER_CELL as u32 + 2 {
            insert(&mut cells, 0, index);
        }
        // The record is the count word followed by the indices, nothing else:
        // the lights past the cap are the ones the cell had no room for.
        assert_eq!(cells[0], LIGHTS_PER_CELL as u32);
        assert_eq!(
            &cells[1..=LIGHTS_PER_CELL],
            &(0..LIGHTS_PER_CELL as u32).collect::<Vec<_>>()[..]
        );
    }
}
