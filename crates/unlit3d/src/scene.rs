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
use unlit_ecs::{LocalWorld, TypeIdHashMap};
use wgpu_unlit_render::specialize::{Specializable, Specializer, SurfaceKey, Variants};

use wgpu_unlit_render::pipeline::{GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_GROUP};
use wgpu_unlit_render::scene::{
    DrawEntry, DrawRange, MAX_VERTEX_BUFFERS, Scene, VertexBufferBinding,
};

use crate::bounds::FrustumPlanes;
use crate::components::{Camera, GpuMaterial, GpuMesh, GpuPipeline, ZSortedDrawing};
use crate::culling::{VisibleMesh, collect_visible};
use crate::pipeline::{
    DrawKey, FamilyContext, PipelineDesc, PipelineFactory, PipelineId, PipelineKey, RenderResources,
};

/// A family: a specialization cache plus the factory that turns each variant
/// into a [PipelineDesc].
///
/// The core variant index a draw resolves to is also its index into
/// registered, so the two lists grow in lockstep. The key type parameter is
/// the [PipelineKey] the family is registered for and the component its
/// queries fetch.
pub(crate) struct Family<T, S, F, K>
where
    T: Specializable + 'static,
    S: Specializer<T>,
    F: PipelineFactory<T>,
{
    /// The variant cache, which owns the specializer and the device.
    variants: Variants<T, S>,
    /// The factory that turns a variant into a wgpu render pipeline.
    factory: F,
    /// registered[v] is the wgpu render pipeline for core variant v.
    registered: Vec<PipelineId>,
    /// Names the key type without owning one.
    _key: PhantomData<fn() -> K>,
}

impl<T, S, F, K> Family<T, S, F, K>
where
    T: Specializable + 'static,
    S: Specializer<T>,
    F: PipelineFactory<T>,
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
    pub(crate) pipeline_id: PipelineId,
    /// Groups opaque draws by material, so neighbours share a bind group.
    /// Ignored for z-sorted entries.
    pub(crate) sort_key: u64,
    /// Distance to the camera, used to order z-sorted entries back-to-front.
    /// Ignored for opaque entries.
    pub(crate) depth: f32,
    /// True when the entity carries [ZSortedDrawing].
    pub(crate) z_sorted: bool,
}

/// The inputs a family needs to collect and resolve one frame.
///
/// Holding the device and resources here lets a family build the
/// [FamilyContext] its factory sees without borrowing the source again.
pub(crate) struct FamilyFrame<'a> {
    /// The world the entities live in.
    pub(crate) world: &'a LocalWorld,
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
struct Resolver<'a, 'f, T, S, F, K>
where
    T: Specializable + 'static,
    S: Specializer<T>,
    F: PipelineFactory<T>,
    K: PipelineKey<Pipeline = T>,
    S::Key: From<(K, DrawKey)>,
{
    frame: &'a FamilyFrame<'f>,
    variants: &'a mut Variants<T, S>,
    factory: &'a F,
    registered: &'a mut Vec<PipelineId>,
    visible: &'a mut Vec<VisibleEntry>,
    register: &'a mut dyn FnMut(PipelineDesc) -> PipelineId,
    _key: PhantomData<fn() -> K>,
}

