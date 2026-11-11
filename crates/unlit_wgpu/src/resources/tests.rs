use std::cell::RefCell;
use std::rc::Rc;

use super::*;

/// A resource stand-in: the graph logic never inspects the handle, so the
/// tests can use a buffer.
fn buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A buffer of a distinctive size, so tests can tell handles apart —
/// wgpu handles carry no comparable identity.
fn sized_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn device() -> wgpu::Device {
    crate::util::test::noop_device().0
}

/// Insert a buffer and hand back the id that keeps it alive.
fn buffer_id(graph: &mut ResourceGraph, device: &wgpu::Device, label: &str) -> BufferId {
    graph.insert(buffer(device, label), None)
}

/// The id a [`buffer_id`] hands back: a buffer id, spelled out so the
/// tests do not have to infer it.
type BufferId = ResHandle<wgpu::Buffer>;

/// Insert a buffer whose recipe records that it ran, under `name`, and
/// returns a fresh buffer.
///
/// The names accumulate in `order`, which is how a test observes both which
/// recipes ran and in what order.
fn rebuildable_buffer(
    graph: &mut ResourceGraph,
    device: &wgpu::Device,
    order: &Rc<RefCell<Vec<&'static str>>>,
    name: &'static str,
) -> BufferId {
    let order = Rc::clone(order);
    let device = device.clone();
    graph.insert(
        buffer(&device, name),
        Some(Rebuild::new(move |_graph| {
            order.borrow_mut().push(name);
            buffer(&device, "rebuilt").into()
        })),
    )
}

#[test]
fn maintain_rebuilds_the_dependents_of_a_replaced_resource() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = buffer_id(&mut graph, &device, "base");
    let order = Rc::new(RefCell::new(Vec::new()));
    let middle = rebuildable_buffer(&mut graph, &device, &order, "middle");
    graph.add_dependency(&middle, &base);
    let leaf = rebuildable_buffer(&mut graph, &device, &order, "leaf");
    graph.add_dependency(&leaf, &middle);

    graph.replace(&base, buffer(&device, "base2"));
    graph.maintain();

    // Dependency order: the middle buffer was rebuilt before the leaf.
    assert_eq!(*order.borrow(), vec!["middle", "leaf"]);
}

#[test]
fn maintain_rebuilds_only_the_dependents_of_the_replaced_resource() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let a = buffer_id(&mut graph, &device, "a");
    let b = buffer_id(&mut graph, &device, "b");
    let order = Rc::new(RefCell::new(Vec::new()));
    let a_dependent = rebuildable_buffer(&mut graph, &device, &order, "a");
    graph.add_dependency(&a_dependent, &a);
    let b_dependent = rebuildable_buffer(&mut graph, &device, &order, "b");
    graph.add_dependency(&b_dependent, &b);

    graph.replace(&a, buffer(&device, "a2"));
    graph.maintain();

    assert_eq!(*order.borrow(), vec!["a"]);
}

/// A dirty node with no recipe is left alone rather than preventing the
/// rebuild of the nodes built from it.
#[test]
fn maintain_leaves_a_dirty_node_without_a_recipe_dirty() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = buffer_id(&mut graph, &device, "base");
    let order = Rc::new(RefCell::new(Vec::new()));
    let dependent = rebuildable_buffer(&mut graph, &device, &order, "dependent");
    graph.add_dependency(&dependent, &base);

    graph.replace(&base, buffer(&device, "base2"));
    graph.maintain();
    graph.maintain();

    // The dependent is rebuilt once; the recipe-less base stays dirty and
    // is skipped on every later pass.
    assert_eq!(*order.borrow(), vec!["dependent"]);
}

/// The old handle is handed back in the id's own kind, so a caller
/// replacing a buffer never sees another kind of resource.
#[test]
fn replace_returns_the_previous_handle() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = graph.insert(sized_buffer(&device, 64), None);

    let previous = graph.replace(&id, sized_buffer(&device, 128)).unwrap();
    assert_eq!(previous.size(), 64);
    assert_eq!(graph.get(&id).unwrap().size(), 128);
}

