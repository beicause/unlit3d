//! Assembling a frame's draw list.
//!
//! The frame's work splits in two: which meshes the camera can see is decided
//! by [crate::culling], and how an entity resolves to a concrete pipeline is
//! decided by [crate::pipeline]. This module joins them. It resolves each
//! visible entity to a pipeline through the family registered for its key
//! type, tags it with what the scene builder needs to group it, and sorts the
//! result. Nothing here compiles a pipeline until a draw first resolves it.

use core::cmp::Ordering;
use core::marker::PhantomData;

use arrayvec::ArrayVec;
use hashbrown::HashMap;
use unlit_ecs::{Query, TypeIdHashMap, World};
use unlit_wgpu::resources::{ResourceId, Virtual};
use unlit_wgpu::specialize::{PipelineDescriptor, Specializer, SurfaceKey, Variants};

use unlit_wgpu::pipeline::{GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_GROUP};
use unlit_wgpu::scene::{DrawEntry, DrawRange, MAX_VERTEX_BUFFERS, Scene, VertexBufferBinding};

use crate::bounds::FrustumPlanes;
use crate::components::{Camera, GpuMaterial, GpuMesh, GpuRenderPipeline, ZSortedDrawing};
use crate::culling::{VisibleMesh, collect_visible};
use crate::pipeline::{
    DrawKey, FamilyContext, RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineId,
    RenderPipelineKey, RenderResources,
};

/// A family: a specialization cache plus the factory that turns each variant
/// into a [RegisteredRenderPipeline].
///
/// The core variant index a draw resolves to is also its index into
/// registered, so the two lists grow in lockstep. The key type parameter is
/// the [RenderPipelineKey] the family is registered for and the component its
/// queries fetch.
pub(crate) struct Family<D, S, F, K>
where
    D: PipelineDescriptor<wgpu::RenderPipeline> + 'static,
    S: Specializer<D>,
    F: RenderPipelineFactory<D>,
{
    /// The variant cache, which owns the specializer and the device.
    variants: Variants<wgpu::RenderPipeline, D, S>,
    /// The factory that turns a variant into a wgpu render pipeline.
    factory: F,
    /// registered\[v\] is the wgpu render pipeline for core variant v.
    registered: Vec<RenderPipelineId>,
    /// Names the key type without owning one.
    _key: PhantomData<fn() -> K>,
}

impl<D, S, F, K> Family<D, S, F, K>
where
    D: PipelineDescriptor<wgpu::RenderPipeline> + 'static,
    S: Specializer<D>,
    F: RenderPipelineFactory<D>,
{
    /// A family whose variants are described by `factory`.
    pub(crate) fn new(device: &wgpu::Device, specializer: S, factory: F) -> Self {
        Self {
            variants: Variants::new(device, specializer),
            factory,
            registered: Vec::new(),
            _key: PhantomData,
        }
    }
}

/// A visible entity awaiting its draw command, tagged with everything the
/// scene builder needs to group it.
///
/// Entries are sorted once per frame so a single linear pass can emit every
/// pipeline and material group without intermediate scratch buffers: opaque
/// entries first, keyed by material so shared-state draws stay adjacent, then
/// z-sorted entries keyed by camera distance so they are drawn
/// back-to-front.
pub(crate) struct VisibleEntry {
    /// The culled mesh this draw came from, with its entity and placement.
    pub(crate) mesh: VisibleMesh,
    /// The concrete pipeline this entity resolves to, an index into the
    /// source's pipeline list. Resolved before the sort and never changed by
    /// it.
    pub(crate) pipeline_id: RenderPipelineId,
    /// Groups opaque draws by material, so neighbours share a bind group.
    /// Ignored for z-sorted entries.
    pub(crate) sort_key: u64,
    /// Distance to the camera, used to order z-sorted entries back-to-front.
    /// Ignored for opaque entries.
    pub(crate) depth: f32,
    /// True when the entity carries [ZSortedDrawing].
    pub(crate) z_sorted: bool,
    /// Everything the draw binds, named by value rather than by handle.
    ///
    /// The resolve pass already holds the mesh and material components, so it
    /// fills this in there and the assembly pass can intern one handle set per
    /// distinct key instead of resolving one per entity.
    pub(crate) handles_key: DrawHandlesKey,
}

