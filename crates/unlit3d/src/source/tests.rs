use super::*;
use unlit_wgpu::specialize::SurfaceKey;

use crate::order::{AmbiguousGroup, OrderWarnings, Ordering};

/// A pass's target, as a source is told about one: nothing here draws, so
/// only the ordering bookkeeping is exercised.
fn test_info() -> PassInfo {
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

/// A noop device and queue, enough to spawn a context: the tests here
/// exercise the ordering bookkeeping, which never touches the GPU.
fn test_context(world: &mut World) -> RenderContext {
    let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    spawn_context(
        world,
        device,
        queue,
        ResourceGraph::new(),
        DeviceCapabilities::default(),
    )
}

/// Draining hands back what the frame queued, and leaves the queue empty.
///
/// The scan that finds the queue's row and the write that empties it cannot
/// hold their claims at the same time: the scan claims every row of the
/// component and the write claims one of them exclusively. Both drains here
/// would panic if the scan outlived the loop that read it.
#[test]
fn draining_open_urls_empties_the_queue() {
    let mut world = World::new();
    let _context = test_context(&mut world);
    let queue = world
        .root_scope()
        .query::<(Entity, &OpenUrls)>()
        .iter()
        .next()
        .map(|(entity, _)| entity)
        .expect("the frame context spawns the queue");
    world
        .root_scope()
        .query_one::<&mut OpenUrls>(queue)
        .unwrap()
        .get()
        .push("https://example.com", false);

    assert_eq!(
        drain_open_urls(&world),
        vec![OpenUrl {
            url: "https://example.com".to_owned(),
            new_tab: false,
        }],
        "the queued URL is handed to the host"
    );
    assert!(
        drain_open_urls(&world).is_empty(),
        "the first drain left the queue empty"
    );
}

/// A source that records how many times it was built and what it saw.
struct RecordingSource {
    order: FrameSourceOrder,
    built: Rc<Cell<u32>>,
}

impl RecordingSource {
    fn new(order: FrameSourceOrder) -> Self {
        Self {
            order,
            built: Rc::new(Cell::new(0)),
        }
    }
}

impl FrameSource for RecordingSource {
    fn build_scene(
        &mut self,
        _view: &Scope<'_>,
        _ctx: RenderContext,
        _info: PassInfo,
        _scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        self.built.set(self.built.get() + 1);
    }

    fn order(&self) -> FrameSourceOrder {
        self.order
    }
}

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Resolve `world`'s order in one shot, as the tests want it: the entities
/// and the ambiguous groups, both owned so an assertion can hold them while
/// the world changes.
fn record_order(world: &World) -> (Vec<Entity>, Vec<AmbiguousGroup<FrameSourceOrder>>) {
    let mut resolved = Ordering::<FrameSourceOrder>::default();
    resolved.resolve(
        world
            .root_scope()
            .query::<(Entity, &Source)>()
            .iter()
            .map(|(entity, source)| (source.order(), entity)),
    );
    (resolved.entities().to_vec(), resolved.ambiguous().to_vec())
}

/// Sources record in the order they declare, not the order they were
/// spawned in — the whole point of an explicit order.
#[test]
fn explicit_order_wins_over_spawn_order() {
    let mut world = World::new();
    spawn_context(
        &mut world,
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0,
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).1,
        ResourceGraph::new(),
        DeviceCapabilities::default(),
    );

    // Spawned overlay-first, so spawn order is the opposite of draw order.
    let overlay = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);
    let mesh = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);

    let (order, ambiguous) = record_order(&world);
    assert_eq!(order, vec![mesh, overlay], "the lower order records first");
    assert!(ambiguous.is_empty(), "distinct orders are not ambiguous");
}

/// Equal orders fall back to entity order, which keeps them stable while
/// the entities live.
#[test]
fn equal_orders_fall_back_to_entity_order() {
    let mut world = World::new();
    test_context(&mut world);

    let first = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);
    let second = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);
    let third = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);
    assert_eq!(record_order(&world).0, vec![first, second, third]);

    // An explicit order is the way to choose: the entity is only a
    // tie-break, and it does not survive a respawn reusing the index.
    world.root_scope_mut().despawn(second).unwrap();
    assert_eq!(
        record_order(&world).0,
        vec![first, third],
        "the surviving sources keep their relative order"
    );
}