impl<T, S, F, K> Resolver<'_, '_, T, S, F, K>
where
    T: Specializable + 'static,
    S: Specializer<T>,
    F: PipelineFactory<T>,
    K: PipelineKey<Pipeline = T>,
    S::Key: From<(K, DrawKey)>,
{
    /// Resolve every visible entity this family draws into [Self::visible].
    ///
    /// The frame's meshes were already culled and their placement resolved,
    /// so this only picks out the ones whose pipeline belongs to the family.
    fn collect(&mut self) {
        for &candidate in self.frame.meshes {
            let entity = candidate.entity;
            // An entity from another family carries a different key type, so
            // it simply is not found here.
            let Some(pipeline) = self.frame.world.get::<GpuPipeline<K>>(entity) else {
                continue;
            };
            // The mesh is what a draw key is derived from; a candidate whose
            // GpuMesh vanished would have no draw.
            let Some(mesh) = self.frame.world.get::<GpuMesh>(entity) else {
                continue;
            };
            self.push(candidate, &pipeline, &mesh);
        }
    }

    /// Resolve one candidate's variant, registering the pipeline on first
    /// sight, and push its [VisibleEntry].
    fn push(&mut self, mesh: VisibleMesh, pipeline: &GpuPipeline<K>, gpu_mesh: &GpuMesh) {
        let entity = mesh.entity;
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

        let centre = glam::Vec3A::new(
            instance.model[0].w,
            instance.model[1].w,
            instance.model[2].w,
        );
        let depth = (centre - glam::Vec3A::from(self.frame.camera.position)).length();
        let sort_key = match self.frame.world.get::<GpuMaterial>(entity) {
            Some(material) => material.sort_key(),
            None => 0,
        };
        self.visible.push(VisibleEntry {
            mesh,
            pipeline_id,
            sort_key,
            depth,
            z_sorted: self.frame.world.has::<ZSortedDrawing>(entity),
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
        register: &mut dyn FnMut(PipelineDesc) -> PipelineId,
    );
}

impl<T, S, F, K> AnyFamily for Family<T, S, F, K>
where
    T: Specializable + 'static,
    S: Specializer<T>,
    F: PipelineFactory<T>,
    K: PipelineKey<Pipeline = T>,
    S::Key: From<(K, DrawKey)>,
{
    fn collect_and_resolve(
        &mut self,
        frame: &FamilyFrame<'_>,
        visible: &mut Vec<VisibleEntry>,
        register: &mut dyn FnMut(PipelineDesc) -> PipelineId,
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
    pub(crate) register: &'a mut dyn FnMut(PipelineDesc) -> PipelineId,
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
    world: &LocalWorld,
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

    collect_visible(world, &frustum, meshes);
    visible.clear();
    let frame = FamilyFrame {
        world,
        meshes,
        camera,
        surface,
        device,
        resources,
    };
    for family in families.values_mut() {
        family.collect_and_resolve(&frame, visible, register);
    }

    // Sort: not z-sorted before z-sorted, then by pipeline, then by the key that
    // matters for that kind. Opaque draws are keyed by material so neighbours
    // share a bind group; z-sorted ones by camera distance so they are
    // composited back-to-front.
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

/// A registered pipeline's GPU handles, indexed by [`PipelineId`].
pub(crate) struct PipelineHandles {
    /// The compiled pipeline.
    pub(crate) pipeline: wgpu::RenderPipeline,
    /// The bind group bound at the global index, when the pipeline binds one.
    pub(crate) global: Option<wgpu::BindGroup>,
}

/// The shape of one draw: what is drawn and from which source.
///
/// Resolved once per entry alongside its handles, so assembling the draws
/// names no [`GpuMesh`] and therefore needs no access to the world.
#[derive(Clone, Copy)]
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

/// One visible entry's resource-graph handles, resolved before the scene is
/// assembled.
///
/// The handles are cloned out of the graph once per entry and owned here, so
/// assembling the draws touches neither the graph nor the world — and the
/// assembled [`Scene`] borrows nothing.
pub(crate) struct EntryHandles {
    /// The mesh's bind group, if it has one.
    pub(crate) mesh_bg: Option<wgpu::BindGroup>,
    /// The material's bind group, if the entity carries a material.
    pub(crate) material_bg: Option<wgpu::BindGroup>,
    /// The mesh's vertex buffers, each with the slot it binds to.
    pub(crate) vertex_buffers: ArrayVec<VertexBufferBinding, MAX_VERTEX_BUFFERS>,
    /// The index buffer and its format, when the mesh is indexed.
    pub(crate) index_buffer: Option<(wgpu::Buffer, wgpu::IndexFormat)>,
    /// What the draw reads: whether it is indexed and how many indices or
    /// vertices.
    pub(crate) shape: DrawShape,
}

/// Append one draw per visible entry to `scene`.
///
/// The entries have already been culled, resolved and sorted and their
/// resource-graph handles already cloned into [`EntryHandles`], so assembling
/// the draws is a linear pass over slices that names nothing the world or the
/// graph owns.
///
/// The draws are appended in `visible` order, so the sort that put neighbours
/// on the same pipeline and bind groups is what keeps recording's state changes
/// few.
pub(crate) fn assemble_scene(
    scene: &mut Scene,
    visible: &[VisibleEntry],
    pipelines: &[PipelineHandles],
    handles: &[EntryHandles],
    instance_buffer: &wgpu::Buffer,
) {
    for (draw_idx, entry) in visible.iter().enumerate() {
        let handle = &handles[draw_idx];
        let pipeline = &pipelines[entry.pipeline_id.as_usize()];

        let instance_range = (draw_idx as u32)..(draw_idx as u32 + 1);
        let first = handle.shape.first;
        let range = if handle.shape.indexed {
            DrawRange::indexed(first..first + handle.shape.count)
                .with_base_vertex(handle.shape.base_vertex as i32)
                .with_instances(instance_range)
        } else {
            DrawRange::vertices(first..first + handle.shape.count).with_instances(instance_range)
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
    }
}