/// A resource nothing names any more is collected by the next `maintain`,
/// with no removal call in between.
#[test]
fn dropping_the_last_id_makes_a_resource_collectable() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = buffer_id(&mut graph, &device, "gone");

    graph.maintain();
    assert!(graph.get(&id).is_some(), "the id still names it");

    drop(id);
    graph.maintain();
    assert!(graph.is_empty(), "the last reference went with the id");
}

/// A resource a node is built from outlives the id it was inserted with:
/// the dependent's edge holds a reference of its own.
#[test]
fn a_dependent_keeps_a_resource_alive_after_its_id_is_dropped() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let part = graph.insert(buffer(&device, "part"), None);
    let dependent = graph.insert(buffer(&device, "dependent"), None);
    graph.add_dependency(&dependent, &part);

    drop(part);
    graph.maintain();
    assert_eq!(graph.len(), 2, "the dependent still holds the part");
}

/// The case the graph exists for: dropping a consumer frees the resource it
/// was the last holder of, in the same `maintain`, even though no id to
/// that resource was ever dropped.
#[test]
fn collecting_a_consumer_releases_the_dependency_it_was_the_last_holder_of() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let uniform = graph.insert(buffer(&device, "uniform"), None);
    let group = graph.insert(buffer(&device, "group"), None);
    graph.add_dependency(&group, &uniform);

    drop(group);
    drop(uniform);
    graph.maintain();
    assert!(graph.is_empty(), "both went in one pass");
}

/// The chain is followed to a fixpoint: a resource freed only because
/// another was freed in the same pass is collected too.
#[test]
fn a_chain_of_only_consumers_goes_in_one_maintain() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let leaf = graph.insert(buffer(&device, "leaf"), None);
    let middle = graph.insert(buffer(&device, "middle"), None);
    graph.add_dependency(&middle, &leaf);
    let root = graph.insert(buffer(&device, "root"), None);
    graph.add_dependency(&root, &middle);

    drop(root);
    drop(middle);
    drop(leaf);
    graph.maintain();
    assert!(graph.is_empty());
}

/// A resource two nodes are built from outlives either one alone: the
/// second dependent's edge still holds it.
#[test]
fn a_shared_dependency_survives_while_another_dependent_holds_it() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let shared = graph.insert(buffer(&device, "shared"), None);
    let first = graph.insert(buffer(&device, "first"), None);
    graph.add_dependency(&first, &shared);
    let second = graph.insert(buffer(&device, "second"), None);
    graph.add_dependency(&second, &shared);

    drop(shared);
    drop(first);
    graph.maintain();
    assert!(
        graph.get(&second).is_some(),
        "the surviving dependent still owns the shared resource"
    );
    assert_eq!(graph.len(), 2);

    drop(second);
    graph.maintain();
    assert!(graph.is_empty());
}

/// Declaring the same edge twice counts one reference, not two: dropping
/// the dependency's own id once is enough for the dependent to be the only
/// holder.
#[test]
fn a_repeated_edge_counts_one_reference() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let part = graph.insert(buffer(&device, "part"), None);
    let dependent = graph.insert(buffer(&device, "dependent"), None);
    graph.add_dependency(&dependent, &part);
    graph.add_dependency(&dependent, &part);

    let held = graph
        .graph
        .get(part.node)
        .expect("the part is in the graph")
        .strong
        .strong_count();
    assert_eq!(held, 2, "the id and the one edge, not the id and two");
}

/// A cycle is refused where it is declared. Without that, a length-2 or
/// longer cycle would make the topological order silently drop the nodes
/// in it, and a walk would stop rebuilding them without saying so.
#[test]
#[should_panic(expected = "would close a cycle")]
fn add_dependency_rejects_a_cycle() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let a = buffer_id(&mut graph, &device, "a");
    let b = graph.insert(buffer(&device, "b"), None);
    graph.add_dependency(&b, &a);
    // `a` already reaches `b`, so the reverse edge would close a cycle.
    graph.add_dependency(&a, &b);
}