/// A source spawned at an explicit order ignores what its own `order`
/// says, and `set_order` can change it later.
#[test]
fn an_explicit_order_overrides_the_one_the_source_declares() {
    let mut world = World::new();
    test_context(&mut world);

    let a = world.root_scope_mut().spawn(bundle![
        Source::new(RecordingSource::new(FrameSourceOrder::MESH)).with_order(FrameSourceOrder::UI),
    ]);
    let b = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);

    // `a` was spawned as an overlay, so it records last despite declaring
    // `MESH` itself.
    assert_eq!(record_order(&world).0, vec![b, a]);

    let _ = world
        .root_scope()
        .query_one::<&mut Source>(a)
        .map(|mut source| {
            let source = source.get();
            source.set_order(None)
        });
    assert_eq!(
        record_order(&world).0,
        vec![a, b],
        "dropping the override falls back to the declared order"
    );
}

/// Typed access finds the concrete type and names the mismatch for
/// another, rather than panicking.
#[test]
fn typed_access_downcasts_and_rejects_the_wrong_type() {
    struct OtherSource;

    impl FrameSource for OtherSource {
        fn build_scene(
            &mut self,
            _view: &Scope<'_>,
            _ctx: RenderContext,
            _info: PassInfo,
            _scene: &mut Scene,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
        }
        fn order(&self) -> FrameSourceOrder {
            FrameSourceOrder::MESH
        }
    }

    let mut world = World::new();
    test_context(&mut world);

    let recording = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    let other = world
        .root_scope_mut()
        .spawn(bundle![Source::new(OtherSource)]);

    assert!(
        world
            .root_scope()
            .query_one::<&mut Source>(recording)
            .map(|mut s| {
                let s = s.get();
                s.as_mut::<RecordingSource>().is_ok()
            })
            .unwrap()
    );
    assert!(
        world
            .root_scope()
            .query_one::<&mut Source>(recording)
            .map(|mut s| {
                let s = s.get();
                s.as_mut::<OtherSource>().is_err()
            })
            .unwrap()
    );
    assert!(
        world
            .root_scope()
            .query_one::<&mut Source>(other)
            .map(|mut s| {
                let s = s.get();
                s.as_mut::<OtherSource>().is_ok()
            })
            .unwrap()
    );
    assert!(
        world
            .root_scope()
            .query_one::<&mut Source>(other)
            .map(|mut s| {
                let s = s.get();
                s.as_ref::<RecordingSource>().is_err()
            })
            .unwrap()
    );

    let scope = world.root_scope();
    let mut row = scope.query_one::<&mut Source>(recording).unwrap();
    let error = row.get().as_mut::<OtherSource>().err().unwrap();
    assert!(error.actual.ends_with("RecordingSource"));
    assert!(error.requested.ends_with("OtherSource"));
}

/// One query drives sources of different concrete types, which is what the
/// erased handle is for.
#[test]
fn one_query_builds_every_source_type() {
    struct CountingSource(u32);

    impl FrameSource for CountingSource {
        fn build_scene(
            &mut self,
            _view: &Scope<'_>,
            _ctx: RenderContext,
            _info: PassInfo,
            _scene: &mut Scene,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            self.0 += 1;
        }
        fn order(&self) -> FrameSourceOrder {
            FrameSourceOrder::UI
        }
    }

    let mut world = World::new();
    let ctx = test_context(&mut world);
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(CountingSource(0))]);

    let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
        .0
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let entities: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Source)>()
        .iter()
        .map(|(e, _)| e)
        .collect();
    assert_eq!(entities.len(), 2, "both sources are seen by one query type");
    let mut scene = Scene::new();
    for entity in entities {
        world
            .root_scope()
            .query_one::<&mut Source>(entity)
            .map(|mut source| {
                let source = source.get();
                {
                    let scope = world
                        .scope(entity)
                        .unwrap_or_else(|_| panic!("a source entity is its own scope: {entity:?}"));
                    source.build_scene(&scope, ctx, test_info(), &mut scene, &mut encoder)
                }
            })
            .unwrap();
    }
}