/// The inputs a family needs to collect and resolve one frame.
///
/// Holding the device and resources here lets a family build the
/// [FamilyContext] its factory sees without borrowing the source again.
pub(crate) struct FamilyFrame<'a> {
    /// The world the entities live in.
    pub(crate) world: &'a World,
    /// The visible meshes culled this frame, with placement resolved.
    pub(crate) meshes: &'a [VisibleMesh],
    /// The frame's camera, for the z-sorted sort.
    pub(crate) camera: &'a Camera,
    /// The frame's render target.
    pub(crate) surface: SurfaceKey,
    /// The device a newly resolved variant is compiled on.
    pub(crate) device: &'a wgpu::Device,
    /// The source's global buffers.
    pub(crate) resources: &'a RenderResources,
}

/// Resolves one family's entities for one frame.
///
/// It bundles the family's mutable state with the frame's inputs so the shared
/// collect and resolve code can run as methods instead of threading eight
/// arguments through a free function.
struct Resolver<'a, 'f, D, S, F, K>
where
    D: PipelineDescriptor<wgpu::RenderPipeline> + 'static,
    S: Specializer<D>,
    F: RenderPipelineFactory<D>,
    K: RenderPipelineKey<Descriptor = D>,
    S::Key: From<(K, DrawKey)>,
{
    frame: &'a FamilyFrame<'f>,
    variants: &'a mut Variants<wgpu::RenderPipeline, D, S>,
    factory: &'a F,
    registered: &'a mut Vec<RenderPipelineId>,
    visible: &'a mut Vec<VisibleEntry>,
    register: &'a mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
    _key: PhantomData<fn() -> K>,
}

/// What a family reads from one visible entity while resolving it.
///
/// Resolving the columns once per archetype rather than once per entity is what
/// makes the frame's resolve pass proportional to the number of *distinct
/// component sets* rather than to the number of entities: a `World::get` per
/// entity would re-read the world's entity table and re-look-up the component
/// type every time.
type Resolve<'a, K> = (
    &'a GpuRenderPipeline<K>,
    &'a GpuMesh,
    Option<&'a GpuMaterial>,
    Option<&'a ZSortedDrawing>,
);