#[test]
#[should_panic(expected = "cannot depend on itself")]
fn add_dependency_rejects_a_self_loop() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = buffer_id(&mut graph, &device, "a");

    graph.add_dependency(&id, &id);
}

/// A node is collected once the graph cannot reach it from anything that
/// still holds it, and a node that is still held is not.
#[test]
fn maintain_keeps_a_node_an_id_still_names() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let held = buffer_id(&mut graph, &device, "held");

    graph.maintain();
    assert!(graph.get(&held).is_some());
}

/// A recipe reads its inputs back out of the graph, so it sees the handles
/// the graph holds when it runs — the replaced ones, not the originals.
#[test]
fn rebuild_observes_the_updated_handles_of_its_dependencies() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = buffer_id(&mut graph, &device, "base");
    let observed = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&observed);
    let rebuilt = device.clone();
    let observed_base = base.clone();
    let dependent = graph.insert(
        buffer(&device, "dependent"),
        Some(Rebuild::new(move |graph| {
            let size = graph
                .get(&observed_base)
                .expect("the dependency outlives the rebuild")
                .size();
            seen.borrow_mut().push(size);
            buffer(&rebuilt, "rebuilt").into()
        })),
    );
    graph.add_dependency(&dependent, &base);

    graph.replace(&base, sized_buffer(&device, 128));
    graph.maintain();

    assert_eq!(*observed.borrow(), vec![128]);
}

#[test]
fn rebuild_visits_a_diamond_in_dependency_order() {
    let device = device();
    let mut graph = ResourceGraph::new();
    //     base
    //    /    \
    //  left   right
    //    \    /
    //     join
    let order = Rc::new(RefCell::new(Vec::new()));
    let base = rebuildable_buffer(&mut graph, &device, &order, "base");
    let left = rebuildable_buffer(&mut graph, &device, &order, "left");
    graph.add_dependency(&left, &base);
    let right = rebuildable_buffer(&mut graph, &device, &order, "right");
    graph.add_dependency(&right, &base);
    let join = rebuildable_buffer(&mut graph, &device, &order, "join");
    graph.add_dependency(&join, &left);
    graph.add_dependency(&join, &right);

    graph.replace(&base, buffer(&device, "base2"));
    graph.maintain();

    // The base and both middle nodes are rebuilt before the join, which
    // observes both of them.
    let order = order.borrow().clone();
    assert_eq!(order.len(), 4);
    assert_eq!(order[0], "base");
    assert_eq!(order[3], "join");
}

// -- kinds -------------------------------------------------------------

/// An id may cross to another thread even though the graph it names may
/// not: the strong reference it carries is an [`Arc`], so an id is `Send`
/// and `Sync` whatever its kind.
#[test]
fn an_id_crosses_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ResHandle>();
    assert_send_sync::<ResHandle<wgpu::Buffer>>();
}

/// A typed id resolves to the resource itself, without the caller
/// knowing which variant the node holds.
#[test]
fn a_typed_id_resolves_to_its_own_kind() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = graph.insert(sized_buffer(&device, 64), None);

    assert_eq!(graph.get(&id).unwrap().size(), 64);
    assert!(graph.get(&id.erase()).is_some());
}

/// Erasing an id keeps it pointing at the same node, so the erased id is
/// what a graph-wide read hands back.
#[test]
fn erasing_keeps_the_node() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = buffer_id(&mut graph, &device, "a");
    let erased = id.erase();

    assert_eq!(erased.index(), id.index());
    assert!(matches!(graph.get(&erased), Some(Resource::Buffer(_))));
    graph.replace(&id, sized_buffer(&device, 128));
    let Resource::Buffer(buffer) = graph.get(&erased).expect("the erased id still resolves") else {
        panic!("the node holds a buffer");
    };
    assert_eq!(
        buffer.size(),
        128,
        "the erased id reads the node's new resource"
    );
}

