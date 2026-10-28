// light -- the light record and the clustering uniform.
//
// Type-declaration module only: it carries the fixed layouts that must stay in
// sync with the Rust mirrors in `crate::light`. The accessors that read a frame's
// light arrays live in `array_access.wesl`, and the lighting itself lives in
// `lit.wesl`.

/// How a light's lanes are used, read from `LightGpu::kind.x`.
///
/// These mirror `crate::light::LightKind`'s discriminants, which the Rust enum
/// documents as part of the shader interface: the record carries the kind as a
/// number so the fragment has one record shape to read, whatever the light is.
public const LIGHT_KIND_DIRECTIONAL: u32 = 0u;
public const LIGHT_KIND_POINT: u32 = 1u;
public const LIGHT_KIND_SPOT: u32 = 2u;
public const LIGHT_KIND_QUAD: u32 = 3u;

/// One light, as clustering packed it for the shader.
///
/// Every light has the same record, so a cell's light indices are all the
/// fragment needs: the kind in `kind.x` says which lanes the rest carry a
/// meaning for.
///
/// The layout must stay a valid WGSL array stride. It is five `vec4<f32>`s --
/// 80 bytes -- which is a whole number of `Rgba32Float` texels on the texel
/// path, so an element never straddles a row there.
public struct LightGpu {
    /// `xyz` = world-space position, which a directional light leaves at the
    /// origin; `w` = the radius clustering culls by, which is also the light's
    /// range.
    ///
    /// For a point, spot or quad light the radius bounds every fragment the light
    /// can reach, so a cell whose sphere it touches is given the light. A
    /// directional light reaches every cell and leaves the radius at zero.
    position_radius: vec4<f32>,
    /// `xyz` = the direction the light shines in, world space; `w` = the cosine
    /// of a spot light's outer half-angle.
    direction_outer: vec4<f32>,
    /// `rgb` = the light's linear colour; `w` = its dimensionless intensity.
    color_intensity: vec4<f32>,
    /// `x` = the cosine of a spot light's inner half-angle; `y` = the reciprocal
    /// of the gap between the inner and outer cosines, which makes the falloff a
    /// single multiply. `zw` are zero for every other kind.
    cones: vec4<f32>,
    /// `x` = the light kind, one of the `LIGHT_KIND_*` constants; `y` = the first
    /// of the light's polygon vertices in the frame's vertex pool; `z` = how many
    /// vertices it has; `w` is zero.
    kind: vec4<u32>,
}

/// The clustering state a frame's fragments read.
///
/// Mirrors `crate::light::ClusterUniform`; the layout is checked at compile time
/// against the uniform address-space rules. The grid dimensions arrive here
/// rather than as shader constants because the grid follows the target's pixel
/// size, which changes at run time.
public struct ClusterUniform {
    /// `xyz` = the grid's dimensions; `w` = the most lights one cell holds.
    dims: vec4<u32>,
    /// Perspective: `x` = `1 / ln(far / first)`, `y` = `ln(first)`, `z` = one
    /// less than the slice count, `w` = `first`. Orthographic: `x` = the
    /// reciprocal linear range, `y` = `first`, `z` = the slice count.
    depth: vec4<f32>,
    /// `rgb` = the linear colour every surface receives with no light on it;
    /// `w` is zero.
    ambient: vec4<f32>,
    /// Nonzero for a perspective projection, zero for an orthographic one.
    perspective: vec4<u32>,
}
