//! End-to-end GPU tests for the resources a caller keeps between frames.
//!
//! The instance stream, the buffer pool, the vertex pool and the texel array
//! each hold GPU memory across frames and hand a caller an offset into it. The
//! unit tests beside each type check the bookkeeping against a no-op device;
//! what they cannot check is the bytes, which is what these tests do: that what
//! a caller writes really lands at the offset it was given, and that growing a
//! helper carries what was already written across to the new resource.

use crate::common::*;
use unlit_wgpu::buffer_pool::BufferPool;
use unlit_wgpu::instance_stream::{InstanceBuffer, InstanceStreamDesc};
use unlit_wgpu::readback::{readback_buffer, readback_texture};
use unlit_wgpu::render_attachments::{
    RenderAttachments, create_color_target, depth_clear, stencil_clear,
};
use unlit_wgpu::resources::TextureExt;
use unlit_wgpu::specialize::{VertexAttributes, VertexBufferLayoutDesc};
use unlit_wgpu::texel_array::Array;
use unlit_wgpu::vertex_pool::VertexStreamPool;
use zerocopy::IntoBytes;

/// Bytes per instance record the stream tests use: four `u32`s.
const RECORD: u32 = 16;

/// Bytes per element the texel test stores: one `vec4<f32>`, so an element is
/// exactly one texel.
const ELEMENT: u64 = 16;

/// How many elements the texel test uploads, and how wide its frame is.
const ELEMENTS: u32 = 32;

/// The format the texel test renders into.
///
/// Linear rather than sRGB, so a texel byte is the value times 255, rounded,
/// and the shader's output reads back without an encoding step to model.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The value a linear 8-bit channel stores, as the shader would hand it over.
fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Upload `count` records of zeros through a fresh encoder, as a frame would.
fn upload_records(ctx: &Ctx, stream: &mut InstanceBuffer, count: usize) {
    let records = vec![0u8; count * RECORD as usize];
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::instances::encoder"),
        });
    stream.upload(&ctx.device, &mut encoder, &records);
    ctx.queue.submit([encoder.finish()]);
}

// ---------------------------------------------------------------------------
// Instance stream
// ---------------------------------------------------------------------------

/// The stream allocates on the first upload and then grows geometrically, so a
/// scene that adds an instance per frame does not reallocate every frame.
async fn an_instance_stream_grows_geometrically() {
    let ctx = Ctx::headless().await;
    let mut stream = InstanceBuffer::new(InstanceStreamDesc {
        slot: 1,
        array_stride: RECORD,
        label: Some("test::instances"),
    });
    assert!(
        stream.buffer().is_none(),
        "no buffer is allocated before the first upload"
    );

    let record = u64::from(RECORD);
    upload_records(&ctx, &mut stream, 4);
    assert_eq!(
        stream.buffer().expect("the upload allocated one").size(),
        record * 4,
        "the buffer starts out exactly as wide as the first upload"
    );

    // A smaller upload reuses what is there rather than shrinking it: a
    // buffer cannot be resized, and a frame with fewer instances is no reason
    // to allocate another.
    upload_records(&ctx, &mut stream, 2);
    assert_eq!(stream.buffer().expect("still allocated").size(), record * 4);

    // A larger one overshoots, so the next few frames fit without another
    // allocation.
    upload_records(&ctx, &mut stream, 5);
    assert_eq!(
        stream.buffer().expect("still allocated").size(),
        record * 6,
        "the buffer grows to 1.5x its capacity, not to the upload"
    );
}

/// A family that declares no instance stream allocates no buffer, whatever it
/// uploads.
async fn a_stream_with_no_stride_allocates_nothing() {
    let ctx = Ctx::headless().await;
    let mut stream = InstanceBuffer::new(InstanceStreamDesc::NONE);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::instances::encoder"),
        });
    stream.upload(&ctx.device, &mut encoder, &[0u8; 64]);
    ctx.queue.submit([encoder.finish()]);

    assert!(
        stream.buffer().is_none(),
        "a stream with no stride binds nothing, so it stores nothing"
    );
}

