//! A caller's family carries its own per-instance vertex data.
//!
//! The built-in unlit family reads a per-instance record from a buffer the
//! source owns and binds at a fixed slot. This test registers a second family
//! whose pipeline declares a different instance-step layout, at a different
//! vertex-buffer slot, and whose records come from an ordinary world component
//! -- proving the per-instance stream is a general mechanism a caller drives,
//! not a privilege of the built-in shader.

pub mod common;

use std::sync::Arc;

use arrayvec::ArrayVec;
use common::*;
use unlit_wgpu::specialize::{
    PipelineVariant, RenderPipelineDesc, SpecializedPipeline, VertexLayout,
};
use unlit_wgpu_test_util::{gpu_test_main, gpu_tests};
use unlit3d::pipeline::DrawContext;
use unlit3d::prelude::*;
use zerocopy::IntoBytes;

/// An interleaved `position + colour` vertex, matching `VertexInput` below.
#[repr(C)]
#[derive(Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// The shader the custom pipeline compiles.
///
/// The mesh's own attributes are authored in clip space, and the instance
/// stream supplies the placement and tint: exactly the division of labour the
/// built-in unlit pipeline uses, but with a record layout of the caller's
/// choosing.
const SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) offset: vec4<f32>,
    @location(3) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(input.position + input.offset.xyz, 1.0);
    out.color = input.color * input.tint;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}"#;

/// A white square in clip space: two triangles, drawn twice with different
/// instance records.
const SQUARE: [Vertex; 6] = [
    Vertex {
        position: [-0.5, -0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [0.5, 0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [-0.5, -0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [0.5, 0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [-0.5, 0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
    },
];

/// The vertex layout of `Vertex`.
const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

/// The vertex-buffer slot the family's instance stream binds at.
///
/// The slot is the index into the pipeline's vertex-buffer list, so it is 1:
/// slot 0 is the mesh's own vertices.
const INSTANCE_SLOT: u32 = 1;

/// One instance record: a clip-space offset and a tint, both `vec4<f32>`.
#[repr(C)]
#[derive(Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
struct InstanceRecord {
    offset: [f32; 4],
    tint: [f32; 4],
}

/// The instance-step layout the custom pipeline declares.
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4];

fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<InstanceRecord>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &INSTANCE_ATTRIBUTES,
    }
}

/// A [VertexBufferDesc] for the mesh's own vertices in slot 0.
fn vertex_buffer(buffer: wgpu::Buffer) -> VertexBufferDesc {
    let layout = vertex_layout();
    VertexBufferDesc {
        slot: 0,
        buffer,
        array_stride: layout.array_stride,
        step_mode: layout.step_mode,
        attributes: layout.attributes.into(),
    }
}

fn custom_pipeline(device: &wgpu::Device) -> RenderPipelineDesc {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("test::custom_instances::shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("test::custom_instances::layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let descriptor = wgpu::RenderPipelineDescriptor {
        label: Some("test::custom_instances::pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            // Slot 0 is the mesh's vertices, slot 1 the family's instance
            // records.
            buffers: &[Some(vertex_layout()), Some(instance_layout())],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: unlit_wgpu::render_attachments::default_depth_stencil_format(device),
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
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
        multiview_mask: None,
        cache: None,
    };
    RenderPipelineDesc::from_wgpu(&descriptor)
}

/// The family's key: one shared pipeline descriptor, identified by it.
#[derive(Clone)]
struct CustomInstanceKey {
    descriptor: Arc<RenderPipelineDesc>,
}

impl CustomInstanceKey {
    fn new(descriptor: RenderPipelineDesc) -> Self {
        Self {
            descriptor: Arc::new(descriptor),
        }
    }
}

impl PartialEq for CustomInstanceKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for CustomInstanceKey {}

impl core::hash::Hash for CustomInstanceKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

#[derive(Clone)]
struct CustomInstanceVariant {
    descriptor: Arc<RenderPipelineDesc>,
    vertex_layout: VertexLayout,
    index_format: Option<wgpu::IndexFormat>,
}

impl PartialEq for CustomInstanceVariant {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.descriptor, &other.descriptor)
            && self.vertex_layout == other.vertex_layout
            && self.index_format == other.index_format
    }
}

impl Eq for CustomInstanceVariant {}

impl core::hash::Hash for CustomInstanceVariant {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
        self.vertex_layout.hash(state);
        self.index_format.hash(state);
    }
}

impl core::fmt::Debug for CustomInstanceVariant {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CustomInstanceVariant")
            .field("index_format", &self.index_format)
            .finish_non_exhaustive()
    }
}

