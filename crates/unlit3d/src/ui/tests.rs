use super::*;
use core::cell::{Cell, RefCell};
use std::rc::Rc;
use unlit_ecs::bundle;
use unlit_wgpu::capabilities::DeviceCapabilities;

use crate::input::{
    InputEvent, InputFeed, InputWriter, Modifiers, MouseButton, MouseEvent, TextEvent,
    refresh_input,
};
use crate::pass::PassInfo;
use crate::source::spawn_context;

/// How many times a panel's closure ran, in a sibling component: a panel
/// cannot keep state in itself.
struct Runs(u32);

/// The frame target the tests draw into.
///
/// Nothing here records a pass, so only the target's key and size matter —
/// no attachment has to exist behind them.
/// The target a frame is laid out against in these tests: the same size
/// the source was built for, so its clip rectangles are packed in pixels
/// that agree.
fn test_ui_target() -> UiTarget {
    UiTarget {
        screen: ScreenDescriptor {
            size_in_pixels: [128, 96],
            pixels_per_point: 1.0,
        },
        max_texture_side: 2048,
    }
}

fn test_target() -> PassInfo {
    PassInfo {
        surface: SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 1,
        },
        width: 128,
        height: 96,
        viewport: None,
    }
}

/// A world holding the frame's context, and that context.
fn test_world() -> (World, RenderContext) {
    let mut world = World::new();
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let ctx = spawn_context(
        &mut world,
        device,
        queue,
        unlit_wgpu::resources::ResourceGraph::new(),
        DeviceCapabilities::default(),
    );
    (world, ctx)
}

/// An encoder to record the frame's uploads into.
fn encoder(world: &World, ctx: RenderContext) -> wgpu::CommandEncoder {
    ctx.device(&world.root_scope())
        .expect("the context's device")
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        })
}

/// Every panel in the world runs, once, and a panel may read and write a
/// sibling component while it does.
#[test]
fn each_panel_runs_once_and_may_write_a_sibling() {
    let mut world = World::new();
    let runs = Rc::new(Cell::new(0u32));
    for _ in 0..2 {
        let runs = Rc::clone(&runs);
        world.root_scope_mut().spawn(bundle![
            Runs(0),
            UiPanel::new(move |world, entity, ui| {
                runs.set(runs.get() + 1);
                // Read the sibling, then write it back advanced by one.
                let seen = world
                    .query_one::<&Runs>(entity)
                    .expect("the sibling exists")
                    .get()
                    .0;
                let _ = world.query_one::<&mut Runs>(entity).map(|mut runs| {
                    let runs = runs.get();
                    runs.0 = seen + 1
                });
                ui.label("panel");
            }),
        ]);
    }

    let source = UiSource::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(128.0, 96.0),
        )),
        ..Default::default()
    };
    // egui panics if a texture delta is dropped unapplied; a real frame
    // hands it to the integration, and this test discards it explicitly.
    let mut output = source.run_panels(&world.root_scope(), input);
    output.textures_delta.clear();

    let panels: Vec<Entity> = world
        .root_scope()
        .query::<(&UiPanel, Entity)>()
        .iter()
        .map(|(_ui_panel, entity)| entity)
        .collect();
    assert_eq!(panels.len(), 2, "both panels are driven by one query");
    for entity in panels {
        assert_eq!(
            world
                .root_scope()
                .query_one::<&Runs>(entity)
                .expect("the sibling")
                .get()
                .0,
            1,
            "the panel ran once and wrote its sibling"
        );
    }
    assert_eq!(runs.get(), 2, "one closure call per panel");
}

/// A panel is driven by the source: the source builds its pipeline and
/// turns what the panel painted into draws.
#[test]
fn a_panel_is_driven_by_the_source() {
    let (mut world, ctx) = test_world();
    let target = test_target();
    world
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            // A plain rectangle, so the frame's contents do not depend on the
            // font atlas being ready on the first pass.
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::Pos2::new(8.0, 8.0), egui::Vec2::new(64.0, 32.0)),
                0.0,
                egui::Color32::from_rgb(0, 255, 0),
            );
        }),]);

    let mut source = UiSource::new();
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();
    source.build_scene(&world.root_scope(), ctx, target, &mut scene, &mut encoder);

    assert!(
        !scene.draws.is_empty(),
        "the panel's rectangle became a draw"
    );
}

/// The source is constructed with no arguments and records over the meshes.
#[test]
fn a_fresh_source_declares_the_overlay_order() {
    let source = UiSource::new();
    assert_eq!(source.order(), FrameSourceOrder::UI);
}