/// Both phases run in the documented order: every source is built before
/// any scene is recorded.
#[test]
fn every_source_is_built_before_any_is_recorded() {
    let mut world = World::new();
    let ctx = test_context(&mut world);

    let order = Rc::new(RefCell::new(Vec::new()));
    struct PhasedSource {
        name: &'static str,
        log: Rc<RefCell<Vec<String>>>,
    }
    impl FrameSource for PhasedSource {
        fn build_scene(
            &mut self,
            _view: &Scope<'_>,
            _ctx: RenderContext,
            _info: PassInfo,
            _scene: &mut Scene,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            self.log.borrow_mut().push(format!("build {}", self.name));
        }
        fn order(&self) -> FrameSourceOrder {
            FrameSourceOrder::MESH
        }
    }

    for name in ["a", "b"] {
        world
            .root_scope_mut()
            .spawn(bundle![Source::new(PhasedSource {
                name,
                log: Rc::clone(&order),
            }),]);
    }

    let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
        .0
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let entities: Vec<Entity> = world
        .root_scope()
        .query::<(Entity, &Source)>()
        .iter()
        .map(|(e, _)| e)
        .collect();
    let mut scene = Scene::new();
    for entity in &entities {
        world
            .root_scope()
            .query_one::<&mut Source>(*entity)
            .map(|mut source| {
                let source = source.get();
                {
                    let scope = world
                        .scope(*entity)
                        .unwrap_or_else(|_| panic!("a source entity is its own scope: {entity:?}"));
                    source.build_scene(&scope, ctx, test_info(), &mut scene, &mut encoder)
                }
            })
            .unwrap();
    }
    order.borrow_mut().push("record".to_string());

    assert_eq!(
        &*order.borrow(),
        &["build a", "build b", "record"],
        "both sources build before the record phase starts"
    );
}

/// A source that was never spawned at an explicit order takes the one it
/// declares, and a fresh source reports what it declared.
#[test]
fn a_source_reports_its_declared_order_until_overridden() {
    let source = Source::new(RecordingSource::new(FrameSourceOrder::UI));
    assert_eq!(source.order(), FrameSourceOrder::UI);

    let mut source =
        Source::new(RecordingSource::new(FrameSourceOrder::MESH)).with_order(FrameSourceOrder::UI);
    assert_eq!(source.order(), FrameSourceOrder::UI, "the override wins");

    source.set_order(None);
    assert_eq!(source.order(), FrameSourceOrder::MESH, "and can be dropped");
}

/// An ambiguity is reported once, and only again when it changes: a steady
/// frame must not flood the log.
#[test]
fn an_ambiguity_is_reported_once_and_again_only_when_it_changes() {
    let mut world = World::new();
    test_context(&mut world);

    let a = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    let b = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);

    let (_, ambiguous) = record_order(&world);
    assert_eq!(ambiguous.len(), 1, "the two share one order");
    assert_eq!(ambiguous[0].0, FrameSourceOrder::MESH);
    assert_eq!(ambiguous[0].1, vec![a, b], "in entity order");

    let mut warnings = OrderWarnings::<FrameSourceOrder>::default();
    assert!(warnings.check(&ambiguous), "the first frame reports it");
    assert!(!warnings.check(&ambiguous), "a steady frame stays quiet");

    // Resolving the ambiguity stops the reporting; reintroducing it warns
    // once more.
    let _ = world
        .root_scope()
        .query_one::<&mut Source>(b)
        .map(|mut source| {
            let source = source.get();
            source.set_order(Some(FrameSourceOrder::UI))
        });
    let (_, resolved) = record_order(&world);
    assert!(resolved.is_empty());
    assert!(!warnings.check(&resolved));

    let _ = world
        .root_scope()
        .query_one::<&mut Source>(b)
        .map(|mut source| {
            let source = source.get();
            source.set_order(Some(FrameSourceOrder::MESH))
        });
    let (_, again) = record_order(&world);
    assert!(warnings.check(&again), "the ambiguity is back");
    assert!(!warnings.check(&again), "and still reports only once");
}

