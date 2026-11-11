use super::*;

/// The smallest shader that compiles: a vertex stage returning the origin
/// and a fragment stage returning opaque white.
const MINIMAL_WGSL: &str = r#"
@vertex
fn vs_main() -> @builtin(position) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
"#;

/// A render pipeline on the noop device, for builder tests that only need
/// a handle to name.
fn noop_pipeline(device: &wgpu::Device) -> wgpu::RenderPipeline {
    let module = crate::util::shader_module(device, MINIMAL_WGSL, "test::shader");
    let targets = [Some(wgpu::ColorTargetState {
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("test::pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[test]
fn draw_range_builders_compose() {
    let range = DrawRange::vertices(0..36).with_instances(2..5);
    match range {
        DrawRange::Vertices {
            vertices,
            instances,
        } => {
            assert_eq!(vertices, 0..36);
            assert_eq!(instances, 2..5);
        }
        other => panic!("expected a non-indexed draw, got {other:?}"),
    }

    let range = DrawRange::indexed(0..12)
        .with_base_vertex(7)
        .with_instances(0..3);
    match range {
        DrawRange::Indexed {
            indices,
            base_vertex,
            instances,
        } => {
            assert_eq!(indices, 0..12);
            assert_eq!(base_vertex, 7);
            assert_eq!(instances, 0..3);
        }
        other => panic!("expected an indexed draw, got {other:?}"),
    }
}

#[test]
fn base_vertex_is_ignored_for_non_indexed_draws() {
    let range = DrawRange::vertices(0..3).with_base_vertex(9);
    assert!(matches!(range, DrawRange::Vertices { .. }));
}

#[test]
fn scissor_rect_builders_compose() {
    let rect = ScissorRect::new(64, 32).at(8, 16);
    assert_eq!(
        rect,
        ScissorRect {
            x: 8,
            y: 16,
            width: 64,
            height: 32
        }
    );
    assert_eq!(ScissorRect::new(64, 32), rect.at(0, 0));
}

#[test]
fn viewport_rect_builders_compose() {
    let rect = ViewportRect::new(64.0, 32.0).at(8.5, 16.25);
    assert_eq!(
        rect,
        ViewportRect {
            x: 8.5,
            y: 16.25,
            width: 64.0,
            height: 32.0,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    );
    assert_eq!(
        rect.with_depth(0.5, 1.0).min_depth,
        0.5,
        "the depth range is the caller's to narrow"
    );
}

/// A letterbox keeps the requested aspect and stays inside the target,
/// centred, whichever way the target and the aspect differ.
#[test]
fn fitting_an_aspect_letterboxes_within_the_target() {
    // A 4:3 target is the aspect itself: no bars, no offset.
    assert_eq!(
        ViewportRect::fit_aspect(4.0 / 3.0, 800, 600),
        ViewportRect::new(800.0, 600.0)
    );

    // A wider target gets bars at the sides: the height is the constraint,
    // and the rectangle is centred horizontally.
    let wide = ViewportRect::fit_aspect(4.0 / 3.0, 1600, 600);
    assert_eq!(wide, ViewportRect::new(800.0, 600.0).at(400.0, 0.0));
    assert!((wide.width / wide.height - 4.0 / 3.0).abs() < 1e-5);
    assert_eq!(wide.x + wide.width + wide.x, 1600.0, "centred horizontally");
    assert_eq!(wide.y, 0.0);

    // A narrower target gets bars above and below, centred vertically.
    let tall = ViewportRect::fit_aspect(4.0 / 3.0, 600, 900);
    assert_eq!(tall, ViewportRect::new(600.0, 450.0).at(0.0, 225.0));
    assert!((tall.width / tall.height - 4.0 / 3.0).abs() < 1e-5);
    assert_eq!(tall.y + tall.height + tall.y, 900.0, "centred vertically");
    assert_eq!(tall.x, 0.0);

    // The depth range is left at the whole of clip space.
    assert_eq!((tall.min_depth, tall.max_depth), (0.0, 1.0));
}

/// A target that cannot hold a letterbox — a degenerate one, or an aspect
/// that is not a positive number — comes back whole rather than empty.
#[test]
fn an_impossible_letterbox_is_the_whole_target() {
    for aspect in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            ViewportRect::fit_aspect(aspect, 320, 240),
            ViewportRect::new(320.0, 240.0),
            "aspect {aspect} has no rectangle to fit"
        );
    }
    for target in [(0, 0), (0, 240), (320, 0)] {
        let fitted = ViewportRect::fit_aspect(4.0 / 3.0, target.0, target.1);
        assert!(fitted.width >= 0.0 && fitted.height >= 0.0);
        assert!(fitted.width.is_finite() && fitted.height.is_finite());
    }
}

/// A scene's own viewport is recorded, and a scene that states none leaves
/// the pass's region to whoever set it.
#[test]
fn a_scene_records_only_the_viewport_it_states() {
    let own = ViewportRect::new(320.0, 180.0).at(0.0, 20.0);
    let scene = Scene {
        viewport: Some(own),
        ..Scene::default()
    };
    assert_eq!(scene.viewport, Some(own));

    // A scene that never states one issues no viewport call of its own: the
    // pass keeps whatever region it holds.
    let plain = Scene::new();
    assert_eq!(plain.viewport, None);
    assert_eq!(plain.resolved_viewport(None), None);
}

/// A fallback region stands in only for a scene that states no viewport of
/// its own, which is what keeps one scene's letterbox from cropping the
/// next one's.
#[test]
fn a_fallback_viewport_fills_in_for_a_scene_that_states_none() {
    let whole = ViewportRect::new(800.0, 600.0);
    let own = ViewportRect::new(320.0, 180.0).at(0.0, 20.0);

    let plain = Scene::new();
    assert_eq!(plain.resolved_viewport(Some(whole)), Some(whole));

    // The scene's own region wins over the fallback rather than the two
    // being combined.
    let letterboxed = Scene {
        viewport: Some(own),
        ..Scene::default()
    };
    assert_eq!(letterboxed.resolved_viewport(Some(whole)), Some(own));

    // With no fallback either, there is nothing to set.
    assert_eq!(letterboxed.resolved_viewport(None), Some(own));
}

/// Setting the same viewport twice records one call, and a later scene
/// that states a different one records again rather than inheriting it.
#[test]
fn viewport_calls_are_deduplicated() {
    let mut pass = MockPass::default();
    let mut state = PassState::default();
    let first = ViewportRect::new(320.0, 180.0);
    let second = ViewportRect::new(160.0, 90.0);

    state.set_viewport(&mut pass, first);
    // The same rectangle again is a no-op: the pass already has it.
    state.set_viewport(&mut pass, first);
    // A fractional difference is a real one: a centred letterbox rarely
    // lands on a pixel boundary.
    let nudged = ViewportRect::new(320.0, 180.0).at(0.5, 0.5);
    state.set_viewport(&mut pass, nudged);
    state.set_viewport(&mut pass, second);

    assert_eq!(pass.viewports, vec![first, nudged, second]);
    assert_eq!(state.viewport, Some(second));
}

/// The builders thread through a draw unchanged, so a draw keeps every
/// bound resource while adding the scissor.
#[test]
fn draw_builders_keep_the_scissor() {
    let (device, _queue) = crate::util::test::noop_device();
    let pipeline = noop_pipeline(&device);
    let draw =
        DrawEntry::new(&pipeline, DrawRange::indexed(0..6)).with_scissor(ScissorRect::new(8, 8));
    assert_eq!(draw.scissor, Some(ScissorRect::new(8, 8)));
    assert!(matches!(
        draw.range,
        DrawRange::Indexed { ref indices, .. } if *indices == (0..6)
    ));

    // A draw that never sets one leaves the pass's rectangle alone.
    let plain = DrawEntry::new(&pipeline, DrawRange::vertices(0..3));
    assert_eq!(plain.scissor, None);
}

/// The stencil reference defaults to zero and the builder sets it without
/// disturbing the rest of the draw.
#[test]
fn stencil_reference_builder_composes() {
    let (device, _queue) = crate::util::test::noop_device();
    let pipeline = noop_pipeline(&device);
    let mut draw =
        DrawEntry::new(&pipeline, DrawRange::indexed(0..6)).with_scissor(ScissorRect::new(8, 8));
    draw.stencil_reference = 3;
    assert_eq!(draw.stencil_reference, 3);
    assert_eq!(draw.scissor, Some(ScissorRect::new(8, 8)));

    let plain = DrawEntry::new(&pipeline, DrawRange::vertices(0..3));
    assert_eq!(plain.stencil_reference, 0);
}

/// A mock [`RenderPassInterface`] that records the calls it receives, so a
/// test can assert exactly which `set_*` calls `PassState` skipped.
/// Recording needs no device and no render pass of its own.
#[derive(Default)]
struct MockPass {
    bind_groups: Vec<u32>,
    vertex_buffers: Vec<u32>,
    index_buffers: Vec<wgpu::IndexFormat>,
    scissors: Vec<ScissorRect>,
    viewports: Vec<ViewportRect>,
    stencil_references: Vec<u32>,
    draws: Vec<(Range<u32>, Range<u32>)>,
    indexed_draws: Vec<(Range<u32>, i32, Range<u32>)>,
}

impl<'a> RenderPassInterface<'a> for MockPass {
    fn set_bind_group(&mut self, index: u32, _bind_group: &'a wgpu::BindGroup) {
        self.bind_groups.push(index);
    }

    fn set_vertex_buffer(&mut self, slot: u32, _buffer: &'a wgpu::Buffer, _range: Range<u64>) {
        self.vertex_buffers.push(slot);
    }

    fn set_index_buffer(
        &mut self,
        _buffer: &'a wgpu::Buffer,
        _range: Range<u64>,
        format: wgpu::IndexFormat,
    ) {
        self.index_buffers.push(format);
    }

    fn set_scissor_rect(&mut self, scissor: ScissorRect) {
        self.scissors.push(scissor);
    }

    fn set_viewport_rect(&mut self, viewport: ViewportRect) {
        self.viewports.push(viewport);
    }

    fn set_stencil_reference(&mut self, reference: u32) {
        self.stencil_references.push(reference);
    }

    fn set_pipeline(&mut self, _pipeline: &'a wgpu::RenderPipeline) {}

    fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>) {
        self.draws.push((vertices, instances));
    }

    fn draw_indexed(&mut self, indices: Range<u32>, base_vertex: i32, instances: Range<u32>) {
        self.indexed_draws.push((indices, base_vertex, instances));
    }
}

/// A repeated scissor rectangle is recorded once; a different one after it
/// again — so a draw never silently inherits a stale clip.
#[test]
fn scissor_calls_are_deduplicated() {
    let mut pass = MockPass::default();
    let mut state = PassState::default();
    let first = ScissorRect::new(16, 8).at(4, 2);
    let second = ScissorRect::new(16, 8).at(0, 0);

    state.set_scissor(&mut pass, first);
    // The same rectangle again is a no-op: the pass already has it.
    state.set_scissor(&mut pass, first);
    state.set_scissor(&mut pass, second);

    assert_eq!(pass.scissors, vec![first, second]);
    assert_eq!(state.scissor, Some(second));
}

/// The stencil reference is recorded once per distinct value, so a value
/// an earlier draw left behind never leaks into one that names none.
#[test]
fn stencil_reference_calls_are_deduplicated() {
    let mut pass = MockPass::default();
    let mut state = PassState::default();

    state.set_stencil_reference(&mut pass, 3);
    // The same value again is a no-op: the pass already holds it.
    state.set_stencil_reference(&mut pass, 3);
    // A later draw that names none resets it to zero.
    state.set_stencil_reference(&mut pass, 0);

    assert_eq!(pass.stencil_references, vec![3, 0]);
    assert_eq!(state.stencil_reference, 0);
}

/// Bind groups, vertex buffers and index buffers are recorded once per
/// distinct target too. The handles are real — identity is what the
/// dedup compares — but the pass is the mock, so the calls are visible.
#[test]
fn bound_resources_are_deduplicated() {
    let (device, _queue) = crate::util::test::noop_device();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test::buffer"),
        size: 64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX | wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("test::layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(buffer.size()),
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("test::bind_group"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });

    let mut pass = MockPass::default();
    let mut state = PassState::default();
    let whole = 0..buffer.size();
    let head = 0..32;
    let tail = 32..64;

    state.set_bind_group(&mut pass, 0, &bind_group);
    state.set_bind_group(&mut pass, 0, &bind_group);
    state.set_bind_group(&mut pass, 3, &bind_group);

    state.set_vertex_buffer(&mut pass, 0, &buffer, whole.clone());
    state.set_vertex_buffer(&mut pass, 0, &buffer, whole.clone());
    // The same buffer under a different range is a different binding, so
    // the call is not skipped.
    state.set_vertex_buffer(&mut pass, 0, &buffer, head.clone());
    state.set_vertex_buffer(&mut pass, 2, &buffer, whole.clone());

    state.set_index_buffer(&mut pass, &buffer, head.clone(), wgpu::IndexFormat::Uint16);
    state.set_index_buffer(&mut pass, &buffer, head.clone(), wgpu::IndexFormat::Uint16);
    // A different range and a different format each re-bind.
    state.set_index_buffer(&mut pass, &buffer, tail.clone(), wgpu::IndexFormat::Uint16);
    state.set_index_buffer(&mut pass, &buffer, tail.clone(), wgpu::IndexFormat::Uint32);

    assert_eq!(pass.bind_groups, vec![0, 3]);
    // Slot 0 is bound whole, skipped as a repeat, re-bound for the
    // narrower range, then slot 2 is bound for the first time.
    assert_eq!(pass.vertex_buffers, vec![0, 0, 2]);
    assert_eq!(
        pass.index_buffers,
        vec![
            wgpu::IndexFormat::Uint16,
            wgpu::IndexFormat::Uint16,
            wgpu::IndexFormat::Uint32
        ]
    );
}

#[test]
fn pass_state_capacity_covers_the_wgpu_defaults() {
    // The fixed arrays must have room for everything a default device
    // accepts, otherwise the recording asserts would fire on valid scenes.
    assert!(MAX_BIND_GROUPS >= wgpu::Limits::default().max_bind_groups as usize);
    assert!(MAX_VERTEX_BUFFERS >= wgpu::Limits::default().max_vertex_buffers as usize);
}

/// The composite draws the sequenced draws of every scene first, then the
/// depth-sorted ones of every scene pooled together back-to-front, then
/// the trailing ones — which is what lets a blended stroke from one scene
/// sort against a blended pane from another instead of always painting
/// over it.
#[test]
fn a_composite_orders_before_sorted_and_after() {
    let (device, _queue) = crate::util::test::noop_device();
    let pipeline = noop_pipeline(&device);
    let draw = |range: DrawRange| DrawEntry::new(&pipeline, range);

    // Meshes: the opaque run, then a z-sorted pane far away and one near.
    let meshes = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(0..1)),
            draw(DrawRange::vertices(0..4).with_instances(1..2)).with_order(DrawOrder::Sorted(7.0)),
            draw(DrawRange::vertices(0..4).with_instances(2..3)).with_order(DrawOrder::Sorted(2.0)),
        ],
        ..Scene::default()
    };
    // Lines: an opaque stroke, then a blended one between the panes.
    let lines = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(3..4)),
            draw(DrawRange::vertices(0..4).with_instances(4..5)).with_order(DrawOrder::Sorted(4.0)),
        ],
        ..Scene::default()
    };
    // UI: a trailing widget.
    let ui = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(5..6)).with_order(DrawOrder::After),
        ],
        ..Scene::default()
    };

    let mut pass = MockPass::default();
    record_composited_into(
        &mut pass,
        &mut PassState::default(),
        &[&meshes, &lines, &ui],
        None,
    );

    // Opaque meshes, opaque line, then panes and the stroke farthest
    // first, then the widget.
    assert_eq!(
        pass.draws,
        vec![
            (0..4, 0..1),
            (0..4, 3..4),
            (0..4, 1..2),
            (0..4, 4..5),
            (0..4, 2..3),
            (0..4, 5..6),
        ]
    );
}

