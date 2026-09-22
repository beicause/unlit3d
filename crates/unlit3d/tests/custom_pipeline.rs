//! End-to-end test that a caller's own pipeline is a first-class citizen of
//! `unlit3d`.
//!
//! The scene is drawn by a hand-written `wgpu::RenderPipeline` — no WESL, no
//! unlit pipeline, no renderer-provided bindings — to show that registering a
//! pipeline and drawing with it needs nothing from the built-in shader.

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// An interleaved `position + colour` vertex, matching `VERTEX` in the WGSL
/// below.
#[repr(C)]
#[derive(Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// The shader the custom pipeline compiles: a plain vertex passthrough with a
/// hard-coded transform, and a fragment stage writing the vertex colour.
const SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // A fixed orthographic-ish placement: the geometry is authored already
    // in clip space, so the pipeline needs no camera uniform at all.
    out.position = vec4<f32>(input.position, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}"#;

/// A single triangle covering the middle of the frame, in clip space.
const TRIANGLE: [Vertex; 3] = [
    Vertex {
        position: [-0.75, -0.75, 0.5],
        color: [1.0, 0.0, 0.0, 1.0],
    },
    Vertex {
        position: [0.75, -0.75, 0.5],
        color: [0.0, 1.0, 0.0, 1.0],
    },
    Vertex {
        position: [0.0, 0.75, 0.5],
        color: [0.0, 0.0, 1.0, 1.0],
    },
];

/// The vertex layout of [`Vertex`], declared once so both the pipeline and
/// the uploaded bytes agree on it.
const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

/// Build the custom pipeline: no bind groups, one interleaved vertex buffer.
fn custom_pipeline(device: &wgpu::Device) -> wgpu::RenderPipeline {
    // The renderer's attachments are multisampled and carry a stencil aspect,
    // so a pipeline that draws into them has to declare both.
    let format = COLOR_FORMAT;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("test::custom::shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("test::custom::layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("test::custom::pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(vertex_layout())],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu_unlit_render::render_attachments::default_depth_stencil_format(device),
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        // An external render target is used single-sampled (the renderer's
        // own MSAA attachments only apply to its internal target).
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// The binding the per-mesh tint uniform is declared at, inside the mesh
/// bind group.
const MESH_TINT_BINDING: u32 = 0;

/// The shader [`tinted_pipeline`] compiles: the same passthrough vertex
/// stage, but the fragment stage adds a per-mesh tint bound at group
/// [`MESH_GROUP`] rather than using the vertex colour alone.
const TINTED_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@group(2) @binding(0) var<uniform> tint: vec4<f32>;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(input.position, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color + tint;
}"#;

/// Build a pipeline whose fragment stage reads a per-mesh tint from `layout`.
fn tinted_pipeline(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("test::custom::tinted::shader"),
        source: wgpu::ShaderSource::Wgsl(TINTED_SHADER.into()),
    });

    // The tint is bound at group 2, so the two groups below it are empty
    // placeholders — the same slots the renderer would bind a global and a
    // material group into.
    let empty = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("test::custom::empty::layout"),
        entries: &[],
    });
    let layouts = [Some(&empty), Some(&empty), Some(layout)];
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("test::custom::tinted::layout"),
        bind_group_layouts: &layouts,
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("test::custom::tinted::pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(vertex_layout())],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu_unlit_render::render_attachments::default_depth_stencil_format(device),
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// A caller-registered pipeline draws, with no built-in shader involved.
#[test]
fn a_custom_pipeline_draws_through_the_ecs() {
    use zerocopy::IntoBytes;

    let ctx = Ctx::headless();

    // A renderer with no unlit pipeline at all: the only pipeline is ours.
    let mut world = unlit_ecs::LocalWorld::new();
    let renderer = world.spawn((
        unlit_ecs::Resource,
        Renderer::new(ctx.device.clone(), ctx.queue.clone(), WIDTH, HEIGHT),
    ));

    // Register the hand-written pipeline like any other.
    let pipeline = world.with_mut::<Renderer, _>(renderer, |r| {
        let pipeline = custom_pipeline(&r.device);
        r.register_pipeline(PipelineDesc {
            pipeline,
            global: None,
            material_layout: None,
            mesh_layout: None,
        })
    });

    // Upload the triangle as one interleaved vertex buffer in slot 0.
    let mesh = world.with_mut::<Renderer, _>(renderer, |r| {
        let buffer = r.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test::custom::vertices"),
            size: core::mem::size_of_val(&TRIANGLE) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        r.queue.write_buffer(&buffer, 0, TRIANGLE.as_bytes());
        r.allocate_mesh(MeshDesc {
            vertex_buffers: vec![VertexBufferDesc { slot: 0, buffer }],
            count: TRIANGLE.len() as u32,
            ..Default::default()
        })
    });

    let mesh = mesh.expect("renderer entity exists");
    let pipeline = pipeline.expect("renderer entity exists");
    assert_eq!(
        pipeline.index(),
        0,
        "the custom pipeline is the first registered"
    );

    // The renderer still wants a camera to derive its uniforms from; nothing
    // in the custom pipeline reads them, but the frame is only drawn when one
    // exists.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));
    world.spawn((Transform::default(), mesh, pipeline));

    let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let (target, target_view) = offscreen_target(&ctx.device, "test::custom");
    let _ = world.with_mut::<Renderer, _>(renderer, |r| {
        r.render(&world, Some(&target_view));
    });

    if let Some(err) = wgpu_unlit_test_util::busy_wait_block_on(scope.pop()) {
        panic!("validation error during custom draw: {err}");
    }

    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };

    // The triangle covers the centre of the frame with its vertex colours, so
    // the three corners read back as the three primaries.
    let centre = frame.pixel_u8(WIDTH / 2, (HEIGHT as f32 * 0.62) as u32);
    assert!(
        centre[2] > centre[0] && centre[2] > centre[1],
        "the top vertex is blue, got {centre:?}"
    );

    let left = frame.pixel_u8((WIDTH as f32 * 0.25) as u32, (HEIGHT as f32 * 0.8) as u32);
    assert!(
        left[0] > left[1] && left[0] > left[2],
        "the bottom-left vertex is red, got {left:?}"
    );

    let right = frame.pixel_u8((WIDTH as f32 * 0.72) as u32, (HEIGHT as f32 * 0.8) as u32);
    assert!(
        right[1] > right[0] && right[1] > right[2],
        "the bottom-right vertex is green, got {right:?}"
    );
}

