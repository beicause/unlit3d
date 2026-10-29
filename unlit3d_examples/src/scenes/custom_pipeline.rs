//! A caller-owned pipeline: a procedural sky and an animated particle cloud.
//!
//! Every other scene in the example draws through the built-in lit family.
//! This one registers two families of its own — pipelines, vertex layouts and
//! bind groups the example owns — to pin the parts of the renderer a caller
//! reaches without going through the built-in shader at all:
//!
//! * a **sky** family whose shader reconstructs a view ray from the frame's
//!   camera, samples a procedurally generated cube map, and blends a gradient
//!   and a sun from a uniform block the example writes every frame. Its only
//!   bind group is the caller's own, declared and rebuilt through the same
//!   rebuild recipe a built-in family uses;
//! * a **particle** family whose per-instance stream comes from an ordinary
//!   world component, and whose vertex stage animates the sprite from a clock
//!   the example advances. It keeps the renderer's shared global group for the
//!   camera and adds its own uniform block at the material group.
//!
//! Neither family declares a single mesh vertex attribute: the sky's three
//! vertices and each particle's six come from the vertex index, so the meshes
//! carry nothing but a count and a bounding box. The animation is parametric:
//! the camera orbits the cloud while the particles drift and spin from one
//! clock, so the window drives it from the frame delta instead of holding each
//! frozen frame, and its snapshots freeze eight instants of that orbit. Its
//! look is deliberately flat: banded colours, hard-edged sprites and a
//! hard-edged sun rather than any attempt at a photographic sky.

use std::sync::Arc;
use unlit_ecs::{Bundle, DynamicBundle, bundle};

use super::{
    SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_FORMAT, TEST_SIZE, camera_looking_at,
};
use unlit_wgpu::pipeline::{GlobalBindings, global_bind_group_layout, supports_storage_buffers};
use unlit_wgpu::resources::{ResHandle, Resource, TextureExt, TextureView};
use unlit_wgpu::specialize::{
    PipelineVariant, RenderPipelineDesc, SpecializedPipeline, SurfaceKey, SurfaceTarget,
};
use unlit3d::pipeline::DrawContext;
use unlit3d::prelude::*;
use zerocopy::IntoBytes;

/// How many frames the scene freezes.
const FRAMES: u32 = 8;

/// The format of the procedural cube map.
const SKY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The edge of one cube face of the procedural sky, in texels.
///
/// The face is written straight into the texture, so the row pitch has to
/// satisfy the copy alignment; the pitch is computed from this and the format
/// rather than written out.
const SKY_FACE_SIZE: u32 = 256;

/// The vertices of the sky's full-target triangle.
const SKY_VERTICES: u32 = 3;

/// How far the sky's bounding box reaches, in metres.
///
/// The triangle is authored in clip space and ignores the transform entirely,
/// so the box exists only to keep the frustum cull from dropping the draw.
const SKY_EXTENT: f32 = 1.0e4;

/// The vertices of one particle's billboard quad.
const PARTICLE_VERTICES: u32 = 6;

/// How many particles the cloud holds.
const PARTICLE_COUNT: usize = 22;

/// The vertex-buffer slot the particle family's instance stream takes.
const PARTICLE_INSTANCE_SLOT: u32 = 1;

/// The radius the camera orbits at, in metres.
const ORBIT_RADIUS: f32 = 4.4;

/// The height the camera orbits at, in metres.
const ORBIT_HEIGHT: f32 = 2.1;

/// The angle the orbit starts at, in radians.
const ORBIT_START: f32 = 0.30;

/// Radians the orbit turns between two frames of the frozen sequence.
///
/// Eight of them sweep a little over half a turn, so the sun crosses the frame
/// and the sky turns under it.
const ORBIT_STEP: f32 = 0.42;

/// Radians the window turns the orbit per second.
///
/// The frozen sweep holds one step for a `SEQUENCE_STEP`, so a continuous run
/// turning at this rate shows the same orbit at the pace its snapshots imply.
const ORBIT_RADIANS_PER_SECOND: f32 = ORBIT_STEP / SEQUENCE_STEP;

/// The point the camera looks at.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 1.05, 0.0);

/// The direction the sun sits in, in world space.
const SUN_DIRECTION: glam::Vec3 = glam::Vec3::new(-0.45, 0.30, -0.84);

