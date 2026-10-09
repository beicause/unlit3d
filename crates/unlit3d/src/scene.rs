//! Assembling a frame's draw list.
//!
//! The frame's work splits in two: which meshes the camera can see is decided
//! by [crate::culling], and how an entity resolves to a concrete pipeline is
//! decided by [crate::pipeline]. This module joins them. It resolves each
//! visible entity to a pipeline through the family registered for its key
//! type, tags it with what the scene builder needs to group it, and sorts the
//! result. Nothing here compiles a pipeline until a draw first resolves it.

use core::cmp::Ordering;

use arrayvec::ArrayVec;
use hashbrown::HashMap;
use unlit_ecs::{Query, World};
use unlit_wgpu::instance_stream::InstanceBuffer;
use unlit_wgpu::resources::{ResHandle, Virtual};
use unlit_wgpu::specialize::{PipelineVariant, SurfaceKey, Variants};

use unlit_wgpu::pipeline::{GLOBAL_GROUP, MATERIAL_GROUP, MESH_GROUP};
use unlit_wgpu::scene::{DrawEntry, DrawRange, MAX_VERTEX_BUFFERS, Scene, VertexBufferBinding};

use crate::bounds::FrustumPlanes;
use crate::components::{Camera, GpuMaterial, GpuMesh, GpuRenderPipeline, ZSortedDrawing};
use crate::culling::{VisibleMesh, collect_visible};
use crate::pipeline::{
    DrawContext, FamilyContext, GlobalResources, InstanceContext, InstanceData,
    RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineId, RenderPipelineKey,
};

/// A registered family's position in the source's family list.
///
/// The source stores its families in registration order and indexes them by
/// this value, so an entry can name the family that drew it without carrying a
/// borrow or a reference-counted handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FamilyId(u32);

impl FamilyId {
    /// The id of the family registered at `index`.
    pub(crate) fn new(index: u32) -> Self {
        Self(index)
    }