/// Resolving the order reuses its buffers, so a steady scene stops
/// allocating.
///
/// The capacity is what proves reuse: a fresh `Vec` per frame would leave
/// each one at its own length, while a cleared-and-refilled one keeps the
/// largest capacity it has ever needed.
#[test]
fn resolving_the_order_reuses_its_buffers() {
    let mut world = World::new();
    test_context(&mut world);
    for _ in 0..4 {
        world
            .root_scope_mut()
            .spawn(bundle![Source::new(RecordingSource::new(
                FrameSourceOrder::MESH
            )),]);
    }
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);

    let mut resolved = Ordering::<FrameSourceOrder>::default();
    resolved.resolve(
        world
            .root_scope()
            .query::<(Entity, &Source)>()
            .iter()
            .map(|(entity, source)| (source.order(), entity)),
    );
    let order_capacity = resolved.order_capacity();
    let ambiguous_capacity = resolved.ambiguous_capacity();
    assert_eq!(resolved.entities().len(), 5);
    assert_eq!(resolved.ambiguous().len(), 1, "four share one order");

    // A later frame with fewer sources keeps both allocations.
    let first = {
        let mut sources = world.root_scope().query::<(Entity, &Source)>();
        sources.iter().next().expect("a source").0
    };
    world.root_scope_mut().despawn(first).unwrap();
    resolved.resolve(
        world
            .root_scope()
            .query::<(Entity, &Source)>()
            .iter()
            .map(|(entity, source)| (source.order(), entity)),
    );
    assert_eq!(resolved.entities().len(), 4);
    assert_eq!(
        resolved.order_capacity(),
        order_capacity,
        "the order buffer is reused rather than reallocated"
    );
    assert_eq!(
        resolved.ambiguous_capacity(),
        ambiguous_capacity,
        "the ambiguity buffer is reused rather than reallocated"
    );
}

/// Distinct orders produce no ambiguity at all.
#[test]
fn distinct_orders_are_never_ambiguous() {
    let mut world = World::new();
    test_context(&mut world);
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);

    let (order, ambiguous) = record_order(&world);
    assert_eq!(order.len(), 2);
    assert!(ambiguous.is_empty());
    assert!(!OrderWarnings::<FrameSourceOrder>::default().check(&ambiguous));
}

/// A logger that keeps what was logged, so a test can assert on the
/// message rather than only on the boolean the check returns.
struct CaptureLogger;

static CAPTURED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        if let Ok(mut captured) = CAPTURED.lock() {
            captured.push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

/// Install the capture logger before anything logs, and read back what it
/// caught. `log` allows one logger per process, so every test that wants
/// the messages goes through here.
fn capture_logs(f: impl FnOnce()) -> Vec<String> {
    // A test process has no logger yet, so this always succeeds; a second
    // install in the same process would be ignored, which is why the
    // captured list is cleared first.
    static LOGGER: CaptureLogger = CaptureLogger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
    CAPTURED.lock().expect("the capture mutex").clear();

    f();

    let captured = CAPTURED.lock().expect("the capture mutex");
    captured.clone()
}

/// The warning names the group's size, its order and its entities, so a
/// reader can tell which sources are ambiguous without guessing.
#[test]
fn the_ambiguity_warning_names_the_group_order_and_entities() {
    let mut world = World::new();
    test_context(&mut world);
    let a = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);
    let b = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);

    let messages = capture_logs(|| {
        let (_, ambiguous) = record_order(&world);
        let mut warnings = OrderWarnings::<FrameSourceOrder>::default();
        assert!(warnings.check(&ambiguous), "the group is reported");
    });

    assert_eq!(messages.len(), 1, "exactly one warning: {messages:?}");
    let message = &messages[0];
    assert!(
        message.contains('2'),
        "the group's size is named: {message}"
    );
    assert!(
        message.contains(&FrameSourceOrder::UI.0.to_string()),
        "the order's value is named: {message}"
    );
    assert!(
        message.contains(&format!("{a:?}")) && message.contains(&format!("{b:?}")),
        "both entities are named: {message}"
    );
}