// -- inspection --------------------------------------------------------

/// Every resource is listed with its slot, kind, dirtiness and whether the
/// graph can rebuild it.
#[test]
fn nodes_reports_every_resource_with_its_kind() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = graph.insert(buffer(&device, "base"), None);
    let rebuilt = device.clone();
    let derived = graph.insert(
        buffer(&device, "derived"),
        Some(Rebuild::new(move |_| buffer(&rebuilt, "rebuilt").into())),
    );
    graph.add_dependency(&derived, &base);

    let nodes: Vec<_> = graph.nodes().collect();
    assert_eq!(nodes.len(), 2, "both resources are listed");
    assert_eq!(nodes[0].id, base.id());
    assert_eq!(nodes[0].resource.kind_name(), "Buffer");
    assert!(!nodes[0].dirty);
    assert!(!nodes[0].rebuildable);
    assert!(nodes[1].rebuildable, "the derived resource has a recipe");
}

/// The graph's summary counts its slots, groups them by kind, and reports
/// how much work maintain still has.
#[test]
fn the_graph_summary_counts_slots_by_kind() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = graph.insert(buffer(&device, "base"), None);
    let rebuilt = device.clone();
    let derived = graph.insert(
        buffer(&device, "derived"),
        Some(Rebuild::new(move |_| buffer(&rebuilt, "rebuilt").into())),
    );
    graph.insert(Resource::Virtual, None);
    graph.add_dependency(&derived, &base);
    graph.replace(&base, sized_buffer(&device, 128));

    let info = graph.info();
    assert_eq!(info.resources, 3, "three slots are occupied");
    assert_eq!(info.dirty, 2, "the base and its dependent are dirty");
    assert_eq!(
        info.rebuildable, 1,
        "only the derived resource has a recipe"
    );
    assert_eq!(
        info.kinds,
        [
            ResourceKindCount {
                kind: "Buffer",
                count: 2,
            },
            ResourceKindCount {
                kind: "Virtual",
                count: 1,
            },
        ],
        "kinds are sorted by name",
    );
    assert_eq!(
        info.kinds.iter().map(|kind| kind.count).sum::<usize>(),
        info.resources,
        "the two count the same slots",
    );

    graph.maintain();
    assert_eq!(
        graph.info().dirty,
        1,
        "the derived resource was rebuilt, but the base has no recipe to clear it",
    );
}

/// The plain-data listing mirrors the borrowed one: same slots in the same
/// order, with the handle instead of the resource.
#[test]
fn the_plain_listing_mirrors_the_borrowed_one() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = graph.insert(buffer(&device, "base"), None);
    graph.insert(Resource::Virtual, None);

    let borrowed: Vec<_> = graph.nodes().map(|node| node.id).collect();
    let plain: Vec<_> = graph
        .resource_infos()
        .into_iter()
        .map(|info| info.id)
        .collect();
    assert_eq!(plain, borrowed);
    assert_eq!(plain[0], base.id());
    assert_eq!(graph.resource_infos()[0].kind, "Buffer");
    assert_eq!(graph.resource_infos()[1].kind, "Virtual");
}

/// A resource that is dirty is reported as such until maintain runs its
/// recipe.
#[test]
fn nodes_reports_dirtiness() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let base = graph.insert(buffer(&device, "base"), None);
    let rebuilt = device.clone();
    let derived = graph.insert(
        buffer(&device, "derived"),
        Some(Rebuild::new(move |_| buffer(&rebuilt, "rebuilt").into())),
    );
    graph.add_dependency(&derived, &base);

    graph.replace(&base, sized_buffer(&device, 128));
    let dirty: Vec<_> = graph.nodes().filter(|node| node.dirty).collect();
    assert_eq!(dirty.len(), 2, "the base and its dependent are dirty");

    graph.maintain();
    let still_dirty: Vec<_> = graph.nodes().filter(|node| node.dirty).collect();
    assert_eq!(
        still_dirty.len(),
        1,
        "only the recipe-less node stays dirty"
    );
    assert_eq!(still_dirty[0].id, base.id());
}