/// The hues the particles cycle through.
///
/// Flat and saturated rather than lit: a sprite's whole colour is this one,
/// with no shading and no falloff.
const PARTICLE_TINTS: [glam::Vec4; 5] = [
    glam::Vec4::new(1.00, 0.35, 0.62, 1.0),
    glam::Vec4::new(0.25, 0.85, 1.00, 1.0),
    glam::Vec4::new(1.00, 0.86, 0.25, 1.0),
    glam::Vec4::new(0.35, 1.00, 0.62, 1.0),
    glam::Vec4::new(0.70, 0.50, 1.00, 1.0),
];

/// The scene: a procedural sky and an orbiting particle cloud, both drawn by
/// families the example owns and animated in real time from the frame delta.
pub static SCENE: SceneDef = SceneDef {
    id: "custom_pipeline",
    title: "Custom pipeline: sky and particles",
    description: "a caller-registered sky family and particle family",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: None,
    frames: FRAMES,
    // The motion is parametric, so the window runs it from the frame delta
    // rather than holding each frozen frame for a step; the capture still walks
    // the sequence its snapshots store.
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// The sky family's uniform block, as the shader reads it.
///
/// The example writes this every frame: the three band colours, the sun's
/// direction and colour, the view-to-world rotation the ray is built through,
/// and the projection's half-angle tangents. The last two are the reason the
/// sky needs no camera of its own — it derives the ray from the frame's camera
/// rather than from a hard-coded field of view.
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
struct SkyUniforms {
    /// The colour overhead.
    zenith: glam::Vec4,
    /// The colour at the horizon.
    horizon: glam::Vec4,
    /// The colour below the horizon.
    ground: glam::Vec4,
    /// The sun's world-space direction, with the cloud opacity in w.
    sun_direction: glam::Vec4,
    /// The sun's colour.
    sun_color: glam::Vec4,
    /// The rotation the view ray is taken into world space through.
    view_rotation: glam::Mat4,
    /// The projection's half-angle tangents in x and y.
    projection: glam::Vec4,
}

/// The particle family's uniform block, as the shader reads it.
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
struct ParticleUniforms {
    /// The scene clock in x, in seconds.
    clock: glam::Vec4,
}

/// One particle's record on the instance stream.
///
/// The layout is the example's own: three vec4s where the built-in family's
/// record has a transform, a colour and a handful of indices.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
)]
struct ParticleRecord {
    /// The orbit's centre in xyz, with the phase offset in w.
    origin: [f32; 4],
    /// The orbit's radius in x, its speed in y, the sprite's size in z and the
    /// vertical bob's amplitude in w.
    motion: [f32; 4],
    /// The sprite's colour in rgb and its shape in a.
    tint: [f32; 4],
}

/// The per-particle data the family reads, as a plain world component.
#[derive(Clone, Copy, Debug)]
struct Particle {
    /// The centre the particle orbits.
    origin: glam::Vec3,
    /// The offset into the orbit's cycle.
    phase: f32,
    /// The radius of the orbit.
    radius: f32,
    /// The angular speed of the orbit, in radians per second.
    speed: f32,
    /// The sprite's half-size, in metres.
    size: f32,
    /// How far the particle bobs vertically.
    bob: f32,
    /// Which flat shape the sprite is: a disc, a star or a ring.
    shape: f32,
    /// The sprite's colour.
    tint: glam::Vec4,
}

/// The scene's sky: the mesh that covers the target and the pipeline that
/// shades it.
///
/// The pipeline is compiled for the mesh's vertex layout, so the two are
/// spawned as one object rather than as two independent components.
struct Sky {
    mesh: GpuMesh,
    pipeline: GpuRenderPipeline,
}

impl Bundle for Sky {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.mesh, self.pipeline]
    }
}

/// One particle of the cloud.
///
/// The mesh, the pipeline and the material are shared by the whole cloud — one
/// of each, so the renderer folds every sprite into a single instanced draw —
/// while the record is what places this one; a spawn states all four together.
struct Sprite {
    mesh: GpuMesh,
    pipeline: GpuRenderPipeline,
    material: GpuMaterial,
    particle: Particle,
}

impl Bundle for Sprite {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.mesh, self.pipeline, self.material, self.particle]
    }
}

/// The sky's shader.
///
/// The vertex stage emits a triangle that covers the whole target at the far
/// plane of the frame's reverse-z projection, so the fragment stage runs once
/// per pixel that no opaque draw has covered. It rebuilds the view ray through
/// the pixel from the uniform block's projection and rotation, paints the
/// gradient in flat bands, draws the sun as a hard-edged disc, and lays the
/// procedural cube map's clouds over it.
const SKY_SHADER: &str = r#"
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
const PARTICLE_SHADER: &str = r#"
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