    /// The id as a `usize`, for indexing the source's family list.
    pub(crate) fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// A family: a specialization cache, the factory that turns each variant into
/// a [RegisteredRenderPipeline], and the per-instance stream its draws read.
///
/// The core variant index a draw resolves to is also its index into
/// registered, so the two lists grow in lockstep. The key type parameter is
/// the [RenderPipelineKey] the family is registered for and the component its
/// queries fetch; the factory's descriptor type is the one the key's variant
/// resolves to. The instance-data parameter is the family's own per-instance
/// record layout, written once per visible instance each frame.
pub(crate) struct Family<K, F, I>
where
    K: RenderPipelineKey,
    F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>,
    I: InstanceData,
{
    /// The variant cache, which owns the device.
    variants: Variants<wgpu::RenderPipeline, K::Variant>,
    /// The factory that turns a variant into a wgpu render pipeline.
    factory: F,
    /// registered\[v\] is the wgpu render pipeline for core variant v.
    registered: Vec<RenderPipelineId>,
    /// The family's per-instance record layout and the writer that fills it.
    instances: I,
    /// The reused buffer one frame's records are uploaded into.
    stream: InstanceBuffer,
    /// The frame's records, packed in visible order. Reused between frames.
    records: Vec<u8>,
}

impl<K, F, I> Family<K, F, I>
where
    K: RenderPipelineKey,
    F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>,
    I: InstanceData,
{
    /// A family whose variants are described by `factory` and whose instances
    /// are written by `instances`.
    pub(crate) fn new(device: &wgpu::Device, factory: F, instances: I) -> Self {
        Self {
            variants: Variants::new(device),
            factory,
            registered: Vec::new(),
            stream: InstanceBuffer::new(instances.stream()),
            instances,
            records: Vec::new(),
        }
    }
}

/// A visible entity awaiting its draw command, tagged with everything the
/// scene builder needs to group it.
///
/// Entries are sorted once per frame so a single linear pass can emit every
/// pipeline and material group without intermediate scratch buffers: opaque
/// entries first, keyed by material so shared-state draws stay adjacent, then
/// z-sorted entries keyed by view-axis depth so they are drawn
/// back-to-front.
pub(crate) struct VisibleEntry {
    /// The culled mesh this draw came from, with its entity and placement.
    pub(crate) mesh: VisibleMesh,
    /// The family that resolved this entry, which owns the instance stream it
    /// reads.
    pub(crate) family: FamilyId,
    /// The concrete pipeline this entity resolves to, an index into the
    /// source's pipeline list. Resolved before the sort and never changed by
    /// it.
    pub(crate) pipeline_id: RenderPipelineId,
    /// This entry's record within its family's instance stream, filled in when
    /// the stream is packed.
    pub(crate) instance_index: u32,
    /// Groups opaque draws by material, so neighbours share a bind group.
    /// Ignored for z-sorted entries.
    pub(crate) sort_key: u64,
    /// Depth along the camera's view axis, used to order z-sorted entries
    /// back-to-front. Ignored for opaque entries.
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
    /// The source's global buffer ids.
    pub(crate) resources: GlobalResources,
}

/// Resolves one family's entities for one frame.
///
/// It bundles the family's mutable state with the frame's inputs so the shared
/// collect and resolve code can run as methods instead of threading eight
/// arguments through a free function.
struct Resolver<'a, 'f, K, F>
where
    K: RenderPipelineKey,
    F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>,
{
    family: FamilyId,
    frame: &'a FamilyFrame<'f>,
    variants: &'a mut Variants<wgpu::RenderPipeline, K::Variant>,
    factory: &'a F,
    registered: &'a mut Vec<RenderPipelineId>,
    visible: &'a mut Vec<VisibleEntry>,
    register: &'a mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
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

impl<K, F> Resolver<'_, '_, K, F>
where
    K: RenderPipelineKey,
    F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>,
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
            let (sort_key, material_bg) = match &material {
                Some(material) => (material.sort_key(), Some(material.bind_group_id.clone())),
                None => (0, None),
            };
            self.push(
                candidate,
                &pipeline,
                &gpu_mesh,
                material.as_deref(),
                sort_key,
                material_bg,
                z_sorted.is_some(),
            );
        }
    }

    /// Resolve one candidate's variant, registering the pipeline on first
    /// sight, and push its [VisibleEntry].
    #[expect(
        clippy::too_many_arguments,
        reason = "one field per resolved draw component"
    )]
    fn push(
        &mut self,
        mesh: VisibleMesh,
        pipeline: &GpuRenderPipeline<K>,
        gpu_mesh: &GpuMesh,
        material: Option<&GpuMaterial>,
        sort_key: u64,
        material_bg: Option<ResHandle<wgpu::BindGroup>>,
        z_sorted: bool,
    ) {
        // The frame, not the entity, knows the target, the mesh's layout and
        // whether a material is bound, so the variant is derived from what the
        // draw actually resolves to.
        let draw = DrawContext {
            surface: self.frame.surface,
            device: self.frame.device,
            world: self.frame.world,
            entity: mesh.entity,
            mesh: gpu_mesh,
            material,
        };
        let variant = pipeline.key().variant(&draw);
        let ordinal = self.variants.specialize(variant);

        let pipeline_id = match self.registered.get(ordinal as usize) {
            Some(&id) => id,
            None => {
                let context = FamilyContext {
                    device: self.frame.device,
                    resources: self.frame.resources.clone(),
                };
                let desc = self
                    .factory
                    .descriptor(&context, self.variants.get(ordinal));
                let id = (self.register)(desc);
                self.registered.push(id);
                id
            }
        };

        // The entity origin is not where the geometry is: a mesh whose pivot
        // sits at its feet or its base would sort by that pivot rather than by
        // what it draws. The mesh's own local bounds say where the geometry
        // actually is, so the centre is taken from them and placed with the
        // entity's model matrix.
        let centre = mesh.world_from_local.transform_point3(gpu_mesh.aabb.center);
        // Depth along the view axis, not distance to the eye: under a
        // perspective projection two entities the same euclidean distance away
        // but at different angles are not equally deep, and only the depth
        // decides which one a blended draw composites first.
        let depth = self.frame.camera.view_depth(centre);
        // The handles a draw binds depend on the mesh, the shape read from it
        // and the material beside it, so the key is filled in here where all
        // three are already in hand.
        let handles_key = DrawHandlesKey {
            mesh: gpu_mesh.parts.root.clone(),
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
            family: self.family,
            pipeline_id,
            // Filled in when the family packs its instance stream; no record
            // exists until then.
            instance_index: 0,
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
        family: FamilyId,
        frame: &FamilyFrame<'_>,
        visible: &mut Vec<VisibleEntry>,
        register: &mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
    );

    /// Drop the previous frame's records, ready to pack this frame's.
    fn begin_instances(&mut self);

    /// Append one record for `context` and return its index in the stream.
    ///
    /// A family with no instance stream returns 0 and writes nothing. The
    /// context also carries the frame's joint-matrix and morph-weight arrays,
    /// which the family appends to only if its records carry those offsets.
    fn push_instance(&mut self, context: &mut InstanceContext<'_>) -> u32;

    /// Upload the frame's packed records through `encoder`.
    fn upload_instances(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder);

    /// The slot and buffer this family's stream binds, or `None` when it has
    /// no stream or nothing to upload.
    fn instance_binding(&self) -> Option<(u32, wgpu::Buffer)>;
}

impl<K, F, I> AnyFamily for Family<K, F, I>
where
    K: RenderPipelineKey,
    F: RenderPipelineFactory<<K::Variant as PipelineVariant<wgpu::RenderPipeline>>::Descriptor>,
    I: InstanceData,
{
    fn collect_and_resolve(
        &mut self,
        family: FamilyId,
        frame: &FamilyFrame<'_>,
        visible: &mut Vec<VisibleEntry>,
        register: &mut dyn FnMut(RegisteredRenderPipeline) -> RenderPipelineId,
    ) {
        Resolver::<K, F> {
            family,
            frame,
            variants: &mut self.variants,
            factory: &self.factory,
            registered: &mut self.registered,
            visible,
            register,
        }
        .collect();
    }

    fn begin_instances(&mut self) {
        self.records.clear();
    }

    fn push_instance(&mut self, context: &mut InstanceContext<'_>) -> u32 {
        let stride = self.instances.stream().array_stride as usize;
        if stride == 0 {
            return 0;
        }
        // Records are appended one stride at a time, so the index of the one
        // about to be written is how many strides precede it.
        let index = (self.records.len() / stride) as u32;
        let start = self.records.len();
        self.records.resize(start + stride, 0);
        self.instances.write(context, &mut self.records[start..]);
        index
    }

    fn upload_instances(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        self.stream.upload(device, encoder, &self.records);
    }

    fn instance_binding(&self) -> Option<(u32, wgpu::Buffer)> {
        let desc = self.instances.stream();
        if desc.array_stride == 0 {
            return None;
        }
        self.stream
            .buffer()
            .map(|buffer| (desc.slot, buffer.clone()))
    }
}

/// The caches and families one frame's draw list is assembled into.
pub(crate) struct SceneFrame<'a> {
    /// Every registered family, in registration order.
    pub(crate) families: &'a mut [Box<dyn AnyFamily>],
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
/// by view-axis depth so blending is order-independent.
pub(crate) fn collect_and_sort_visible(
    frame: SceneFrame<'_>,
    world: &World,
    camera: &Camera,
    surface: SurfaceKey,
    device: &wgpu::Device,
    resources: GlobalResources,
) {
    let SceneFrame {
        families,
        meshes,
        visible,
        register,
    } = frame;
    let frustum = FrustumPlanes::from_clip_from_world(camera.clip_from_world());

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
        for (index, family) in families.iter_mut().enumerate() {
            family.collect_and_resolve(FamilyId::new(index as u32), &frame, visible, register);
        }
    }

    // Sort: not z-sorted before z-sorted, then by pipeline, then by the key that
    // matters for that kind. Opaque draws are keyed by material so neighbours
    // share a bind group; z-sorted ones by view-axis depth so they are
    // composited back-to-front.
    profiling::scope!("scene.sort");
    visible.sort_unstable_by(|a, b| {
        a.z_sorted
            .cmp(&b.z_sorted)
            .then_with(|| a.pipeline_id.cmp(&b.pipeline_id))
            .then_with(|| {
                if a.z_sorted {
                    // Back-to-front: the deepest entity is drawn first.
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
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct DrawHandlesKey {
    /// The mesh's root node, which names its buffers and bind group.
    pub(crate) mesh: ResHandle<Virtual>,
    /// What the draw reads from the mesh.
    pub(crate) shape: DrawShape,
    /// The material's bind group, when the entity carries a material.
    pub(crate) material: Option<ResHandle<wgpu::BindGroup>>,
}

/// A registered pipeline's GPU handles, indexed by [RenderPipelineId].
pub(crate) struct RenderPipelineHandles {
    /// The compiled pipeline.
    pub(crate) pipeline: wgpu::RenderPipeline,
    /// The bind group bound at the global index, when the pipeline binds one.
    pub(crate) global: Option<wgpu::BindGroup>,
}

/// The shape of one draw: what is drawn and from which source.
///
/// Resolved once per entry alongside its handles, so assembling the draws
/// names no [GpuMesh] and therefore needs no access to the world.
///
/// Comparable so that two entries drawing the same geometry are recognized as
/// one instanced draw; see [assemble_scene].
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
/// once per distinct [DrawHandlesKey] per frame rather than once per entry.
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
/// its resources by [DrawHandlesKey]; `handles` maps a key to the handles
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
/// The instance range is what keeps a merged draw correct. Each entry names its
/// family's stream and its own record in it, and the records were packed in
/// `visible` order, so the entries of a run read consecutive records of one
/// stream. Instance-stepped attributes are fetched at the instance's ordinal —
/// `firstInstance` plus its index within the draw — so the run reads exactly
/// the records its entries wrote.
///
/// # Panics
///
/// If an entry's key has no handles in `handles`: every key the visible set
/// carries is interned before this is called. If an entry names a family whose
/// stream was not collected into `instances`.
pub(crate) fn assemble_scene(
    scene: &mut Scene,
    visible: &[VisibleEntry],
    pipelines: &[RenderPipelineHandles],
    handles: &HashMap<DrawHandlesKey, EntryHandles>,
    instances: &[Option<(u32, wgpu::Buffer)>],
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
        let instance = &instances[entry.family.as_usize()];
        // The run's records are consecutive within its family's stream: the
        // family appended them in visible order and the run is a slice of that
        // order.
        let base = entry.instance_index;
        let count = (end - start) as u32;

        let first = shape.first;
        let range = if shape.indexed {
            DrawRange::indexed(first..first + shape.count)
                .with_base_vertex(shape.base_vertex as i32)
                .with_instances(base..base + count)
        } else {
            DrawRange::vertices(first..first + shape.count).with_instances(base..base + count)
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
        // The family's own instance stream, bound at the slot it declared. A
        // family with no stream binds nothing, exactly as a caller's pipeline
        // that declares no instance step mode expects.
        if let Some((slot, buffer)) = instance {
            draw = draw.with_vertex_buffer(*slot, buffer);
        }

        scene.push(draw);
        start = end;
    }
}
