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
//!   bind group is the caller's own: the family names its three inputs when it
//!   makes the recipe, so the group is rebuilt when one of them is replaced and
//!   never because a draw moved to another camera;
//! * a **particle** family whose per-instance stream comes from an ordinary
//!   world component, and whose vertex stage animates the sprite from a clock
//!   the example advances. It takes the frame-bound recipe for the camera, as a
//!   built-in family does, and adds its own uniform block at the material group.
//!
//! Neither family declares a single mesh vertex attribute: the sky's three
//! vertices and each particle's six come from the vertex index, so the meshes
//! carry nothing but a count and a bounding box. The animation is parametric:
//! the camera orbits the cloud while the particles drift and spin from one
//! clock, so the window drives it from the frame delta instead of holding each
//! frozen frame, and its snapshots freeze eight instants of that orbit. Its
//! look is deliberately flat: banded colours, hard-edged sprites and a
//! hard-edged sun rather than any attempt at a photographic sky.

use unlit_ecs::{Bundle, DynamicBundle, bundle};

use super::{
    SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_FORMAT, TEST_SIZE, camera_looking_at,
};
use unlit_wgpu::pipeline::{GlobalBindings, global_bind_group_layout, supports_storage_buffers};
use unlit_wgpu::resources::{ResHandle, ResourceGraph, TextureExt, TextureView};
use unlit_wgpu::specialize::{
    PipelineVariant, RenderPipelineDesc, SpecializedPipeline, SurfaceKey, SurfaceTarget,
};
use unlit3d::pipeline::{
    DrawContext, FamilyContext, GlobalRecipe, InstanceContext, InstanceData, InstanceStreamDesc,
    RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineKey,
};
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

mod particles;
mod shaders;
mod sky;

use self::particles::*;
use self::shaders::*;
use self::sky::*;

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
        let sky_texture = graph.get().insert(sky_texture, None);
        let sky_view = graph.get().insert(sky_view, None);
        // The view keeps the texture alive, so the graph needs no other handle
        // on it.
        graph.get().add_dependency(&sky_view, &sky_texture);
        let sky_uniforms = graph.get().insert(sky_uniforms_buffer.clone(), None);
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
            layout: sky_layout.clone(),
            uniforms: sky_uniforms,
            view: sky_view,
            sampler: sky_sampler,
        },
        (),
    );
    let sky_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        source: wgpu::ShaderSource::Wgsl(SKY_SHADER.into()),
    });
    let sky_pipeline = GpuRenderPipeline::new(SkyKey {
        module: sky_module,
        layout: sky_layout.clone(),
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
        .get()
        .insert(particle_uniforms_buffer.clone(), None);
    source.register_family::<ParticleKey, _, _>(
        &world.root_scope(),
        ParticleFactory,
        ParticleInstances,
    );
    let particle_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        source: wgpu::ShaderSource::Wgsl(PARTICLE_SHADER.into()),
    });
    let particle_pipeline = GpuRenderPipeline::new(ParticleKey {
        module: particle_module,
        layout: particle_layout.clone(),
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

            let mut camera = world
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
                    glam::Mat3::from_mat4(camera.get().view_from_world).transpose(),
                ),
                projection: glam::Vec4::new(
                    1.0 / camera.get().clip_from_view.x_axis.x,
                    1.0 / camera.get().clip_from_view.y_axis.y,
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