/// The identity of the sky family's pipeline, keyed on the compiled descriptor.
///
/// The descriptor is immutable once built, so two keys that share it are the
/// same pipeline; the key compares the allocation rather than the contents.
#[derive(Clone)]
struct SkyKey {
    descriptor: Arc<RenderPipelineDesc>,
}

impl PartialEq for SkyKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for SkyKey {}

impl core::hash::Hash for SkyKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

/// The sky family's blueprint, specialised for one render target.
#[derive(Clone, Debug)]
struct SkyVariant {
    /// The pipeline as built, before the target is applied.
    descriptor: Arc<RenderPipelineDesc>,
    /// The target the pipeline is compiled for.
    surface: SurfaceKey,
}

impl PartialEq for SkyVariant {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface && Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for SkyVariant {}

impl core::hash::Hash for SkyVariant {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.surface.hash(state);
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

impl RenderPipelineKey for SkyKey {
    type Variant = SkyVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> SkyVariant {
        SkyVariant {
            descriptor: self.descriptor.clone(),
            surface: draw.surface,
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for SkyVariant {
    type Descriptor = RenderPipelineDesc;

    fn descriptor(&self, _device: &wgpu::Device) -> RenderPipelineDesc {
        // The stored descriptor carries the format the example draws its own
        // captures with; rewriting the target here is what lets the same family
        // draw into a window's swap chain, whose colour format differs.
        let mut descriptor = self.descriptor.as_ref().clone();
        descriptor.set_surface(self.surface);
        descriptor
    }
}

/// The sky family's factory.
///
/// The family owns its whole group 0, so the factory's job is to hand the
/// renderer the recipe that builds it — the same rebuild the built-in lit
/// family returns, over a group whose entries the example chose.
struct SkyFactory {
    /// The device the group is created on.
    device: wgpu::Device,
    /// The layout the group must match.
    layout: wgpu::BindGroupLayout,
    /// The uniform block the group binds.
    uniforms: ResHandle<wgpu::Buffer>,
    /// The cube map's view.
    view: ResHandle<TextureView>,
    /// The cube map's sampler.
    sampler: ResHandle<wgpu::Sampler>,
}

impl RenderPipelineFactory<RenderPipelineDesc> for SkyFactory {
    fn descriptor(
        &self,
        _context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, RenderPipelineDesc>,
    ) -> RegisteredRenderPipeline {
        let device = self.device.clone();
        let layout = self.layout.clone();
        let uniforms = self.uniforms.clone();
        let view = self.view.clone();
        let sampler = self.sampler.clone();
        let rebuild = Rebuild::new(move |graph| {
            let uniforms = graph
                .get(&uniforms)
                .expect("the sky's uniform buffer is in the graph");
            let view = graph
                .get(&view)
                .expect("the sky's cube view is in the graph");
            let sampler = graph
                .get(&sampler)
                .expect("the sky's sampler is in the graph");
            Resource::BindGroup(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("unlit3d_examples::custom_pipeline::sky"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            }))
        });
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: Some(rebuild),
        }
    }
}

/// The identity of the particle family's pipeline.
#[derive(Clone)]
struct ParticleKey {
    descriptor: Arc<RenderPipelineDesc>,
}

impl PartialEq for ParticleKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for ParticleKey {}

impl core::hash::Hash for ParticleKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

/// The particle family's blueprint, specialised for one render target.
#[derive(Clone, Debug)]
struct ParticleVariant {
    /// The pipeline as built, before the target is applied.
    descriptor: Arc<RenderPipelineDesc>,
    /// The target the pipeline is compiled for.
    surface: SurfaceKey,
}

impl PartialEq for ParticleVariant {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface && Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for ParticleVariant {}

impl core::hash::Hash for ParticleVariant {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.surface.hash(state);
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

impl RenderPipelineKey for ParticleKey {
    type Variant = ParticleVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> ParticleVariant {
        ParticleVariant {
            descriptor: self.descriptor.clone(),
            surface: draw.surface,
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for ParticleVariant {
    type Descriptor = RenderPipelineDesc;

    fn descriptor(&self, _device: &wgpu::Device) -> RenderPipelineDesc {
        let mut descriptor = self.descriptor.as_ref().clone();
        descriptor.set_surface(self.surface);
        descriptor
    }
}

/// The particle family's factory.
///
/// The family keeps the renderer's shared global group, so its group is built
/// from the frame's own camera and globals exactly as a built-in family's is.
struct ParticleFactory;

impl RenderPipelineFactory<RenderPipelineDesc> for ParticleFactory {
    fn descriptor(
        &self,
        context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, RenderPipelineDesc>,
    ) -> RegisteredRenderPipeline {
        let bindings = GlobalBindings::default();
        let layout = context.resources.layout(context.device, bindings);
        let rebuild = context.resources.rebuild(context.device, layout, bindings);
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: Some(rebuild),
        }
    }
}

/// The particle family's per-instance stream.
#[derive(Clone, Copy, Debug, Default)]
struct ParticleInstances;

impl InstanceData for ParticleInstances {
    fn stream(&self) -> InstanceStreamDesc {
        InstanceStreamDesc {
            slot: PARTICLE_INSTANCE_SLOT,
            array_stride: core::mem::size_of::<ParticleRecord>() as u32,
        }
    }

    fn write(&mut self, context: &mut InstanceContext<'_>, out: &mut [u8]) {
        let particle = context
            .world
            .query_one::<&Particle>(context.entity)
            .expect("every visible particle carries a Particle component");
        let record = ParticleRecord {
            origin: [
                particle.origin.x,
                particle.origin.y,
                particle.origin.z,
                particle.phase,
            ],
            motion: [particle.radius, particle.speed, particle.size, particle.bob],
            tint: [
                particle.tint.x,
                particle.tint.y,
                particle.tint.z,
                particle.shape,
            ],
        };
        out.copy_from_slice(record.as_bytes());
    }
}

/// The layout of the sky family's one bind group.
fn sky_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        <SkyUniforms as const_shader_layout::ShaderLayout>::SIZE.get(),
                    ),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// The layout of the particle family's material group.
fn particle_material_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(
                    <ParticleUniforms as const_shader_layout::ShaderLayout>::SIZE.get(),
                ),
            },
            count: None,
        }],
    })
}

