//! The tests that go through the resource graph and the depth-less target.

use super::fixture::*;
use super::*;

/// A loaded color attachment keeps its previous contents: the second pass
/// draws nothing, so the frame the first pass left is still there.
///
/// This pins the `LoadOp::Load` path of `begin_pass`, which the other tests
/// never exercise — they all clear.
pub(super) async fn a_loaded_color_attachment_keeps_its_contents() {
    let ctx = Ctx::headless().await;
    // No MSAA: the pass draws straight into the color texture, so a stored
    // frame survives into a second pass that loads.
    let (context, target) = render_target(&ctx, 1);

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
    // Pass one: clear to the test clear color. Draws nothing.
    {
        let mut pass = context.begin_pass(
            &mut encoder,
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass, None);
    }
    // Pass two: load the color (the depth attachment is transient, so it
    // still clears) and draw nothing. If the color load were a clear to the
    // wgpu default (transparent black), the frame would come back empty.
    {
        let mut pass = context.begin_pass(
            &mut encoder,
            wgpu::LoadOp::Load,
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass, None);
    }
    ctx.queue.submit([encoder.finish()]);
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");

    let frame = Frame {
        rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target),
        width: WIDTH,
        height: HEIGHT,
    };
    // The clear color of pass one survived pass two. The readback is the
    // texture's own encoding (sRGB here), not the linear clear value.
    let expected = rgb(CLEAR[0], CLEAR[1], CLEAR[2]);
    let first = frame.pixel_u8(0, 0);
    let encode = |linear: f64| {
        let c = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0) as u8
    };
    // Off-by-one rounding tolerance on each channel.
    let within = |a: u8, b: u8| a.abs_diff(b) <= 1;
    assert!(
        within(first[0], encode(expected.r))
            && within(first[1], encode(expected.g))
            && within(first[2], encode(expected.b))
            && first[3] == 255,
        "a loaded attachment must keep its contents"
    );
}

/// A uniform buffer the graph tests can stand in for any resource kind.
pub(super) fn uniform(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    })
}

pub(super) async fn resource_graph_rebuilds_a_dependent_after_a_resource_change() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use unlit_wgpu::resources::{Rebuild, Resource, ResourceGraph};

    let ctx = Ctx::headless().await;
    let mut graph = ResourceGraph::new();

    let base = graph.insert(uniform(&ctx.device, "test::base"), None);

    // A dependent whose recipe records that it ran: rebuilding is driven
    // purely by the graph.
    let rebuilt = Rc::new(RefCell::new(false));
    let seen = Rc::clone(&rebuilt);
    let device = ctx.device.clone();
    let dependent = graph.insert(
        uniform(&ctx.device, "test::dependent"),
        Some(Rebuild::new({
            let base = base.clone();
            move |graph| {
                assert!(
                    graph.get(&base).is_some(),
                    "the dependency is rebuilt before its dependent"
                );
                *seen.borrow_mut() = true;
                Resource::Buffer(uniform(&device, "test::rebuilt"))
            }
        })),
    );
    graph.add_dependency(&dependent, &base);

    // A no-op pass leaves a clean graph alone, recipes and all.
    graph.maintain();
    assert!(!*rebuilt.borrow());

    // Swapping the base must dirty the dependent, which the next pass then
    // rebuilds in dependency order.
    graph.replace(&base, uniform(&ctx.device, "test::base2"));
    graph.maintain();

    assert!(*rebuilt.borrow());
}

/// A target with no depth attachment must work.
///
/// The pass declares no depth attachment, so every pipeline bound into it must
/// declare no depth state either — `wgpu` rejects the mismatch when the
/// pipeline is bound. A pipeline that kept the base options' depth format
/// therefore could not be drawn into a color-only target at all, which is what
/// a UI-only or overlay-only pass needs.
pub(super) async fn a_color_only_target_draws_a_cube() {
    let ctx = Ctx::headless().await;
    // State the depth-less target the same way a caller would: the surface
    // carries no depth attachment, so the pipeline builds without a
    // depth-stencil state whatever the policy says.
    let mut variant = vertex_color_variant(&ctx);
    variant.surface = SurfaceKey {
        color_format: COLOR_FORMAT,
        depth_stencil_format: None,
        sample_count: 4,
    };
    assert!(
        !fixture(&ctx, &variant, 4).has_depth,
        "so the fixture's pass declares no depth attachment"
    );

    let fixture = fixture(&ctx, &variant, 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0; 4])]);

    // The cube reached the frame: some pixel is brighter than the clear color.
    let clear_sum = ((CLEAR[0] + CLEAR[1] + CLEAR[2]) * 255.0) as u16;
    let covered = frame
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] as u16 + p[1] as u16 + p[2] as u16 > clear_sum + 30)
        .count();
    assert!(
        covered > 0,
        "the cube should reach a target with no depth attachment"
    );
}