impl RenderPipelineKey for CustomInstanceKey {
    type Variant = CustomInstanceVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> Self::Variant {
        CustomInstanceVariant {
            descriptor: self.descriptor.clone(),
            vertex_layout: draw.mesh.vertex_layout.clone(),
            index_format: draw
                .mesh
                .parts
                .index_buffer
                .as_ref()
                .map(|(_, format)| *format),
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for CustomInstanceVariant {
    type Descriptor = RenderPipelineDesc;

    fn descriptor(&self, _device: &wgpu::Device) -> RenderPipelineDesc {
        self.descriptor.as_ref().clone()
    }
}

/// The family binds no frame resources: its whole placement rides the instance
/// stream.
struct NoBindingFactory;

impl RenderPipelineFactory<RenderPipelineDesc> for NoBindingFactory {
    fn descriptor(
        &self,
        _context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, RenderPipelineDesc>,
    ) -> RegisteredRenderPipeline {
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: None,
        }
    }
}

/// The per-instance data the family reads, as a plain world component.
#[derive(Clone, Copy, Debug)]
struct CustomInstance {
    offset: [f32; 3],
    tint: [f32; 4],
}

/// The family's [InstanceData].
///
/// It owns slot [INSTANCE_SLOT] and a 32-byte record, unlike the built-in unlit
/// family's slot 2 and `MeshInstance`.
#[derive(Clone, Copy, Debug, Default)]
struct CustomInstances;

impl InstanceData for CustomInstances {
    fn stream(&self) -> InstanceStreamDesc {
        InstanceStreamDesc {
            slot: INSTANCE_SLOT,
            array_stride: core::mem::size_of::<InstanceRecord>() as u32,
        }
    }

    fn write(&mut self, context: &InstanceContext<'_>, out: &mut [u8]) {
        let instance = context
            .world
            .get::<CustomInstance>(context.entity)
            .expect("every visible custom entity carries `CustomInstance`");
        let record = InstanceRecord {
            offset: [
                instance.offset[0],
                instance.offset[1],
                instance.offset[2],
                0.0,
            ],
            tint: instance.tint,
        };
        out.copy_from_slice(record.as_bytes());
    }
}

/// Upload one interleaved vertex buffer for the square.
fn upload(device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[Vertex]) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test::custom_instances::vertices"),
        size: core::mem::size_of_val(vertices) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, vertices.as_bytes());
    buffer
}

/// One mesh, one pipeline, two entities whose only difference is the
/// per-instance record the custom family writes for them.
///
/// Both entities resolve to the same pipeline and the same mesh, so the scene
/// folds them into a single instanced draw; the shader reads each instance's own
/// offset and tint, which is what makes the two squares land in different places
/// with different colours.
async fn a_custom_family_owns_its_instance_stream() {
    let ctx = Ctx::headless().await;
    let mut world = unlit_ecs::World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    let (pipeline, mesh) = gpu.with_mesh_source(&world, |source, world| {
        let device = source.device(world);
        let queue = source.queue(world);
        let key = CustomInstanceKey::new(custom_pipeline(&device));
        // The family owns its instance stream; nothing here touches the unlit
        // family's buffer or slot.
        source.register_family::<CustomInstanceKey, _, _>(world, NoBindingFactory, CustomInstances);
        let pipeline = GpuRenderPipeline::new(key);
        let buffer = upload(&device, &queue, &SQUARE);
        let mesh = source.allocate_mesh(
            world,
            MeshDesc {
                vertex_buffers: ArrayVec::try_from(&[vertex_buffer(buffer)][..])
                    .expect("one buffer fits"),
                count: SQUARE.len() as u32,
                ..Default::default()
            },
        );
        (pipeline, mesh)
    });

    // The renderer wants a camera to derive its uniforms from, but the custom
    // shader reads none of them.
    world.spawn((camera_view(WIDTH as f32 / HEIGHT as f32),));
    world.spawn((
        mesh.clone(),
        pipeline.clone(),
        CustomInstance {
            offset: [-0.35, 0.0, 0.0],
            tint: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    world.spawn((
        mesh,
        pipeline,
        CustomInstance {
            offset: [0.35, 0.0, 0.0],
            tint: [0.0, 1.0, 0.0, 1.0],
        },
    ));

    let target = gpu.render_to_offscreen(&world, "test::custom_instances");
    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };

    // Each square covers a quarter of the width, offset from the centre, and
    // carries the tint of its own record.
    let left = frame.pixel_u8(WIDTH / 4, HEIGHT / 2);
    assert!(
        left[0] > 200 && left[1] < 60 && left[2] < 60,
        "the left square reads its own instance record, got {left:?}"
    );
    let right = frame.pixel_u8(WIDTH * 3 / 4, HEIGHT / 2);
    assert!(
        right[1] > 200 && right[0] < 60 && right[2] < 60,
        "the right square reads its own instance record, got {right:?}"
    );
}

// The registry both runners drive: `cargo nextest` natively, and a browser
// through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    a_custom_family_owns_its_instance_stream,
}

gpu_test_main!(all_tests());