/// The kinds a stored resource can be are named, and the name is the
/// erased kind a graph-wide read reports.
#[test]
fn a_resource_names_its_kind() {
    assert_eq!(Resource::Virtual.kind_name(), "Virtual");
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = graph.insert(buffer(&device, "buffer"), None);
    let erased = id.erase();
    assert_eq!(graph.get(&erased).unwrap().kind_name(), "Buffer");
}

/// A handle resolves back to an id to the same resource.
#[test]
fn a_handle_resolves_back_to_its_resource() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = buffer_id(&mut graph, &device, "a");
    let plain = id.id();

    let resolved = graph.resolve(plain).expect("the slot holds a resource");
    assert_eq!(resolved.id(), plain);
    assert!(matches!(graph.get(&resolved), Some(Resource::Buffer(_))));
    assert_eq!(
        graph.resolve(ResId::from_bits(plain.to_bits().wrapping_add(1))),
        None,
        "the next slot is vacant"
    );
}

/// A handle stops resolving once its slot is reused, so a stale handle
/// cannot come to name a later resource.
#[test]
fn a_stale_handle_does_not_resolve_to_the_slot_s_new_resource() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = buffer_id(&mut graph, &device, "a");
    let plain = id.id();
    let index = id.index();
    drop(id);
    graph.maintain();

    let reused = buffer_id(&mut graph, &device, "b");
    assert_eq!(reused.index(), index, "the slot is reused");
    assert!(
        graph.resolve(plain).is_none(),
        "the old handle names nothing",
    );
    assert!(graph.resolve(reused.id()).is_some());
}

/// A handle keeps the resource alive like any other id, so resolving one
/// does not race the next maintain.
#[test]
fn a_handle_resolves_only_while_the_resource_is_held() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let id = buffer_id(&mut graph, &device, "a");
    let plain = id.id();
    drop(id);

    assert!(
        graph.resolve(plain).is_none(),
        "a resource nothing holds cannot be named again",
    );
    graph.maintain();
    assert!(graph.is_empty());
}

/// A view records the format its descriptor stated.
///
/// This is the whole point of [`TextureView`] and [`TextureExt`]: wgpu's
/// bare method reports the texture's format for a view that reinterprets
/// it, so the two would otherwise be indistinguishable.
#[test]
fn a_texture_view_records_the_format_it_was_created_with() {
    let device = device();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        // The sRGB view the tests create has to be declared up front,
        // which is what a swap chain does for the view it presents.
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    });
    let mut graph = ResourceGraph::new();
    let default = graph.insert(
        TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
        None,
    );
    assert_eq!(
        graph.get(&default).map(TextureView::format),
        Some(wgpu::TextureFormat::Rgba8Unorm)
    );

    let srgb = graph.insert(
        TextureExt::create_view(
            &texture,
            &wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                ..Default::default()
            },
        ),
        None,
    );
    assert_eq!(
        graph.get(&srgb).map(TextureView::format),
        Some(wgpu::TextureFormat::Rgba8UnormSrgb)
    );
    // The view really is an sRGB one over a non-sRGB texture, which wgpu
    // itself cannot report back.
    assert_eq!(
        graph.get(&srgb).unwrap().view().texture().format(),
        wgpu::TextureFormat::Rgba8Unorm
    );
}

/// Replacing a view replaces its format with it: the two travel together
/// in a [`TextureView`], so they cannot drift apart.
#[test]
fn replace_records_the_new_views_format() {
    let device = device();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        // The sRGB view the tests create has to be declared up front,
        // which is what a swap chain does for the view it presents.
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    });
    let mut graph = ResourceGraph::new();
    let id = graph.insert(
        TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
        None,
    );

    graph.replace(
        &id,
        TextureExt::create_view(
            &texture,
            &wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                ..Default::default()
            },
        ),
    );
    assert_eq!(
        graph.get(&id).map(TextureView::format),
        Some(wgpu::TextureFormat::Rgba8UnormSrgb)
    );
}