impl<D, S, F, K> Resolver<'_, '_, D, S, F, K>
where
    D: PipelineDescriptor<wgpu::RenderPipeline> + 'static,
    S: Specializer<D>,
    F: RenderPipelineFactory<D>,
    K: RenderPipelineKey<Descriptor = D>,
    S::Key: From<(K, DrawKey)>,
{
    /// Resolve every visible entity this family draws into [Self::visible].
    ///
    /// The frame's meshes were already culled and their placement resolved,
    /// so this only picks out the ones whose pipeline belongs to the family.
    ///
    /// Culling walks the archetypes in storage order, so the candidates arrive
    /// grouped by archetype and the columns below are resolved once per group.
    /// The caching only helps when they are grouped, but it is correct either
    /// way: a cache miss just re-resolves.
    fn collect(&mut self) {
        profiling::scope!("scene.resolve.family");
        // Copied out of `self.frame` so the resolved state borrows the world
        // rather than `self`, which `push` needs mutably.
        let world = self.frame.world;
        let mut cached: Option<u32> = None;
        let mut state = None;
        for &candidate in self.frame.meshes {
            let Some(location) = world.location(candidate.entity) else {
                continue;
            };
            if cached != Some(location.archetype()) {
                cached = Some(location.archetype());
                let archetype = world
                    .archetype(location.archetype())
                    .expect("a live entity's archetype exists");
                // An entity from another family carries a different key type,
                // so its archetype does not match and the whole group is
                // skipped.
                state = <Resolve<'_, K> as Query>::matches(archetype)
                    .then(|| <Resolve<'_, K> as Query>::fetch_state(archetype));
            }
            let Some(state) = state.as_ref() else {
                continue;
            };
            let (pipeline, gpu_mesh, material, z_sorted) =
                <Resolve<'_, K> as Query>::fetch(state, location.row());
            // The material decides both how the draw is grouped and which bind
            // group it binds, so it is read once and both are taken from it.
            let (sort_key, material_bg) = match material {
                Some(material) => (material.sort_key(), Some(material.bind_group_id)),
                None => (0, None),
            };
            self.push(
                candidate,
                &pipeline,
                &gpu_mesh,
                sort_key,
                material_bg,
                z_sorted.is_some(),
            );
        }
    }

    /// Resolve one candidate's variant, registering the pipeline on first
    /// sight, and push its [VisibleEntry].
    fn push(
        &mut self,
        mesh: VisibleMesh,
        pipeline: &GpuRenderPipeline<K>,
        gpu_mesh: &GpuMesh,
        sort_key: u64,
        material_bg: Option<ResourceId<wgpu::BindGroup>>,
        z_sorted: bool,
    ) {
        let instance = mesh.instance;
        let draw = DrawKey::for_mesh(self.frame.surface, gpu_mesh);
        let key = S::Key::from((pipeline.key().clone(), draw));
        let ordinal = self
            .variants
            .specialize(|| pipeline.key().base_descriptor(), key);

        let pipeline_id = match self.registered.get(ordinal as usize) {
            Some(&id) => id,
            None => {
                let context = FamilyContext {
                    device: self.frame.device,
                    resources: self.frame.resources,
                };
                let desc = self
                    .factory
                    .descriptor(&context, self.variants.get(ordinal));
                let id = (self.register)(desc);
                self.registered.push(id);
                id
            }
        };

        let centre = glam::Vec3A::from(instance.translation());
        let depth = (centre - glam::Vec3A::from(self.frame.camera.position)).length();
        // The handles a draw binds depend on the mesh, the shape read from it
        // and the material beside it, so the key is filled in here where all
        // three are already in hand.
        let handles_key = DrawHandlesKey {
            mesh: gpu_mesh.root,
            shape: DrawShape {
                indexed: gpu_mesh.indexed,
                count: gpu_mesh.count,
                first: gpu_mesh.first,
                base_vertex: gpu_mesh.base_vertex,
            },
            material: material_bg,
        };
        self.visible.push(VisibleEntry {
            mesh,
            pipeline_id,
            sort_key,
            depth,
            z_sorted,
            handles_key,
        });
    }
}

/// A registered family, as the source stores it. Type-erased.
pub(crate) trait AnyFamily {
    /// Resolve every entity this family's key names and append the visible
    /// ones to `visible`.
    fn collect_and_resolve(
        &mut self,
        frame: &FamilyFrame<'_>,
        visible: &mut Vec<VisibleEntry>,
        register: &mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
    );
}

impl<D, S, F, K> AnyFamily for Family<D, S, F, K>
where
    D: PipelineDescriptor<wgpu::RenderPipeline> + 'static,
    S: Specializer<D>,
    F: RenderPipelineFactory<D>,
    K: RenderPipelineKey<Descriptor = D>,
    S::Key: From<(K, DrawKey)>,
{
    fn collect_and_resolve(
        &mut self,
        frame: &FamilyFrame<'_>,
        visible: &mut Vec<VisibleEntry>,
        register: &mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
    ) {
        Resolver {
            frame,
            variants: &mut self.variants,
            factory: &self.factory,
            registered: &mut self.registered,
            visible,
            register,
            _key: PhantomData,
        }
        .collect();
    }
}

/// The caches and families one frame's draw list is assembled into.
pub(crate) struct SceneFrame<'a> {
    /// Every registered family, keyed by its key type.
    pub(crate) families: &'a mut TypeIdHashMap<Box<dyn AnyFamily>>,
    /// Reused buffer the frame's culled meshes land in.
    pub(crate) meshes: &'a mut Vec<VisibleMesh>,
    /// Reused buffer the sorted draw entries land in.
    pub(crate) visible: &'a mut Vec<VisibleEntry>,
    /// Registers a newly resolved pipeline and returns its id.
    pub(crate) register: &'a mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
}