/// One pipeline draws several meshes, each through its own mesh bind group:
/// the pipeline handle is a plain shareable component, and the bind group
/// that differs per draw is the mesh's, not the pipeline's.
#[test]
fn one_pipeline_draws_many_meshes() {
    let ctx = Ctx::headless();
    let mut world = unlit_ecs::LocalWorld::new();
    let renderer = world.spawn((
        unlit_ecs::Resource,
        Renderer::new(ctx.device.clone(), ctx.queue.clone(), WIDTH, HEIGHT),
    ));

    // A per-mesh tint the fragment stage adds to the interpolated vertex
    // colour. It is what makes two draws of one pipeline differ, and it
    // lives in the mesh-level bind group (`MESH_GROUP`).
    let layout = ctx
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test::custom::mesh::layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: MESH_TINT_BINDING,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

    let (pipeline, red, green) = world
        .with_mut::<Renderer, _>(renderer, |r| {
            let pipeline = tinted_pipeline(&r.device, &layout);
            let pipeline = r.register_pipeline(PipelineDesc {
                pipeline,
                global: None,
                material_layout: None,
                mesh_layout: Some(layout.clone()),
            });

            // The two meshes upload identical geometry and differ only in
            // the tint their mesh bind group carries.
            let mut mesh = |tint: [f32; 4]| {
                let buffer = upload(&r.device, &r.queue, &TRIANGLE);
                let tint_buffer = r.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("test::custom::tint"),
                    size: core::mem::size_of::<[f32; 4]>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                use zerocopy::IntoBytes;
                r.queue.write_buffer(&tint_buffer, 0, tint.as_bytes());
                let bind_group = r.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("test::custom::mesh::bind_group"),
                    layout: &layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: MESH_TINT_BINDING,
                        resource: tint_buffer.as_entire_binding(),
                    }],
                });
                r.allocate_mesh(MeshDesc {
                    vertex_buffers: vec![VertexBufferDesc { slot: 0, buffer }],
                    count: TRIANGLE.len() as u32,
                    bind_group: Some(bind_group),
                    ..Default::default()
                })
            };
            (
                pipeline,
                mesh([1.0, 0.0, 0.0, 0.0]),
                mesh([0.0, 1.0, 0.0, 0.0]),
            )
        })
        .expect("renderer entity exists");

    // The same handle is spawned twice: it is `Copy`, not owned by a mesh.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));
    // The mesh is uploaded in clip space and the vertex stage ignores the
    // instance matrix, so the two draws land on top of each other; only the
    // tint they were bound with distinguishes them.
    world.spawn((Transform::default(), red, pipeline));
    world.spawn((Transform::default(), green, pipeline));

    let (target, target_view) = offscreen_target(&ctx.device, "test::custom::shared");
    let _ = world.with_mut::<Renderer, _>(renderer, |r| {
        r.render(&world, Some(&target_view));
    });

    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };
    // The nearer draw wins the depth test, so the centre reads the vertex
    // colour of one triangle plus that mesh's tint: the red tint lifts the
    // red and green channels above the plain blue vertex colour the
    // untinted pipeline produced.
    let centre = frame.pixel_u8(WIDTH / 2, (HEIGHT as f32 * 0.62) as u32);
    assert!(
        centre[0] > 0 && centre[1] > 0,
        "the mesh bind group was bound, got {centre:?}"
    );
}

/// Upload one interleaved vertex buffer for the triangle.
fn upload(device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[Vertex]) -> wgpu::Buffer {
    use zerocopy::IntoBytes;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test::custom::vertices"),
        size: core::mem::size_of_val(vertices) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, vertices.as_bytes());
    buffer
}