/// Compile the sky family's pipeline against the family's bind group layout.
fn sky_pipeline(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> RenderPipelineDesc {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        source: wgpu::ShaderSource::Wgsl(SKY_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    let descriptor = wgpu::RenderPipelineDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            // The triangle is generated from the vertex index.
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: unlit_wgpu::render_attachments::default_depth_stencil_format(device),
            // The sky is the backdrop: it is tested against the depth buffer so
            // anything opaque in front of it wins, but it writes nothing, so it
            // never hides a draw that comes after it.
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TEST_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    RenderPipelineDesc::from_wgpu(&descriptor)
}

/// Compile the particle family's pipeline.
///
/// Slot 0 is left empty — the mesh has no vertex buffer — and slot 1 is the
/// family's own instance stream. Group 0 is the renderer's shared global group
/// and group 1 is the family's uniform block.
fn particle_pipeline(
    device: &wgpu::Device,
    material: &wgpu::BindGroupLayout,
) -> RenderPipelineDesc {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        source: wgpu::ShaderSource::Wgsl(PARTICLE_SHADER.into()),
    });
    let global = global_bind_group_layout(
        device,
        GlobalBindings::default(),
        !supports_storage_buffers(device),
    );
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        bind_group_layouts: &[Some(&global), Some(material)],
        immediate_size: 0,
    });
    let descriptor = wgpu::RenderPipelineDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[None, Some(particle_instance_layout())],
        },
        primitive: wgpu::PrimitiveState {
            // A billboard's winding flips with the camera, so neither face is
            // culled.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: unlit_wgpu::render_attachments::default_depth_stencil_format(device),
            // The sprites are opaque, so they write depth and occlude one
            // another; the shapes are flat, so overlapping them would only
            // flatten the cloud into one silhouette.
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TEST_FORMAT,
                // Opaque: a sprite's colour is exactly its tint, with no
                // blending to soften it.
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    RenderPipelineDesc::from_wgpu(&descriptor)
}

/// The instance-step vertex layout of one particle record.
fn particle_instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<ParticleRecord>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