/// A panel's queued spawn lands once the frame loop applies the queue.
///
/// A panel gets only a shared world, so a structural change is queued —
/// the same rule every other behaviour component follows.
#[test]
fn a_panel_may_queue_a_spawn() {
    /// The entity a panel asks for.
    struct Spawned;

    let world = World::new();
    let source = UiSource::new();
    let world = Rc::new(RefCell::new(world));
    {
        let world = Rc::clone(&world);
        world
            .borrow_mut()
            .root_scope_mut()
            .spawn(bundle![UiPanel::new(move |world, _entity, ui| {
                if world.query::<&Spawned>().iter().next().is_none() {
                    world.queue().spawn(bundle![Spawned]);
                }
                ui.label("once");
            }),]);
    }

    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(128.0, 96.0),
        )),
        ..Default::default()
    };
    // The panel only queues; nothing exists until the caller applies.
    let mut output = {
        let world = world.borrow();
        source.run_panels(&world.root_scope(), input.clone())
    };
    output.textures_delta.clear();
    assert_eq!(
        world
            .borrow()
            .root_scope()
            .query::<&Spawned>()
            .iter()
            .count(),
        0,
        "a queued spawn has not happened yet"
    );

    world.borrow_mut().apply().unwrap();
    assert_eq!(
        world
            .borrow()
            .root_scope()
            .query::<&Spawned>()
            .iter()
            .count(),
        1,
        "the queue landed on apply"
    );

    // A second frame sees the entity and does not queue another.
    let mut output = source.run_panels(&world.borrow().root_scope(), input);
    output.textures_delta.clear();
    world.borrow_mut().apply().unwrap();
    assert_eq!(
        world
            .borrow()
            .root_scope()
            .query::<&Spawned>()
            .iter()
            .count(),
        1,
        "the panel re-ran and found what it asked for"
    );
}

/// Every pass of a multi-pass layout drives every panel.
///
/// egui calls the closure again when a pass asks for a discard, so the
/// query must be rebuilt per pass rather than iterated once: collecting
/// the entities up front would make the second pass see nothing.
#[test]
fn every_pass_drives_every_panel() {
    let mut world = World::new();
    let runs = Rc::new(Cell::new(0u32));
    for _ in 0..2 {
        let runs = Rc::clone(&runs);
        world
            .root_scope_mut()
            .spawn(bundle![UiPanel::new(move |_world, _entity, _ui| {
                runs.set(runs.get() + 1);
            }),]);
    }

    let source = UiSource::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(128.0, 96.0),
        )),
        ..Default::default()
    };
    // Run the driving closure twice by hand, as a discard does, and count
    // what each pass saw.
    let mut seen = Vec::new();
    for _ in 0..2 {
        let mut output = source.run_panels(&world.root_scope(), input.clone());
        output.textures_delta.clear();
        seen.push(runs.get());
    }

    assert_eq!(
        seen,
        vec![2, 4],
        "each pass drove both panels again, so the query was rebuilt"
    );
}

/// A caller can select a subset of the panels by filtering.
///
/// Panels are ordinary components, so a second kind of panel is a second
/// component and a caller can be selective without the source knowing
/// either type. A query filter fetches no column, so selecting costs no
/// borrow.
#[test]
fn a_filter_selects_which_panels_a_caller_collects() {
    /// Marks a panel as belonging to one layer.
    struct Foreground;

    let mut world = World::new();
    world
        .root_scope_mut()
        .spawn(bundle![Foreground, UiPanel::new(|_world, _entity, _ui| {})]);
    world
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, _ui| {})]);

    let foreground: Vec<Entity> = world
        .root_scope()
        .query::<Entity>()
        .by(|archetype: &unlit_ecs::Archetype| archetype.has::<Foreground>())
        .iter()
        .collect();
    assert_eq!(foreground.len(), 1, "the filter picked one of two panels");

    let all: Vec<Entity> = world
        .root_scope()
        .query::<(&UiPanel, Entity)>()
        .iter()
        .map(|(_ui_panel, e)| e)
        .collect();
    assert_eq!(all.len(), 2, "the unfiltered query sees both");
    assert!(
        all.contains(&foreground[0]),
        "the filtered panel is one of them"
    );
}