/// Records are whole strides: half a record has no instance it could belong to.
async fn a_partial_record_is_rejected() {
    let ctx = Ctx::headless().await;
    let mut stream = InstanceBuffer::new(InstanceStreamDesc {
        slot: 1,
        array_stride: RECORD,
        label: None,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::instances::encoder"),
        });
    stream.upload(&ctx.device, &mut encoder, &[0u8; RECORD as usize + 1]);
}

// ---------------------------------------------------------------------------
// Buffer pool
// ---------------------------------------------------------------------------

/// Every range keeps its own bytes: what one range holds is not what another
/// wrote, which is what makes the pool safe to share between resources.
async fn a_buffer_pool_keeps_each_range_at_its_own_offset() {
    let ctx = Ctx::headless().await;
    let usage = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
    let mut pool = BufferPool::new(&ctx.device, "test::buffer_pool", usage, 512);

    let first = pool
        .allocate(&ctx.device, &ctx.queue, 64)
        .expect("the pool has room");
    let second = pool
        .allocate(&ctx.device, &ctx.queue, 64)
        .expect("the pool has room");
    assert_ne!(
        first.allocation.offset, second.allocation.offset,
        "two live ranges never share an offset"
    );

    let one = [1u8; 64];
    let two = [2u8; 64];
    ctx.queue
        .write_buffer(pool.buffer(), u64::from(first.allocation.offset), &one);
    ctx.queue
        .write_buffer(pool.buffer(), u64::from(second.allocation.offset), &two);

    let bytes = readback_buffer(&ctx.device, &ctx.queue, pool.buffer(), 0, pool.size());
    let at = |range: &unlit_wgpu::buffer_pool::BufferRange| {
        let start = range.allocation.offset as usize;
        &bytes[start..start + range.size as usize]
    };
    assert_eq!(
        at(&first),
        &one[..],
        "the first range holds what it was given"
    );
    assert_eq!(at(&second), &two[..], "the second does too");
}

/// Growing replaces the backing buffer, so the old contents have to be carried
/// across: a range allocated before the grow still holds what it held.
async fn growing_a_buffer_pool_carries_the_contents_across() {
    let ctx = Ctx::headless().await;
    let usage = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
    let mut pool = BufferPool::new(&ctx.device, "test::buffer_pool", usage, 256);

    let first = pool
        .allocate(&ctx.device, &ctx.queue, 128)
        .expect("the pool has room");
    let written = [0xABu8; 128];
    ctx.queue
        .write_buffer(pool.buffer(), u64::from(first.allocation.offset), &written);

    let before = pool.size();
    // A range that does not fit grows the pool, so the buffer this returns
    // ranges from is not the one the write above reached.
    let grown = pool
        .allocate(&ctx.device, &ctx.queue, 512)
        .expect("the pool grows for a range that does not fit");
    assert!(pool.size() > before, "the pool grew");

    let bytes = readback_buffer(&ctx.device, &ctx.queue, pool.buffer(), 0, pool.size());
    let start = first.allocation.offset as usize;
    assert_eq!(
        &bytes[start..start + written.len()],
        &written[..],
        "the grow carried the old contents across"
    );

    // The grown-to range is as writable as the one that was there first.
    let fresh = [0xCDu8; 512];
    ctx.queue
        .write_buffer(pool.buffer(), u64::from(grown.allocation.offset), &fresh);
    let bytes = readback_buffer(&ctx.device, &ctx.queue, pool.buffer(), 0, pool.size());
    let start = grown.allocation.offset as usize;
    assert_eq!(&bytes[start..start + fresh.len()], &fresh[..]);
}

// ---------------------------------------------------------------------------
// Vertex pool
// ---------------------------------------------------------------------------