/// Create the procedural cube map and fill each of its six faces.
///
/// The faces are generated on the CPU from a deterministic hash of the
/// direction each texel looks along, so the pattern is continuous across face
/// seams and identical on every machine — which is what lets a snapshot pin it.
fn upload_sky_cubemap(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let size = SKY_FACE_SIZE;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky_cube"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SKY_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texel_size = SKY_FORMAT
        .block_copy_size(None)
        .expect("the sky format copies whole texels") as usize;
    let mut face = vec![0u8; (size * size) as usize * texel_size];
    for layer in 0..6u32 {
        for y in 0..size {
            for x in 0..size {
                let u = (x as f32 + 0.5) / size as f32;
                let v = (y as f32 + 0.5) / size as f32;
                let texel = sky_texel(cube_direction(layer, u, v));
                let start = ((y * size + x) as usize) * texel_size;
                for (channel, value) in texel.iter().enumerate() {
                    face[start + channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &face,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size * texel_size as u32),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
    }
    texture
}

/// The direction a cube-map texel looks along.
///
/// The faces are in the order a cube map samples them — +X, -X, +Y, -Y, +Z, -Z
/// — and the pair (u, v) runs across the face from its top-left.
fn cube_direction(face: u32, u: f32, v: f32) -> glam::Vec3 {
    let sc = 2.0 * u - 1.0;
    let tc = 2.0 * v - 1.0;
    let direction = match face {
        0 => glam::Vec3::new(1.0, -tc, -sc),
        1 => glam::Vec3::new(-1.0, -tc, sc),
        2 => glam::Vec3::new(sc, 1.0, tc),
        3 => glam::Vec3::new(sc, -1.0, -tc),
        4 => glam::Vec3::new(sc, -tc, 1.0),
        _ => glam::Vec3::new(-sc, -tc, -1.0),
    };
    direction.normalize()
}

/// The colour one cube-map texel takes, from the direction it looks along.
///
/// The map holds two flat cloud colours and nothing else: a texel is either
/// inside a cloud or it is not, so the shader can lay it over the sky with one
/// blend and no falloff. The alpha of the result says whether there is a cloud
/// there at all.
fn sky_texel(direction: glam::Vec3) -> [f32; 4] {
    let puff = value_noise(direction * 9.0);
    if puff < 0.66 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    // Two flat tones, picked by the same noise, so a cloud reads as a drawn
    // shape rather than as a shaded one.
    let colour = if puff > 0.72 {
        glam::Vec3::new(1.00, 0.86, 0.92)
    } else {
        glam::Vec3::new(0.72, 0.62, 0.88)
    };
    [colour.x, colour.y, colour.z, 1.0]
}

/// Trilinearly interpolated value noise over the integer lattice.
fn value_noise(point: glam::Vec3) -> f32 {
    let base = point.floor();
    let fraction = point - base;
    // The interpolation is smoothed so a cloud's outline is a clean curve
    // rather than the staircase of the lattice it was sampled on.
    let smooth = fraction * fraction * (glam::Vec3::splat(3.0) - fraction * 2.0);
    let (x, y, z) = (base.x as i32, base.y as i32, base.z as i32);
    let mut value = 0.0;
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let corner = glam::Vec3::new(
                    if dx == 1 { smooth.x } else { 1.0 - smooth.x },
                    if dy == 1 { smooth.y } else { 1.0 - smooth.y },
                    if dz == 1 { smooth.z } else { 1.0 - smooth.z },
                );
                value += corner.x * corner.y * corner.z * hash3(x + dx, y + dy, z + dz);
            }
        }
    }
    value
}

/// A deterministic hash of three integers into the unit interval.
fn hash3(x: i32, y: i32, z: i32) -> f32 {
    let mut hash = (x as u32).wrapping_mul(0x9E37_79B9)
        ^ (y as u32).wrapping_mul(0x85EB_CA6B)
        ^ (z as u32).wrapping_mul(0xC2B2_AE35);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2C1B_3C6D);
    hash ^= hash >> 12;
    hash = hash.wrapping_mul(0x297A_2D39);
    hash ^= hash >> 15;
    (hash >> 8) as f32 / (1u32 << 24) as f32
}