/// Cull `world` against the `camera`, resolve every survivor through its
/// family, and sort the result for drawing.
///
/// Culling is a CPU pass over every mesh entity, done once here rather than
/// once per family: it resolves each mesh's placement and tint and drops the
/// entities outside the camera frustum. Only the survivors reach the
/// registered families, so nothing is specialized or registered for a mesh the
/// camera cannot see.
///
/// The ordering is the whole point of this pass: opaque entities first,
/// grouped by pipeline and then by material so a draw never re-binds state a
/// neighbour already set; z-sorted entities after them, sorted back-to-front
/// by camera distance so blending is order-independent.
pub(crate) fn collect_and_sort_visible(
    frame: SceneFrame<'_>,
    world: &World,
    camera: &Camera,
    surface: SurfaceKey,
    device: &wgpu::Device,
    resources: &RenderResources,
) {
    let SceneFrame {
        families,
        meshes,
        visible,
        register,
    } = frame;
    let frustum = FrustumPlanes::from_clip_from_world(camera.clip_from_world);

    {
        profiling::scope!("scene.cull");
        collect_visible(world, &frustum, meshes);
    }
    visible.clear();
    let frame = FamilyFrame {
        world,
        meshes,
        camera,
        surface,
        device,
        resources,
    };
    {
        profiling::scope!("scene.resolve");
        for family in families.values_mut() {
            family.collect_and_resolve(&frame, visible, register);
        }
    }

    // Sort: not z-sorted before z-sorted, then by pipeline, then by the key that
    // matters for that kind. Opaque draws are keyed by material so neighbours
    // share a bind group; z-sorted ones by camera distance so they are
    // composited back-to-front.
    profiling::scope!("scene.sort");
    visible.sort_unstable_by(|a, b| {
        a.z_sorted
            .cmp(&b.z_sorted)
            .then_with(|| a.pipeline_id.cmp(&b.pipeline_id))
            .then_with(|| {
                if a.z_sorted {
                    // Back-to-front: the farthest entity is drawn first.
                    b.depth.partial_cmp(&a.depth).unwrap_or(Ordering::Equal)
                } else {
                    a.sort_key.cmp(&b.sort_key)
                }
            })
    });
}

/// Identifies the resource-graph handles one draw names.
///
/// Everything a draw binds is determined by the mesh it reads, the shape it
/// reads from that mesh, and the material bound beside it. Two entries with the
/// same key therefore name the same buffers and bind groups, so the handle set
/// is resolved once per key rather than once per entry.
///
/// This is what the visible set carries instead of the handles themselves: a
/// scene of many entities sharing a few meshes would otherwise deep-clone a
/// handful of wgpu handles per entity per frame, and clone them into a set it
/// immediately drops again.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DrawHandlesKey {
    /// The mesh's root node, which names its buffers and bind group.
    pub(crate) mesh: ResourceId<Virtual>,
    /// What the draw reads from the mesh.
    pub(crate) shape: DrawShape,
    /// The material's bind group, when the entity carries a material.
    pub(crate) material: Option<ResourceId<wgpu::BindGroup>>,
}

/// A registered pipeline's GPU handles, indexed by [`RenderPipelineId`].
pub(crate) struct RenderPipelineHandles {
    /// The compiled pipeline.
    pub(crate) pipeline: wgpu::RenderPipeline,
    /// The bind group bound at the global index, when the pipeline binds one.
    pub(crate) global: Option<wgpu::BindGroup>,
}

/// The shape of one draw: what is drawn and from which source.
///
/// Resolved once per entry alongside its handles, so assembling the draws
/// names no [`GpuMesh`] and therefore needs no access to the world.
///
/// Comparable so that two entries drawing the same geometry are recognized as
/// one instanced draw; see [`assemble_scene`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DrawShape {
    /// Whether the draw is indexed; when it is not, `count` is a vertex count.
    pub(crate) indexed: bool,
    /// How many indices or vertices the draw reads.
    pub(crate) count: u32,
    /// Where the draw starts in the buffer it reads: the first index for an
    /// indexed draw, the first vertex for a non-indexed one.
    pub(crate) first: u32,
    /// Where the mesh's vertices start, for an indexed draw to offset every
    /// index by.
    pub(crate) base_vertex: u32,
}

/// The resource-graph handles one interned key resolves to.
///
/// These are the concrete wgpu handles a draw binds, cloned out of the graph
/// once per distinct [`DrawHandlesKey`] per frame rather than once per entry.
pub(crate) struct EntryHandles {
    /// The mesh's bind group, if it has one.
    pub(crate) mesh_bg: Option<wgpu::BindGroup>,
    /// The material's bind group, if the entity carries a material.
    pub(crate) material_bg: Option<wgpu::BindGroup>,
    /// The mesh's vertex buffers, each with the slot it binds to.
    pub(crate) vertex_buffers: ArrayVec<VertexBufferBinding, MAX_VERTEX_BUFFERS>,
    /// The index buffer and its format, when the mesh is indexed.
    pub(crate) index_buffer: Option<(wgpu::Buffer, wgpu::IndexFormat)>,
}

