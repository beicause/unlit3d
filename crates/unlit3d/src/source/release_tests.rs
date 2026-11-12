use super::*;
use unlit_wgpu::resources::{ResHandle, Resource as GraphResource};

/// A pass's target; these tests never draw, so only the graph matters.
fn test_info() -> PassInfo {
    PassInfo {
        surface: unlit_wgpu::specialize::SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 1,
        },
        width: 128,
        height: 96,
        viewport: None,
    }
}

/// A source that registers a node, so dropping it has something to give up.
///
/// It holds the id in its own state, as a real source does: the node stays
/// alive for as long as the source component does.
struct OwningSource {
    context: RenderContext,
    node: Option<ResHandle>,
}

impl FrameSource for OwningSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        _info: PassInfo,
        _scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        // Register once, on the first frame: a node the source owns and
        // nothing else refers to.
        self.context = ctx;
        if self.node.is_none() {
            self.node = Some(
                scope
                    .query_one::<&mut ResourceGraph>(ctx.graph)
                    .expect("the context's graph exists")
                    .get()
                    .insert(GraphResource::Virtual, None),
            );
        }
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::UI
    }
}

/// A source whose build takes a maintain scope and then returns at once,
/// the way a source with nothing to draw does.
struct EarlyReturnSource {
    context: RenderContext,
    node: Option<ResHandle>,
}

impl FrameSource for EarlyReturnSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        _info: PassInfo,
        _scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        // The scope is the whole of the build's graph handling: this source
        // never calls `maintain`, and the return is what settles the graph.
        let _maintain = ctx.maintain_scope(*scope);
        self.context = ctx;
        if self.node.is_none() {
            self.node = Some(
                scope
                    .query_one::<&mut ResourceGraph>(ctx.graph)
                    .expect("the context's graph exists")
                    .get()
                    .insert(GraphResource::Virtual, None),
            );
        } else {
            // A source with nothing left to draw: it gives its node up and
            // returns, leaving the scope's drop to collect it.
            self.node = None;
        }
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::MESH
    }
}

fn noop_context(world: &mut World) -> RenderContext {
    spawn_context(
        world,
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0,
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).1,
        ResourceGraph::new(),
        DeviceCapabilities::default(),
        DeviceHealth::unwatched(),
    )
}

/// Build the source once, so it registers its node.
///
/// The node's id is deliberately not handed back: an id is a strong
/// reference, so a test holding one would keep the very node it wants to
/// watch the graph collect.
fn build_once(world: &mut World, entity: Entity, ctx: RenderContext) {
    let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
        .0
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let mut scene = Scene::new();
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
        .unwrap_or_else(|_| panic!("the source entity exists: {entity:?}"));
}

/// How many nodes the context's graph holds.
fn graph_len(world: &World, ctx: RenderContext) -> usize {
    world
        .root_scope()
        .query_one::<&mut ResourceGraph>(ctx.graph)
        .unwrap()
        .get()
        .len()
}

/// Drive the graph's collection the way a frame would.
fn maintain(world: &mut World, ctx: RenderContext) {
    world
        .root_scope()
        .query_one::<&mut ResourceGraph>(ctx.graph)
        .unwrap()
        .get()
        .maintain();
}

/// Despawning a source drops the ids it holds, and the next `maintain`
/// collects the nodes they were the last references to.
///
/// `despawn` drops the component, so nothing has to tell the graph that an
/// entity is gone — the reference counts carry that on their own.
#[test]
fn dropping_a_source_releases_the_nodes_it_owns() {
    let mut world = World::new();
    let ctx = noop_context(&mut world);
    let base = graph_len(&world, ctx);
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Source::new(OwningSource {
            context: ctx,
            node: None,
        }),]);
    build_once(&mut world, entity, ctx);
    assert_eq!(
        graph_len(&world, ctx),
        base + 1,
        "the source registered exactly the node it owns"
    );

    world.root_scope_mut().despawn(entity).unwrap();

    // Giving up the id only moves a count: the node is still there, and
    // the graph resolves it until something maintains.
    assert_eq!(
        graph_len(&world, ctx),
        base + 1,
        "dropping an id does not itself remove the node"
    );

    maintain(&mut world, ctx);
    assert_eq!(
        graph_len(&world, ctx),
        base,
        "maintain collected the node nothing refers to"
    );
}

/// A source that owns nothing still despawns, so dropping a source is safe
/// for every source.
#[test]
fn removing_a_source_that_owns_nothing_still_removes_it() {
    let mut world = World::new();
    let ctx = noop_context(&mut world);
    let base = graph_len(&world, ctx);
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Source::new(OwningSource {
            context: ctx,
            node: None,
        }),]);

    world.root_scope_mut().despawn(entity).unwrap();
    assert!(!world.root_scope().contains(entity));
    maintain(&mut world, ctx);
    assert_eq!(graph_len(&world, ctx), base);
}

/// Two sources leaving in the same frame give up their ids, and the frame's
/// single `maintain` is what drops both.
///
/// Neither removal touches the graph: each is a reference count reaching
/// zero, and one pass collects whatever the frame left unreferenced.
#[test]
fn one_maintain_drops_the_nodes_every_source_gave_up() {
    let mut world = World::new();
    let ctx = noop_context(&mut world);
    let base = graph_len(&world, ctx);
    let mut entities = Vec::new();
    for _ in 0..2 {
        entities.push(
            world
                .root_scope_mut()
                .spawn(bundle![Source::new(OwningSource {
                    context: ctx,
                    node: None,
                }),]),
        );
    }

    for &entity in &entities {
        build_once(&mut world, entity, ctx);
    }
    assert_eq!(
        graph_len(&world, ctx),
        base + 2,
        "each source registered a node"
    );

    for &entity in &entities {
        world.root_scope_mut().despawn(entity).unwrap();
    }

    // Both removals only moved counts: the nodes the two sources owned are
    // still there until the frame maintains.
    assert_eq!(
        graph_len(&world, ctx),
        base + 2,
        "dropping an id does not itself remove the node"
    );

    // The frame's one pass drops everything the two of them gave up.
    maintain(&mut world, ctx);
    assert_eq!(
        graph_len(&world, ctx),
        base,
        "the single maintain collected every unreferenced node"
    );
}

/// A build that returns early still settles the graph: the scope a source
/// takes at its top maintains on the way out, so no return has to remember
/// the call.
#[test]
fn a_build_that_returns_early_still_maintains() {
    let mut world = World::new();
    let ctx = noop_context(&mut world);
    let base = graph_len(&world, ctx);
    let entity = world
        .root_scope_mut()
        .spawn(bundle![Source::new(EarlyReturnSource {
            context: ctx,
            node: None,
        }),]);

    // The first build registers the node the source owns.
    build_once(&mut world, entity, ctx);
    assert_eq!(
        graph_len(&world, ctx),
        base + 1,
        "the source registered a node"
    );

    // The second build gives it up and returns at once. Nothing calls
    // `maintain`: the scope's drop is the pass, and it collects the node
    // the build left unreferenced.
    build_once(&mut world, entity, ctx);
    assert_eq!(
        graph_len(&world, ctx),
        base,
        "the early return maintained the graph"
    );
}