/// Build the scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    let device = source.device(&world.root_scope());
    let queue = source.queue(&world.root_scope());

    // -- the sky family ------------------------------------------------------
    let sky_layout = sky_bind_group_layout(&device);
    let sky_uniforms_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky_uniforms"),
        size: <SkyUniforms as const_shader_layout::ShaderLayout>::SIZE.get(),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let sky_texture = upload_sky_cubemap(&device, &queue);
    // The texture's default view is two-dimensional; a cube map is sampled
    // through a view of the whole array as a cube, which is created here and
    // entered in the graph as a resource of its own.
    let sky_view = TextureExt::create_view(
        &sky_texture,
        &wgpu::TextureViewDescriptor {
            label: Some("unlit3d_examples::custom_pipeline::sky_cube"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        },
    );
    let (sky_view, sky_uniforms) = {
        let root = world.root_scope();
        let mut graph = context.graph(&root);
        let sky_texture = graph.insert(sky_texture, None);
        let sky_view = graph.insert(sky_view, None);
        // The view keeps the texture alive, so the graph needs no other handle
        // on it.
        graph.add_dependency(&sky_view, &sky_texture);
        let sky_uniforms = graph.insert(sky_uniforms_buffer.clone(), None);
        (sky_view, sky_uniforms)
    };
    let sky_sampler = source.register_sampler(
        &world.root_scope(),
        Some(wgpu::SamplerDescriptor {
            label: Some("unlit3d_examples::custom_pipeline::sky"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }),
    );
    source.register_family::<SkyKey, _, _>(
        &world.root_scope(),
        SkyFactory {
            device: device.clone(),
            layout: sky_layout.clone(),
            uniforms: sky_uniforms,
            view: sky_view,
            sampler: sky_sampler,
        },
        (),
    );
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            clippy::arc_with_non_send_sync,
            reason = "a wgpu handle is not `Send` on wasm32; one `Arc` type everywhere is simpler, and on wasm32 it costs nothing over `Rc`"
        )
    )]
    let sky_pipeline = GpuRenderPipeline::new(SkyKey {
        descriptor: Arc::new(sky_pipeline(&device, &sky_layout)),
    });
    let sky_mesh = source.allocate_mesh(
        &world.root_scope(),
        MeshDesc {
            count: SKY_VERTICES,
            aabb: Aabb::new(glam::Vec3::ZERO, glam::Vec3::splat(SKY_EXTENT)),
            ..Default::default()
        },
    );

    // -- the particle family -------------------------------------------------
    let particle_layout = particle_material_layout(&device);
    let particle_uniforms_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particle_uniforms"),
        size: <ParticleUniforms as const_shader_layout::ShaderLayout>::SIZE.get(),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let particle_uniforms = context
        .graph(&world.root_scope())
        .insert(particle_uniforms_buffer.clone(), None);
    source.register_family::<ParticleKey, _, _>(
        &world.root_scope(),
        ParticleFactory,
        ParticleInstances,
    );
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            clippy::arc_with_non_send_sync,
            reason = "a wgpu handle is not `Send` on wasm32; one `Arc` type everywhere is simpler, and on wasm32 it costs nothing over `Rc`"
        )
    )]
    let particle_pipeline = GpuRenderPipeline::new(ParticleKey {
        descriptor: Arc::new(particle_pipeline(&device, &particle_layout)),
    });
    let particle_mesh = source.allocate_mesh(
        &world.root_scope(),
        MeshDesc {
            count: PARTICLE_VERTICES,
            aabb: Aabb::new(
                glam::Vec3::new(0.0, 1.30, 0.0),
                glam::Vec3::new(3.0, 1.60, 3.0),
            ),
            ..Default::default()
        },
    );
    let particle_material = {
        let device = device.clone();
        let layout = particle_layout.clone();
        let uniforms = particle_uniforms.clone();
        source.allocate_material(
            &world.root_scope(),
            move |graph| {
                let uniforms = graph
                    .get(&uniforms)
                    .expect("the particles' uniform buffer is in the graph");
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("unlit3d_examples::custom_pipeline::particles"),
                    layout: &layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    }],
                })
            },
            [particle_uniforms.clone().erase()],
        )
    };

    // No family of the built-in lit pipeline is registered: every draw in this
    // scene goes through a pipeline the example compiled itself.
    let source_entity = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(source)]);
    let camera_entity = super::spawn_in(
        world,
        source_entity,
        bundle![camera_looking_at(
            orbit_eye(ORBIT_START),
            CAMERA_TARGET,
            size,
        ),],
    );

    super::spawn_in(
        world,
        source_entity,
        Sky {
            mesh: sky_mesh,
            pipeline: sky_pipeline,
        },
    );
    // One mesh, one pipeline and one material for the whole cloud, so the
    // renderer folds the particles into a single instanced draw and every
    // sprite's placement comes from its own record.
    for particle in particle_cloud() {
        super::spawn_in(
            world,
            source_entity,
            Sprite {
                mesh: particle_mesh.clone(),
                pipeline: particle_pipeline.clone(),
                material: particle_material.clone(),
                particle,
            },
        );
    }

    // A capture replays the frozen sequence so a stored frame stays
    // reproducible; the window instead runs the motion continuously from the
    // frame delta, which is what this clock accumulates.
    let reproducible = options.reproducible;
    let mut clock = 0.0f32;

    SceneControl {
        advance: Box::new(move |world, frame, delta, frame_size| {
            // A capture walks the sequence the snapshots froze, so its frames
            // are exactly the stored ones. The window runs the same motion
            // continuously from the frame delta, so the orbit and the particles
            // move at the pace the display refreshes rather than in the two
            // steps a second a held sequence gives.
            let (time, angle) = if reproducible {
                (
                    frame as f32 * SEQUENCE_STEP,
                    ORBIT_START + frame as f32 * ORBIT_STEP,
                )
            } else {
                clock += delta;
                (clock, ORBIT_START + clock * ORBIT_RADIANS_PER_SECOND)
            };
            super::aim_camera(
                world,
                source_entity,
                camera_entity,
                orbit_eye(angle),
                CAMERA_TARGET,
                frame_size,
            );

            let camera = world
                .scope(source_entity)
                .expect("the source is alive")
                .query_one::<&Camera>(camera_entity)
                .expect("the scene's camera exists");
            let sky = SkyUniforms {
                zenith: glam::Vec4::new(0.29, 0.45, 0.95, 1.0),
                horizon: glam::Vec4::new(0.98, 0.62, 0.52, 1.0),
                ground: glam::Vec4::new(0.20, 0.24, 0.52, 1.0),
                sun_direction: glam::Vec4::new(
                    SUN_DIRECTION.x,
                    SUN_DIRECTION.y,
                    SUN_DIRECTION.z,
                    1.0,
                ),
                sun_color: glam::Vec4::new(1.0, 0.88, 0.35, 1.0),
                // The ray is built in view space and rotated here, so the sky
                // follows whatever camera the frame chose.
                view_rotation: glam::Mat4::from_mat3(
                    glam::Mat3::from_mat4(camera.view_from_world).transpose(),
                ),
                projection: glam::Vec4::new(
                    1.0 / camera.clip_from_view.x_axis.x,
                    1.0 / camera.clip_from_view.y_axis.y,
                    0.0,
                    0.0,
                ),
            };
            queue.write_buffer(&sky_uniforms_buffer, 0, sky.as_bytes());

            let particles = ParticleUniforms {
                clock: glam::Vec4::new(time, 0.0, 0.0, 0.0),
            };
            queue.write_buffer(&particle_uniforms_buffer, 0, particles.as_bytes());
        }),
        snapshot: Box::new(|frame| {
            (frame < FRAMES).then(|| format!("custom_pipeline/frame_{frame:02}.webp"))
        }),
    }
}