/// Whether two adjacent entries can be drawn as one instanced draw.
///
/// A draw's state is its pipeline, its bind groups, its buffers and its
/// geometry; the interned handle key names all of those but the pipeline, so
/// two entries with equal keys and equal pipelines differ only in which
/// instance record they read, and one draw with a wider instance range says the
/// same thing. The global bind group is the pipeline's, so the pipeline id
/// covers it.
///
/// Z-sorted entries never merge, with each other or with anything else: they
/// are blended back-to-front, so the order they are drawn in is the result, and
/// a merged draw would rasterize its instances in record order instead. Opaque
/// draws are depth-tested with blending off, so their order is not observable
/// and merging them is safe.
fn same_draw(a: &VisibleEntry, b: &VisibleEntry) -> bool {
    !a.z_sorted && !b.z_sorted && a.pipeline_id == b.pipeline_id && a.handles_key == b.handles_key
}

/// Append the frame's draws to `scene`, one per run of entries that share a
/// draw's state.
///
/// The entries have already been culled, resolved and sorted, and each names
/// its resources by [`DrawHandlesKey`]; `handles` maps a key to the handles
/// resolved for it, one entry per distinct key. Assembling is therefore a
/// linear pass over slices that names nothing the world or the graph owns, and
/// it resolves no handle of its own.
///
/// The draws are appended in `visible` order, so the sort that put neighbours
/// on the same pipeline and bind groups is what keeps recording's state changes
/// few. Neighbours that match on every part of a draw's state are then folded
/// into a single instanced draw: recording costs a command and a state re-bind,
/// so an entity that shares a mesh with the one before it is nearly free.
///
/// The instance range is what keeps a merged draw correct. Instance-stepped
/// attributes are fetched at the instance's ordinal — `firstInstance` plus its
/// index within the draw — and the instance buffer is packed in `visible`
/// order, so instances `a..b` read exactly the records of entries `a..b`, the
/// same ones the separate draws would have read.
///
/// # Panics
///
/// If an entry's key has no handles in `handles`: every key the visible set
/// carries is interned before this is called.
pub(crate) fn assemble_scene(
    scene: &mut Scene,
    visible: &[VisibleEntry],
    pipelines: &[RenderPipelineHandles],
    handles: &HashMap<DrawHandlesKey, EntryHandles>,
    instance_buffer: &wgpu::Buffer,
) {
    let mut start = 0;
    while start < visible.len() {
        // Extend the run while each entry matches the one before it, which is
        // enough to make every entry of the run match every other: the
        // comparison is an equality on a draw's state.
        let mut end = start + 1;
        while end < visible.len() && same_draw(&visible[end - 1], &visible[end]) {
            end += 1;
        }

        let entry = &visible[start];
        let shape = entry.handles_key.shape;
        let handle = handles
            .get(&entry.handles_key)
            .expect("every visible entry's handles were interned");
        let pipeline = &pipelines[entry.pipeline_id.as_usize()];

        let first = shape.first;
        let range = if shape.indexed {
            DrawRange::indexed(first..first + shape.count)
                .with_base_vertex(shape.base_vertex as i32)
                .with_instances(start as u32..end as u32)
        } else {
            DrawRange::vertices(first..first + shape.count).with_instances(start as u32..end as u32)
        };

        let mut draw = DrawEntry::new(&pipeline.pipeline, range);
        if let Some(global) = &pipeline.global {
            draw = draw.with_bind_group(GLOBAL_GROUP, global);
        }
        if let Some(material_bg) = &handle.material_bg {
            draw = draw.with_bind_group(MATERIAL_GROUP, material_bg);
        }
        if let Some(mesh_bg) = &handle.mesh_bg {
            draw = draw.with_bind_group(MESH_GROUP, mesh_bg);
        }
        for (slot, buffer, range) in &handle.vertex_buffers {
            draw = draw.with_vertex_buffer_range(*slot, buffer, range.clone());
        }
        if let Some((buffer, format)) = &handle.index_buffer {
            draw = draw.with_index_buffer(buffer, *format);
        }
        draw = draw.with_vertex_buffer(INSTANCE_SLOT, instance_buffer);

        scene.push(draw);
        start = end;
    }
}
