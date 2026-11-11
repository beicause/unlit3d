/// The sky's shader.
///
/// The vertex stage emits a triangle that covers the whole target at the far
/// plane of the frame's reverse-z projection, so the fragment stage runs once
/// per pixel that no opaque draw has covered. It rebuilds the view ray through
/// the pixel from the uniform block's projection and rotation, paints the
/// gradient in flat bands, draws the sun as a hard-edged disc, and lays the
/// procedural cube map's clouds over it.
pub(super) const SKY_SHADER: &str = r#"
struct SkyUniforms {
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    ground: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    view_rotation: mat4x4<f32>,
    projection: vec4<f32>,
};

@group(0) @binding(0) var<uniform> sky: SkyUniforms;
@group(0) @binding(1) var sky_cube: texture_cube<f32>;
@group(0) @binding(2) var sky_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // The one triangle that covers the target, with its far corners outside it.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let corner = corners[index];
    var out: VertexOutput;
    // z is the reverse-z far plane, which the depth clear already holds, so the
    // sky passes the test wherever nothing closer has been drawn.
    out.position = vec4<f32>(corner, 0.0, 1.0);
    out.ndc = corner;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let view_direction = vec3<f32>(
        input.ndc.x * sky.projection.x,
        input.ndc.y * sky.projection.y,
        -1.0,
    );
    let direction = normalize((sky.view_rotation * vec4<f32>(view_direction, 0.0)).xyz);

    let elevation = direction.y;
    // Both halves are painted in flat bands rather than blended: a cartoon sky
    // shows the steps it was painted in. Each band is spaced by a power of the
    // elevation, because the camera looks along the horizon and would otherwise
    // see the first band of a half alone.
    //
    // The lower half keeps the same banded language as the upper one, darkening
    // in steps from the horizon colour to the ground colour. Painting it one
    // flat colour instead left a hard seam across the frame, with the saturated
    // ground meeting the pale horizon with nothing between them.
    let above = clamp(elevation, 0.0, 1.0);
    let below = clamp(-elevation, 0.0, 1.0);
    let band = floor(pow(above, 0.42) * 8.0) / 7.0;
    let lower_band = floor(pow(below, 0.42) * 8.0) / 7.0;
    let upper = mix(sky.horizon.rgb, sky.zenith.rgb, band);
    let lower = mix(sky.horizon.rgb, sky.ground.rgb, lower_band);
    let side = step(0.0, elevation);
    var colour = mix(lower, upper, side);

    // The sun is a disc with one hard edge, not a glow: past the limb there is
    // nothing added at all.
    let cosine = dot(direction, normalize(sky.sun_direction.xyz));
    colour = mix(colour, sky.sun_color.rgb, step(0.988, cosine));

    // The cube map holds flat clouds rather than a star field, and its alpha is
    // the mask: a texel outside a cloud leaves the banded sky untouched.
    //
    // A cloud thins into the haze as it reaches the horizon rather than being
    // masked off at it. Cutting them with a step at zero elevation sliced every
    // puff the horizon crossed along a dead-straight line, which read as the
    // sky being torn rather than as clouds meeting the ground.
    let haze = smoothstep(0.0, 0.08, elevation);
    let clouds = textureSample(sky_cube, sky_sampler, direction);
    colour = mix(colour, clouds.rgb, clouds.a * sky.sun_direction.w * haze);

    return vec4<f32>(colour, 1.0);
}"#;

/// The particle shader.
///
/// The mesh carries no vertex attributes at all: the six vertices of each
/// sprite come from the vertex index, and the sprite's own placement comes from
/// the instance stream. The global group is the renderer's own — the camera's
/// view and projection matrices — while the clock lives in the family's own
/// uniform block at the material group.
pub(super) const PARTICLE_SHADER: &str = r#"
struct View {
    clip_from_world: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    camera_position: vec4<f32>,
};

struct Globals {
    time: f32,
    delta_time: f32,
    frame_count: u32,
    pad0: u32,
};

struct ParticleUniforms {
    clock: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<uniform> frame: Globals;
@group(1) @binding(0) var<uniform> particles: ParticleUniforms;

struct ParticleInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(2) origin: vec4<f32>,
    @location(3) motion: vec4<f32>,
    @location(4) tint: vec4<f32>,
};

struct ParticleOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec3<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) @interpolate(flat) shape: u32,
};

@vertex
fn vs_main(input: ParticleInput) -> ParticleOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );
    let corner = corners[input.vertex_index];

    let phase = input.origin.w + particles.clock.x * input.motion.y;
    // The particle drifts around its own centre rather than spinning in place,
    // so the cloud reshapes instead of only rotating.
    let centre = vec3<f32>(
        input.origin.x + cos(phase) * input.motion.x,
        input.origin.y + sin(phase * 1.7) * input.motion.w,
        input.origin.z + sin(phase) * input.motion.x,
    );
    let size = input.motion.z * (0.85 + 0.15 * sin(phase * 3.0));

    // The camera's world-space axes are the view matrix's rows, which is what
    // keeps the sprite facing the viewer whichever way the camera is turned.
    let right = vec3<f32>(
        view.view_from_world[0][0],
        view.view_from_world[1][0],
        view.view_from_world[2][0],
    );
    let up = vec3<f32>(
        view.view_from_world[0][1],
        view.view_from_world[1][1],
        view.view_from_world[2][1],
    );
    let world = centre + right * (corner.x * size) + up * (corner.y * size);

    var out: ParticleOutput;
    out.position = view.clip_from_world * vec4<f32>(world, 1.0);
    out.corner = corner;
    // A flat fill: the sprite is one colour with no shading, and the spin is
    // the whole of its motion.
    out.colour = input.tint.rgb;
    out.shape = u32(input.tint.a + 0.5);
    return out;
}

@fragment
fn fs_main(input: ParticleOutput) -> @location(0) vec4<f32> {
    // Each sprite is one flat shape with a hard edge, drawn over the frame
    // rather than added to it, so its colour is exactly the tint it was given.
    if input.shape == 0u {
        if dot(input.corner, input.corner) > 1.0 {
            discard;
        }
    } else if input.shape == 1u {
        let point = input.corner;
        if length(point) > 1.0 || length(point - vec2<f32>(0.0, -0.62)) < 0.42 {
            discard;
        }
    } else {
        let radius = length(input.corner);
        if radius > 1.0 || radius < 0.55 {
            discard;
        }
    }
    return vec4<f32>(input.colour, 1.0);
}"#;