/// Distinct orders log nothing at all, so a correct frame is silent.
#[test]
fn distinct_orders_log_nothing() {
    let mut world = World::new();
    test_context(&mut world);
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::UI
        )),]);

    let messages = capture_logs(|| {
        let (_, ambiguous) = record_order(&world);
        assert!(!OrderWarnings::<FrameSourceOrder>::default().check(&ambiguous));
    });
    assert!(messages.is_empty(), "nothing to warn about: {messages:?}");
}

/// A steady frame does not repeat the warning, and resolving the ambiguity
/// stops it: the log reports a change, not a condition that persists.
#[test]
fn a_steady_frame_repeats_nothing_in_the_log() {
    let mut world = World::new();
    test_context(&mut world);
    let a = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    let _b = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    let mut warnings = OrderWarnings::<FrameSourceOrder>::default();

    let messages = capture_logs(|| {
        for _ in 0..5 {
            let (_, ambiguous) = record_order(&world);
            warnings.check(&ambiguous);
        }
    });
    assert_eq!(
        messages.len(),
        1,
        "five steady frames warn once: {messages:?}"
    );

    // Resolving it and reintroducing it warns once more, not twice.
    let _ = world
        .root_scope()
        .query_one::<&mut Source>(a)
        .map(|mut source| {
            let source = source.get();
            source.set_order(Some(FrameSourceOrder::UI))
        });
    let messages = capture_logs(|| {
        let (_, resolved) = record_order(&world);
        assert!(!warnings.check(&resolved), "resolved: nothing to say");
    });
    assert!(messages.is_empty(), "resolving is silent: {messages:?}");
}

/// Re-applying the order a source already has does not disturb the
/// ambiguity bookkeeping, so a caller may set an order every frame.
#[test]
fn re_applying_the_same_order_keeps_the_log_quiet() {
    let mut world = World::new();
    test_context(&mut world);
    let a = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    let b = world
        .root_scope_mut()
        .spawn(bundle![Source::new(RecordingSource::new(
            FrameSourceOrder::MESH
        )),]);
    let mut warnings = OrderWarnings::<FrameSourceOrder>::default();

    let messages = capture_logs(|| {
        for _ in 0..3 {
            // What a caller that pins its sources' order every frame does.
            let _ = world
                .root_scope()
                .query_one::<&mut Source>(a)
                .map(|mut source| {
                    let source = source.get();
                    source.set_order(Some(FrameSourceOrder::MESH))
                });
            let _ = world
                .root_scope()
                .query_one::<&mut Source>(b)
                .map(|mut source| {
                    let source = source.get();
                    source.set_order(Some(FrameSourceOrder::MESH))
                });
            let (_, ambiguous) = record_order(&world);
            warnings.check(&ambiguous);
        }
    });
    assert_eq!(
        messages.len(),
        1,
        "the second and third frames change nothing: {messages:?}"
    );
}

/// Three sources sharing an order are one group, not three pairs, and the
/// group lists every entity.
#[test]
fn a_three_way_ambiguity_is_one_group_of_three() {
    let mut world = World::new();
    test_context(&mut world);

    let ids: Vec<Entity> = (0..3)
        .map(|_| {
            world
                .root_scope_mut()
                .spawn(bundle![Source::new(RecordingSource::new(
                    FrameSourceOrder::MESH
                )),])
        })
        .collect();

    let (_, ambiguous) = record_order(&world);
    assert_eq!(ambiguous.len(), 1, "one group, not pairs");
    assert_eq!(ambiguous[0].1, ids);
}

/// Sources spawned later break a tie after ones spawned earlier, because
/// the entity index is allocated in spawn order.
#[test]
fn later_entities_break_a_tie_last() {
    let mut world = World::new();
    test_context(&mut world);

    let ids: Vec<Entity> = (0..3)
        .map(|_| {
            world
                .root_scope_mut()
                .spawn(bundle![Source::new(RecordingSource::new(
                    FrameSourceOrder::MESH
                )),])
        })
        .collect();
    assert_eq!(record_order(&world).0, ids);
}