/// The frame's events reach the panels.
///
/// A panel that records the events egui gave it proves the world's
/// `InputEvents` were read and translated, not merely looked up.
#[test]
fn the_frame_events_reach_the_panels() {
    let (mut world, ctx) = test_world();
    let target = test_target();

    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&seen);
    world
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(move |_world, _entity, ui| {
            // egui's own input state is what a widget reads; the raw events are
            // not exposed to a panel, so this asserts on the effect instead: a
            // typed character must land in the text a focused field sees.
            ui.text_edit_singleline(&mut String::new());
            let pressed = ui.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });
            recorded.borrow_mut().extend(pressed);
        }),]);

    // The pointer and a typed key arrive in the world's own event types.
    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(InputEvent::Resized { size_px: (128, 96) });
    refresh_input(&world);

    let mut source = UiSource::new();
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();
    // egui only lays a text field out once it knows its fonts, so the
    // first frame establishes them; the text is then this tick's batch.
    source.build_scene(&world.root_scope(), ctx, target, &mut scene, &mut encoder);
    input.push(InputEvent::Text(TextEvent("hi".to_string())));
    refresh_input(&world);
    source.build_scene(&world.root_scope(), ctx, target, &mut scene, &mut encoder);

    assert_eq!(
        &*seen.borrow(),
        &["hi".to_string()],
        "the frame's text event reached the panel's egui input, once"
    );
}

/// A mouse click becomes a click a panel can see.
///
/// The whole chain is exercised: the world's event, its translation, and
/// egui's own hit testing on a widget the panel painted.
#[test]
fn a_mouse_click_reaches_a_widget() {
    /// Where the button is, in points.
    fn button() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::new(8.0, 8.0), egui::Vec2::new(48.0, 24.0))
    }
    let clicked = Rc::new(Cell::new(false));

    let (mut world, ctx) = test_world();
    let target = test_target();
    let flag = Rc::clone(&clicked);
    world
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(move |_world, _entity, ui| {
            // A real widget, so the click has to come through egui's own hit
            // testing rather than a hand-rolled rectangle test.
            let response = ui.allocate_rect(button(), egui::Sense::click());
            if response.clicked() {
                flag.set(true);
            }
        }),]);

    let input = InputWriter::new();
    world.root_scope_mut().spawn(InputFeed(input.clone()));
    input.push(InputEvent::Resized { size_px: (128, 96) });
    refresh_input(&world);

    let mut source = UiSource::new();
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();
    // The first frame lays the widget out and knows its fonts; nothing is
    // clicked yet because no input has arrived.
    source.build_scene(&world.root_scope(), ctx, target, &mut scene, &mut encoder);
    assert!(!clicked.get(), "nothing is clicked before any input");

    // The pointer arrives over the button and presses and releases there,
    // in *physical* pixels at a density of one, so points and pixels
    // coincide. A click is complete within the frame egui sees it, which
    // is how a fast click looks to a frame loop.
    let centre = [button().center().x, button().center().y];
    for event in [
        MouseEvent::Moved { position: centre },
        MouseEvent::Button {
            position: centre,
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        },
        MouseEvent::Button {
            position: centre,
            button: MouseButton::Primary,
            pressed: false,
            modifiers: Modifiers::default(),
        },
    ] {
        input.push(InputEvent::Mouse(event));
    }
    // The tick's batch: what the pointer did since the last frame.
    refresh_input(&world);
    source.build_scene(&world.root_scope(), ctx, target, &mut scene, &mut encoder);

    assert!(
        clicked.get(),
        "a press and release inside a widget must click it"
    );
}

