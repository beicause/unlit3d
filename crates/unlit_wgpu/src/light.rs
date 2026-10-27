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
//! and depth into [`ClusterConfig::z_slices`] slices that are equal in
//! `ln(depth)` rather than in depth, so a cell stays a useful size as the view
//! recedes. Both mappings are monotonic and shared by the CPU assignment and the
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
    /// The nearest depth the slices span.
    pub near: f32,
    /// The farthest depth the slices span.
    pub far: f32,
}

impl ClusterConfig {
    /// Cells of about 128 pixels, 16 depth slices, at most 4096 cells, spanning
    /// 0.1 to 1000 world units of depth.
    pub const DEFAULT: Self = Self {
        cell_pixels: 128,
        z_slices: 16,
        max_clusters: 4096,
        near: 0.1,
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
    pub fn uniform(&self, size: (u32, u32), ambient: Vec4) -> ClusterUniform {
        let dims = self.dims(size);
        let near = self.near.max(f32::MIN_POSITIVE);
        let ln_near = near.ln();
        let ln_far = self.far.max(near).ln();
        ClusterUniform {
            dims: UVec4::new(dims[0], dims[1], dims[2], LIGHTS_PER_CELL as u32),
            depth: Vec4::new(
                1.0 / (ln_far - ln_near).max(f32::MIN_POSITIVE),
                ln_near,
                dims[2] as f32,
                near,
            ),
            ambient,
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
    /// `x` = `1 / (ln(far) - ln(near))`; `y` = `ln(near)`; `z` = the slice count;
    /// `w` = `near`.
    ///
    /// A fragment's depth slice is `((ln(max(d, w)) - y) * x * z)`, floored and
    /// clamped to the last slice — the same arithmetic the CPU assignment runs
    /// over each light's depth range.
    pub depth: Vec4,
    /// `rgb` = the linear colour every surface receives with no light on it;
    /// `w` is zero.
    pub ambient: Vec4,
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

        let near = cfg.near.max(f32::MIN_POSITIVE);
        let ln_near = near.ln();
        let inv_ln_range = 1.0 / (cfg.far.max(near).ln() - ln_near).max(f32::MIN_POSITIVE);
        let last_slice = (dims[2] - 1) as f32;
        let slice_of = |depth: f32| -> u32 {
            ((depth.max(near).ln() - ln_near) * inv_ln_range * dims[2] as f32)
                .floor()
                .clamp(0.0, last_slice) as u32
        };
        let perspective = clip_from_view.row(3).truncate() != Vec3::ZERO;
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
            if depth + radius < near || depth - radius > cfg.far {
                continue;
            }
            let z0 = slice_of(depth - radius);
            let z1 = slice_of(depth + radius);
            let (lo, hi) = screen_cells(
                clip_from_view,
                perspective,
                center,
                depth,
                radius,
                dims,
                full,
            );
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
/// A sphere projects to a disc. From the eye, a sphere at depth `d` subtends the
/// half-angle whose tangent is `radius / sqrt(d² - radius²)`; a perspective
/// projection maps every such cone to a disc of the same size at every depth, so
/// the screen radius is the projection's scale times that tangent. An
/// orthographic projection has no cone, and the sphere is simply scaled.
fn screen_cells(
    clip_from_view: &Mat4,
    perspective: bool,
    center: Vec3,
    depth: f32,
    radius: f32,
    dims: [u32; 3],
    full: ((u32, u32), (u32, u32)),
) -> ((u32, u32), (u32, u32)) {
    let clip_center = *clip_from_view * center.extend(1.0);
    let w = clip_center.w;
    if !w.is_finite() || w <= 0.0 {
        return full;
    }
    let ndc_center = Vec2::new(clip_center.x / w, clip_center.y / w);
    let scale = Vec2::new(clip_from_view.x_axis.x.abs(), clip_from_view.y_axis.y.abs());
    let ndc_radius = if perspective {
        // The eye is inside the sphere, so no cone bounds what it covers: take
        // the whole screen rather than a number that is not one.
        if depth <= radius {
            return full;
        }
        scale * (radius / (depth * depth - radius * radius).sqrt())
    } else {
        scale * radius
    };
    let low = ndc_center - ndc_radius;
    let high = ndc_center + ndc_radius;
    if !low.is_finite() || !high.is_finite() {
        return full;
    }
    let cell = |ndc: f32, count: u32| -> u32 {
        let scaled = (ndc * 0.5 + 0.5) * count as f32;
        scaled.floor().clamp(0.0, (count - 1) as f32) as u32
    };
    (
        (cell(low.x, dims[0]), cell(low.y, dims[1])),
        (cell(high.x, dims[0]), cell(high.y, dims[1])),
    )
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
        assert_eq!(cfg.dims((1280, 720)), [10, 6, 16]);
        assert_eq!(cfg.dims((1, 1)), [1, 1, 16]);
        assert_eq!(cfg.dims((0, 0)), [1, 1, 16]);
    }

    #[test]
    fn dims_respect_the_cluster_budget() {
        let cfg = ClusterConfig {
            max_clusters: 64,
            ..ClusterConfig::DEFAULT
        };
        let dims = cfg.dims((7680, 4320));
        assert_eq!(dims[2], 16);
        assert!(dims[0] * dims[1] * dims[2] <= 64, "{dims:?}");
    }

    #[test]
    fn the_uniform_and_the_assignment_agree_on_the_depth_slice() {
        let cfg = ClusterConfig::DEFAULT;
        let uniform = cfg.uniform((1280, 720), Vec4::ZERO);
        let dims = cfg.dims((1280, 720));
        let near = cfg.near;
        let ln_range = (cfg.far / near).ln();
        // The shader mirror: floor((ln(max(d, w)) - y) * x * z), clamped.
        let shader_slice = |d: f32| -> u32 {
            let depth = d.clamp(near, cfg.far);
            let slice = ((depth.max(uniform.depth.w).ln() - uniform.depth.y)
                * uniform.depth.x
                * uniform.depth.z)
                .floor()
                .clamp(0.0, (dims[2] - 1) as f32);
            slice as u32
        };
        assert_eq!(shader_slice(near), 0);
        assert_eq!(shader_slice(cfg.far), dims[2] - 1);
        // Equal in log space: each slice spans the same ratio. The slice
        // boundaries are exact ratios, so a depth just past one lands in the
        // next slice; the rounding of the multiply itself may put the boundary
        // one ulp either way, which is why the probe is inside the slice.
        for slice in 1..dims[2] {
            let edge = near * (ln_range * slice as f32 / dims[2] as f32).exp();
            let inside = edge * (1.0 + 1e-4);
            assert_eq!(shader_slice(inside), slice, "slice {slice} at {inside}");
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