/// The sort is stable: sorted draws that tie on depth keep the order
/// their scenes were listed in, and the sequenced and trailing tiers keep
/// scene order around the pool.
#[test]
fn a_composite_keeps_the_order_of_equal_draws() {
    let (device, _queue) = crate::util::test::noop_device();
    let pipeline = noop_pipeline(&device);
    let draw = |range: DrawRange| DrawEntry::new(&pipeline, range);

    let meshes = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(0..1)).with_order(DrawOrder::Sorted(3.0)),
            draw(DrawRange::vertices(0..4).with_instances(1..2)),
        ],
        ..Scene::default()
    };
    let lines = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(2..3)).with_order(DrawOrder::Sorted(3.0)),
            draw(DrawRange::vertices(0..4).with_instances(3..4)).with_order(DrawOrder::After),
        ],
        ..Scene::default()
    };

    let mut pass = MockPass::default();
    record_composited_into(
        &mut pass,
        &mut PassState::default(),
        &[&meshes, &lines],
        None,
    );

    assert_eq!(
        pass.draws,
        vec![(0..4, 1..2), (0..4, 0..1), (0..4, 2..3), (0..4, 3..4)]
    );
}

/// A scene that states no order keeps its place: with every draw
/// sequenced, the composite is the draws in scene order, and no depth is
/// ever compared.
#[test]
fn a_composite_without_sorted_draws_keeps_scene_order() {
    let (device, _queue) = crate::util::test::noop_device();
    let pipeline = noop_pipeline(&device);
    let draw = |range: DrawRange| DrawEntry::new(&pipeline, range);

    // Listed back to front on purpose: nothing here joins the pool, so
    // nothing may be reordered.
    let lines = Scene {
        draws: vec![draw(DrawRange::vertices(0..4).with_instances(0..1))],
        ..Scene::default()
    };
    let meshes = Scene {
        draws: vec![draw(DrawRange::vertices(0..4).with_instances(1..2))],
        ..Scene::default()
    };

    let mut pass = MockPass::default();
    record_composited_into(
        &mut pass,
        &mut PassState::default(),
        &[&lines, &meshes],
        None,
    );

    assert_eq!(pass.draws, vec![(0..4, 0..1), (0..4, 1..2)]);
}