// -- virtual nodes -----------------------------------------------------

/// The aggregation-root pattern: the root is built from its parts, so the
/// root's edge is what keeps each part alive once the caller's own id to it
/// is gone.
#[test]
fn a_virtual_root_keeps_its_parts_alive() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let part = graph.insert(buffer(&device, "part"), None);
    let root = graph.insert(Virtual, None);
    graph.add_dependency(&root, &part);

    drop(part);
    graph.maintain();
    assert!(graph.get(&root).is_some());
    assert_eq!(graph.len(), 2, "the root's edge still holds the part");
}

/// Dropping a virtual root frees the parts it was the last holder of, in
/// the same `maintain`.
#[test]
fn dropping_a_virtual_root_frees_its_parts() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let part = graph.insert(buffer(&device, "part"), None);
    let root = graph.insert(Virtual, None);
    graph.add_dependency(&root, &part);

    drop(part);
    drop(root);
    graph.maintain();
    assert!(graph.is_empty());
}

/// A part two roots are built from outlives either root alone: the other
/// root's edge still holds it.
#[test]
fn a_shared_part_survives_while_another_root_is_alive() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let part = graph.insert(buffer(&device, "part"), None);
    let first = graph.insert(Virtual, None);
    graph.add_dependency(&first, &part);
    let second = graph.insert(Virtual, None);
    graph.add_dependency(&second, &part);

    drop(part);
    drop(first);
    graph.maintain();
    assert!(
        graph.get(&second).is_some(),
        "the surviving root still owns the shared part"
    );
    assert_eq!(graph.len(), 2);

    drop(second);
    graph.maintain();
    assert!(graph.is_empty());
}

/// Replacing a part marks the virtual root built from it dirty, and the
/// root's recipe follows its part in dependency order.
#[test]
fn replacing_a_part_rebuilds_the_virtual_root() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let part = graph.insert(buffer(&device, "part"), None);
    let order = Rc::new(RefCell::new(Vec::new()));
    let root_order = Rc::clone(&order);
    let root = graph.insert(
        Virtual,
        Some(Rebuild::new(move |_graph| {
            root_order.borrow_mut().push("root");
            Resource::Virtual
        })),
    );
    graph.add_dependency(&root, &part);

    graph.replace(&part, buffer(&device, "part2"));
    graph.maintain();
    assert_eq!(*order.borrow(), vec!["root"]);
}

/// A virtual node is a graph citizen with no handle: it resolves to
/// [`Virtual`] and to nothing else.
#[test]
fn a_virtual_node_has_no_handle_of_its_own() {
    let mut graph = ResourceGraph::new();
    let root = graph.insert(Virtual, None);

    assert!(matches!(graph.get(&root), Some(Virtual)));
}

/// A virtual node nothing holds is collected like any other node.
#[test]
fn maintain_collects_a_virtual_node_with_no_holders() {
    let mut graph = ResourceGraph::new();
    let root = graph.insert(Virtual, None);

    drop(root);
    graph.maintain();
    assert!(graph.is_empty());
}

/// A node keeps its kind: replacing a virtual node hands back the previous
/// virtual value, and the node stays a root its parts hang from.
#[test]
fn a_virtual_node_can_be_replaced() {
    let device = device();
    let mut graph = ResourceGraph::new();
    let root = graph.insert(Virtual, None);
    let dependent = graph.insert(buffer(&device, "dependent"), None);
    graph.add_dependency(&dependent, &root);

    let previous = graph
        .replace(&root, Virtual)
        .expect("the node is in the graph");
    assert_eq!(previous, Virtual);
    assert!(matches!(graph.get(&root), Some(Virtual)));
    assert_eq!(
        graph.dependencies(&dependent).collect::<Vec<_>>(),
        vec![root.erase()]
    );
}