/// The eye point of the orbit at `angle` radians.
fn orbit_eye(angle: f32) -> glam::Vec3 {
    glam::Vec3::new(
        ORBIT_RADIUS * angle.sin(),
        ORBIT_HEIGHT,
        ORBIT_RADIUS * angle.cos(),
    )
}

/// The particle cloud: a ring of sprites, each with its own orbit, size, hue
/// and shape.
fn particle_cloud() -> [Particle; PARTICLE_COUNT] {
    core::array::from_fn(|index| {
        let slot = index as f32;
        let angle = slot * core::f32::consts::TAU / PARTICLE_COUNT as f32;
        // Alternating radii keep the cloud from reading as a flat ring.
        let ring = if index.is_multiple_of(2) { 1.35 } else { 2.05 };
        Particle {
            origin: glam::Vec3::new(
                angle.cos() * ring,
                1.05 + (index % 3) as f32 * 0.28,
                angle.sin() * ring,
            ),
            phase: slot * 0.9,
            radius: 0.22 + (index % 3) as f32 * 0.11,
            speed: 0.75 + (index % 4) as f32 * 0.22,
            size: 0.20 + (index % 3) as f32 * 0.06,
            bob: 0.16 + (index % 2) as f32 * 0.12,
            shape: (index % 3) as f32,
            tint: PARTICLE_TINTS[index % PARTICLE_TINTS.len()],
        }
    })
}