/// A layout the vertex pool tests stream at: 12 bytes per vertex, which is the
/// stride of a packed `vec3<f32>`.
fn layout() -> VertexBufferLayoutDesc {
    VertexBufferLayoutDesc {
        array_stride: 12,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: VertexAttributes::new(),
    }
}

/// Each allocation's elements land at `offset * stride`, which is the offset
/// the pool reports for drawing it.
async fn a_vertex_pool_writes_each_element_at_its_stride() {
    let ctx = Ctx::headless().await;
    let layout = layout();
    let mut pool = VertexStreamPool::new(
        &ctx.device,
        "test::vertex_pool",
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        4,
    );
    let allocation = pool
        .allocate(&ctx.device, &ctx.queue, std::slice::from_ref(&layout), 4)
        .expect("the pool has room");
    assert_eq!(allocation.count, 4);

    let buffer = pool.buffer(&layout).expect("the layout got a buffer");
    let values = [0x11u8, 0x22, 0x33, 0x44];
    for (index, value) in values.iter().enumerate() {
        let element = allocation.allocation.offset + index as u32;
        ctx.queue.write_buffer(
            buffer,
            VertexStreamPool::byte_offset(&layout, element),
            &[*value; 12],
        );
    }

    let size = buffer.size();
    let bytes = readback_buffer(&ctx.device, &ctx.queue, buffer, 0, size);
    for (index, value) in values.iter().enumerate() {
        let element = allocation.allocation.offset as usize + index;
        let start = element * 12;
        assert_eq!(
            &bytes[start..start + 12],
            &[*value; 12][..],
            "element {element} holds what it was given"
        );
    }
}

/// Growing a stream replaces its buffer, so the elements already written have
/// to be copied across before the next frame draws them.
async fn growing_a_vertex_stream_keeps_its_elements() {
    let ctx = Ctx::headless().await;
    let layout = layout();
    let mut pool = VertexStreamPool::new(
        &ctx.device,
        "test::vertex_pool",
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        4,
    );
    let first = pool
        .allocate(&ctx.device, &ctx.queue, std::slice::from_ref(&layout), 4)
        .expect("the pool has room");
    let written = [0x5Au8; 48];
    let buffer = pool.buffer(&layout).expect("the layout got a buffer");
    ctx.queue.write_buffer(buffer, 0, &written);
    let before = buffer.size();

    // A second allocation does not fit, so the stream grows and its buffer is
    // replaced.
    pool.allocate(&ctx.device, &ctx.queue, std::slice::from_ref(&layout), 4)
        .expect("the pool grows for an allocation that does not fit");
    let buffer = pool.buffer(&layout).expect("the layout kept its buffer");
    assert!(buffer.size() > before, "the stream grew");

    let bytes = readback_buffer(&ctx.device, &ctx.queue, buffer, 0, buffer.size());
    let start = first.allocation.offset as usize * 12;
    assert_eq!(
        &bytes[start..start + written.len()],
        &written[..],
        "the grow carried the elements across"
    );
}

// ---------------------------------------------------------------------------
// Texel array
// ---------------------------------------------------------------------------

/// The shader the texel test draws with: a full-screen triangle whose fragment
/// stage reads one element per pixel, at the texel coordinate the module docs
/// give.
///
/// The element index comes from the pixel's own position, so a frame
/// `ELEMENTS` wide reads every uploaded element exactly once.
const TEXEL_SHADER: &str = r#"
@group(0) @binding(0) var elements: texture_2d<f32>;

const ELEMENTS: i32 = 32;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32((vertex << 1u) & 2u);
    let y = f32(vertex & 2u);
    out.position = vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let width = i32(textureDimensions(elements).x);
    let index = min(i32(floor(input.uv.x * f32(ELEMENTS))), ELEMENTS - 1);
    let texel = vec2<i32>(index % width, index / width);
    return textureLoad(elements, texel, 0);
}
"#;