/// A published frame is drawn on every frame it is the newest one, not
/// just the frame it arrived: a settled UI is neither laid out nor
/// published again, so the drawing side has to keep what it was given.
#[test]
fn a_published_frame_is_drawn_until_a_newer_one_arrives() {
    let (world, ctx) = test_world();
    let info = test_target();
    // Laid out in a context of its own, the way a publishing world would:
    // its texture ids start where every context's do.
    let mut other = World::new();
    other
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                0.0,
                egui::Color32::RED,
            );
        }),]);
    let target = test_ui_target();
    let frame = lay_out(
        &other,
        &egui::Context::default(),
        raw_input(target, Some(0.0), true, Vec::new()),
        target.screen,
    );

    let mut source = UiSource::new();
    source.frames.publish(0, frame);
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();

    source.build_scene(&world.root_scope(), ctx, info, &mut scene, &mut encoder);
    assert!(
        !scene.draws.is_empty(),
        "the published frame is drawn when it arrives"
    );

    // The publisher has nothing new, and the frame it published must not
    // have been emptied by drawing it.
    source.build_scene(&world.root_scope(), ctx, info, &mut scene, &mut encoder);
    assert!(
        !scene.draws.is_empty(),
        "the same frame is drawn again while no newer one arrives"
    );
}
/// A publisher that lays frames out faster than the drawing side draws
/// them replaces its own slot before the frame is ever drawn, and egui
/// sends a texture once — on the frame that first writes it. The displaced
/// frame therefore hands its updates over; dropping them would leave every
/// later draw with no material to bind.
#[test]
fn a_displaced_frame_hands_its_textures_to_its_successor() {
    let (world, ctx) = test_world();
    let info = test_target();
    // One context, laid out twice: the second pass writes no texture, and
    // is what a publisher outrunning the drawing side publishes over the
    // first.
    let context = egui::Context::default();
    let mut other = World::new();
    other
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            ui.label("a panel with text");
        }),]);
    let target = test_ui_target();
    let run = |time: f64| {
        lay_out(
            &other,
            &context,
            raw_input(target, Some(time), true, Vec::new()),
            target.screen,
        )
    };
    let first = run(0.0);
    let second = run(1.0);
    assert!(
        !first.textures.is_empty(),
        "the first pass writes the font atlas"
    );
    assert!(
        second.textures.is_empty(),
        "the second pass writes nothing new"
    );

    let mut source = UiSource::new();
    source.frames.publish(0, first);
    source.frames.publish(0, second);
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();
    source.build_scene(&world.root_scope(), ctx, info, &mut scene, &mut encoder);
    assert!(
        !scene.draws.is_empty(),
        "the frame that survived carries the atlas its predecessor wrote"
    );
}
/// A frame laid out at another size is not drawn, because its clip
/// rectangles belong to pixels the pass does not have — but its texture
/// updates still go through. egui sends a texture once, on the frame that
/// first writes it, so losing them at a resize would leave the slot with
/// nothing to sample once its publisher had caught up with the new size.
#[test]
fn a_frame_laid_out_at_another_size_hands_its_textures_over() {
    let (world, ctx) = test_world();
    let target = test_ui_target();
    let mut other = World::new();
    other
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            ui.label("a panel with text");
        }),]);
    let frame = lay_out(
        &other,
        &egui::Context::default(),
        raw_input(target, Some(0.0), true, Vec::new()),
        target.screen,
    );
    assert!(
        !frame.textures.is_empty(),
        "the first pass writes the font atlas"
    );

    let mut source = UiSource::new();
    source.frames.publish(0, frame);
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();

    // A target of another size: the frame contributes no geometry, and its
    // updates are taken anyway.
    let [width, height] = target.screen.size_in_pixels;
    let resized = PassInfo {
        width: 2 * width,
        height: 2 * height,
        ..test_target()
    };
    source.build_scene(&world.root_scope(), ctx, resized, &mut scene, &mut encoder);
    assert!(
        scene.draws.is_empty(),
        "a frame laid out at another size is not drawn"
    );
    source.frames.with_frames(|slots| {
        let frame = slots[0].as_ref().expect("the frame stays in its slot");
        assert!(
            frame.textures.is_empty(),
            "its updates were taken and uploaded"
        );
    });

    // Back at the size it was laid out for, the same frame draws: the
    // atlas it wrote is there because the other-size pass uploaded it.
    source.build_scene(
        &world.root_scope(),
        ctx,
        test_target(),
        &mut scene,
        &mut encoder,
    );
    assert!(
        !scene.draws.is_empty(),
        "its textures were not lost to the pass that could not draw it"
    );
}
/// Two contexts allocate texture ids from zero, so the same id names
/// different images in different worlds. Keys carry the origin, so a
/// publisher's atlas and the drawing world's own do not collide.
#[test]
fn two_contexts_keep_their_textures_apart() {
    let target = test_ui_target();

    // A separate world, with a context whose ids start at zero like any
    // other, paints the frame that gets published.
    let mut other = World::new();
    other
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                0.0,
                egui::Color32::RED,
            );
        }),]);
    let frame = lay_out(
        &other,
        &egui::Context::default(),
        raw_input(target, Some(0.0), true, Vec::new()),
        target.screen,
    );
    let ids = frame
        .primitives
        .iter()
        .filter_map(|primitive| match &primitive.primitive {
            egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!ids.is_empty(), "the frame names a texture");

    // The ids are whatever its context allocated: publishing rewrites
    // nothing, because the drawing side keys by (source, id).
    let mut source = UiSource::new();
    source.frames.publish(0, frame);
    let drawn = source.frames.with_frames(|slots| {
        slots[0]
            .as_ref()
            .expect("published")
            .primitives
            .iter()
            .filter_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(ids, drawn, "publishing leaves the ids alone");
}
/// Without an event source the UI still draws, with an idle frame.
#[test]
fn a_world_without_input_state_draws_an_idle_frame() {
    let (mut world, ctx) = test_world();
    let target = test_target();
    world
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|_world, _entity, ui| {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16.0)),
                0.0,
                egui::Color32::RED,
            );
        }),]);

    let mut source = UiSource::new();
    let mut encoder = encoder(&world, ctx);
    let mut scene = Scene::new();
    source.build_scene(&world.root_scope(), ctx, target, &mut scene, &mut encoder);
    assert!(
        !scene.draws.is_empty(),
        "a panel draws with no input in the world"
    );
}