/// Every draw draws into the region its own scene resolves to, so the
/// pool interleaving scenes never moves a draw out of its scene's
/// letterbox; identical regions collapse through the usual dedup.
#[test]
fn a_composite_draws_into_each_scenes_own_region() {
    let (device, _queue) = crate::util::test::noop_device();
    let pipeline = noop_pipeline(&device);
    let draw = |range: DrawRange| DrawEntry::new(&pipeline, range);

    let letter = ViewportRect::new(320.0, 180.0).at(0.0, 20.0);
    let whole = ViewportRect::new(800.0, 600.0);
    let meshes = Scene {
        viewport: Some(letter),
        draws: vec![draw(DrawRange::vertices(0..4).with_instances(0..1))],
    };
    let lines = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(1..2)).with_order(DrawOrder::Sorted(3.0)),
        ],
        ..Scene::default()
    };
    let ui = Scene {
        draws: vec![
            draw(DrawRange::vertices(0..4).with_instances(2..3)).with_order(DrawOrder::After),
        ],
        ..Scene::default()
    };

    let mut pass = MockPass::default();
    record_composited_into(
        &mut pass,
        &mut PassState::default(),
        &[&meshes, &lines, &ui],
        Some(whole),
    );

    assert_eq!(pass.draws.len(), 3);
    // The mesh scene states its own region; the line and UI scenes state
    // none, so they draw into the fallback — stated once, the second
    // being a repeat the pass already holds.
    assert_eq!(pass.viewports, vec![letter, whole]);
}