/// The texture path holds the same bytes the buffer path would: an element
/// reads back as the value it was uploaded as, across a row boundary.
async fn a_texel_array_reads_back_the_elements_it_was_given() {
    let ctx = Ctx::headless().await;
    let elements: [[f32; 4]; ELEMENTS as usize] = core::array::from_fn(|index| {
        let t = index as f32 / (ELEMENTS - 1) as f32;
        [t, 1.0 - t, 0.5, 1.0]
    });

    // A device without storage buffers asks for a texture on its own; asking
    // for one here exercises the same path on every device.
    let max_dimension = ctx.device.limits().max_texture_dimension_2d;
    let mut array = Array::new(
        &ctx.device,
        Some("test::texel"),
        ELEMENT,
        u64::from(ELEMENTS),
        Some(max_dimension),
    );
    assert!(array.is_texel(), "the array took the texture path");

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::texel::encoder"),
        });
    array.upload(&ctx.device, &mut encoder, elements.as_bytes());

    let handle = array.handle();
    assert!(
        matches!(handle, unlit_wgpu::texel_array::ArrayHandle::Texel(_)),
        "the handle names the texture the shader reads"
    );
    let entry = handle.layout_entry(0, wgpu::ShaderStages::FRAGMENT, ELEMENT);
    let bind_group_layout = ctx
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test::texel::layout"),
            entries: &[entry],
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("test::texel::bind_group"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: handle.binding_resource(),
        }],
    });

    let shader = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test::texel::shader"),
            source: wgpu::ShaderSource::Wgsl(TEXEL_SHADER.into()),
        });
    let pipeline_layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("test::texel::pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
    let pipeline = ctx
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("test::texel::pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
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
            // A color-only target: no depth attachment to declare.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

    let target = create_color_target(&ctx.device, COLOR_FORMAT, ELEMENTS, 1);
    let attachments = RenderAttachments::from_views(
        Some(TextureExt::create_view(
            &target,
            &wgpu::TextureViewDescriptor::default(),
        )),
        None,
        None,
    );
    {
        let mut pass = attachments.begin_pass(
            &mut encoder,
            wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            depth_clear(),
            stencil_clear(),
        );
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    ctx.queue.submit([encoder.finish()]);
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");

    let frame = Frame {
        rgba: readback_texture(&ctx.device, &ctx.queue, &target),
        width: ELEMENTS,
        height: 1,
    };
    for (index, element) in elements.iter().enumerate() {
        let wanted = [
            byte(element[0]),
            byte(element[1]),
            byte(element[2]),
            byte(element[3]),
        ];
        let actual = frame.pixel_u8(index as u32, 0);
        for (channel, (&actual, &wanted)) in actual.iter().zip(&wanted).enumerate() {
            assert!(
                i32::from(actual).abs_diff(i32::from(wanted)) <= 1,
                "element {index} channel {channel} read back as {actual}, \
                 expected {wanted}; the whole pixel was {actual_pixel:?}",
                actual_pixel = actual_all(&frame, index as u32),
            );
        }
    }
}

/// The pixel at `index` of the texel test's frame, for a failure message.
fn actual_all(frame: &Frame, index: u32) -> [u8; 4] {
    frame.pixel_u8(index, 0)
}

// This file's share of the suite's registry: the suite root concatenates it
// with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
gpu_tests! {
    an_instance_stream_grows_geometrically,
    a_stream_with_no_stride_allocates_nothing,
    #[should_panic(expected = "instance records are whole strides")]
    a_partial_record_is_rejected,
    a_buffer_pool_keeps_each_range_at_its_own_offset,
    growing_a_buffer_pool_carries_the_contents_across,
    a_vertex_pool_writes_each_element_at_its_stride,
    growing_a_vertex_stream_keeps_its_elements,
    a_texel_array_reads_back_the_elements_it_was_given,
}
