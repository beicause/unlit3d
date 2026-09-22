//! The renderer's pipeline abstraction.
//!
//! A [PipelineDesc] is everything the renderer needs to draw with a
//! wgpu render pipeline: the pipeline itself, the bind-group layouts its
//! draws agree with, and -- for a pipeline that reads the renderer's own
//! camera, globals or metadata buffers -- a way to rebuild its global bind
//! group when those buffers change. Nothing here is specific to the built-in
//! unlit shader: the unlit family is registered through the same
//! [Renderer::register_family](crate::Renderer::register_family) a caller's
//! own family uses, and supplies its own factory like anyone else.
//!
//! # Pipeline keys and families
//!
//! A [GpuPipeline] component does not name a compiled pipeline. It carries a
//! [PipelineKey], the entity's request for one family's variant: which
//! concrete pipeline an entity needs depends on the frame's render target and
//! on the mesh's vertex layout, neither of which is known when the entity is
//! spawned. A *family* closes that gap. It pairs a [Variants] cache with a
//! [Specializer] and a [PipelineFactory], queries the world for the entities
//! that carry its key type, and resolves each to a concrete pipeline. The
//! renderer registers every family under the [TypeId](core::any::TypeId) of
//! its key type and drives them all once per frame through
//! [AnyFamily::collect_and_resolve]; a family's variant is compiled and
//! registered the first time a key is seen, and later frames reuse it.
//!
//! The entity, not the renderer, chooses its base descriptor: a
//! [PipelineKey] reports the blueprint ([PipelineKey::base_descriptor]) its
//! variants start from, so one family can draw entities whose base options
//! differ. The blueprint is supplied to [Variants::specialize] lazily and only
//! on a cache miss.
//!
//! Geometry is described by [crate::MeshDesc], which lists vertex buffers
//! tagged with the slot a pipeline expects them in and carries the layout each
//! buffer has. The renderer assumes no vertex layout, so a mesh can carry any
//! combination of attributes and a family can specialize on it at draw time.

use core::hash::Hash;
use core::marker::PhantomData;
use std::sync::Arc;

use unlit_ecs::{Entity, LocalWorld};
use wgpu_unlit_render::mesh::MeshInstance;
use wgpu_unlit_render::resources::ResourceId;
use wgpu_unlit_render::specialize::{
    CachedRenderPipeline, Specializable, Specializer, SpecializerKey, SurfaceKey, Variants,
    VertexBufferLayoutDesc,
};

use crate::components::{
    BoundingSphere, Camera, GpuMaterial, GpuMesh, GpuPipeline, InstanceData, Transform, Transparent,
};

/// A bind group together with the layout it was created from.
#[derive(Clone, Debug)]
pub struct PipelineBinding {
    /// The layout the bind group was built from.
    pub layout: wgpu::BindGroupLayout,
    /// The bind group itself.
    pub bind_group: wgpu::BindGroup,
}

/// Rebuilds a pipeline's global bind group against the renderer's current
/// buffers.
///
/// The renderer calls it whenever a buffer the group was built from is
/// replaced -- the camera, globals or metadata buffer, say. A pipeline that
/// binds no global group passes None instead and is never visited.
pub type GlobalGroupRebuild = Arc<dyn Fn(&RenderResources) -> wgpu::BindGroup + Send + Sync>;

/// The renderer's global buffers, as a rebuild closure sees them.
///
/// These are the buffers every pipeline can rely on the renderer keeping
/// up to date: the camera uniform, the frame globals and the mesh-metadata
/// storage buffer.
#[derive(Clone, Debug)]
pub struct RenderResources {
    /// The camera uniform buffer.
    pub camera: wgpu::Buffer,
    /// The frame-globals uniform buffer.
    pub globals: wgpu::Buffer,
    /// The mesh-metadata storage buffer.
    pub metadata: wgpu::Buffer,
}

/// A pipeline and the layouts its draws agree with.
///
/// This is what a [PipelineFactory] returns and what the renderer registers.
/// Every concrete pipeline in the renderer is described this way, the built-in
/// unlit ones included.
pub struct PipelineDesc {
    /// The compiled render pipeline.
    pub pipeline: wgpu::RenderPipeline,

    /// The bind group bound at
    /// [GLOBAL_GROUP](wgpu_unlit_render::pipeline::GLOBAL_GROUP), together
    /// with how to rebuild it when the renderer's buffers change.
    ///
    /// None for a pipeline that binds nothing at that index -- a shader
    /// with no uniform or storage inputs, say.
    pub global: Option<GlobalBinding>,

    /// The layout a material bind group must be built from, bound at
    /// [MATERIAL_GROUP](wgpu_unlit_render::pipeline::MATERIAL_GROUP).
    ///
    /// None for a pipeline whose draws bind no material group.
    pub material_layout: Option<wgpu::BindGroupLayout>,

    /// The layout a mesh bind group must be built from, bound at
    /// [MESH_GROUP](wgpu_unlit_render::pipeline::MESH_GROUP).
    ///
    /// None for a pipeline whose draws bind no per-mesh group.
    pub mesh_layout: Option<wgpu::BindGroupLayout>,
}

impl core::fmt::Debug for PipelineDesc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PipelineDesc")
            .field("pipeline", &self.pipeline)
            .field("global", &self.global)
            .field("material_layout", &self.material_layout)
            .field("mesh_layout", &self.mesh_layout)
            .finish()
    }
}

/// A pipeline's global bind group and the closure that keeps it current.
#[derive(Clone)]
pub struct GlobalBinding {
    /// The bind group bound for this pipeline's draws.
    pub bind_group: wgpu::BindGroup,
    /// Rebuilds [GlobalBinding::bind_group] from the renderer's current
    /// buffers after one of them is replaced.
    pub rebuild: GlobalGroupRebuild,
}

impl core::fmt::Debug for GlobalBinding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlobalBinding")
            .field("bind_group", &self.bind_group)
            .finish_non_exhaustive()
    }
}

/// The resource id a registered pipeline's global group lives under.
#[derive(Clone)]
pub(crate) struct RegisteredGlobal {
    /// The id in the renderer's resource graph.
    pub(crate) id: ResourceId,
    /// How to rebuild the group.
    pub(crate) rebuild: GlobalGroupRebuild,
}

/// What a [PipelineFactory] may read from the renderer.
pub struct FamilyContext<'a> {
    /// The device the pipeline is compiled on.
    pub device: &'a wgpu::Device,
    /// The renderer's global buffers, for a pipeline that binds them.
    pub resources: &'a RenderResources,
}

/// The base descriptor an entity's pipeline variants start from.
///
/// A [GpuPipeline] component carries a key of this type. It selects the family
/// the entity draws with -- the family registered for this key type -- and
/// supplies the blueprint that family's [Specializer] rewrites into the
/// concrete descriptor. Because the key carries the base, one family can serve
/// entities that begin from different descriptors; because it is the component
/// itself, the renderer never hands out a family handle.
pub trait PipelineKey: Clone + Hash + Eq + 'static {
    /// The specializable pipeline this key selects.
    type Pipeline: Specializable;

    /// The blueprint this key's variant is specialized from. Called only when
    /// the key's variant is compiled for the first time.
    fn base_descriptor(&self) -> <Self::Pipeline as Specializable>::Descriptor;
}

/// Everything that can change which concrete pipeline an entity needs beyond
/// the entity's own [PipelineKey].
///
/// The mesh layout uses the owned [VertexBufferLayoutDesc] from
/// [wgpu_unlit_render::specialize] (an owned mirror of a wgpu vertex buffer
/// layout), because a borrowed vertex layout cannot be stored in a key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DrawKey {
    /// The frame's render target.
    pub surface: SurfaceKey,
    /// The mesh's vertex layout, slot by slot.
    pub vertex_buffers: Vec<(u32, VertexBufferLayoutDesc)>,
}

impl DrawKey {
    /// The key for drawing a mesh into an attachment set with `surface`.
    pub fn for_mesh(surface: SurfaceKey, mesh: &GpuMesh) -> Self {
        Self {
            surface,
            vertex_buffers: mesh.vertex_layout.clone(),
        }
    }
}

/// A concrete pipeline's position in a renderer's own pipeline list.
///
/// A lower value draws before a higher one. Opaque so the index is only ever
/// compared with another [PipelineId], never a plain integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PipelineId(u32);

impl PipelineId {
    /// The id for the pipeline registered at `index`.
    pub(crate) fn new(index: u32) -> Self {
        Self(index)
    }

    /// The id as a `usize`, for indexing the renderer's pipeline list.
    pub(crate) fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// A [SpecializerKey] a family that rewrites nothing uses.
///
/// It pairs the entity's [PipelineKey] with the [DrawKey] the draw resolved to.
/// Distinct keys always produce distinct descriptors, so the secondary cache is
/// skipped and every distinct key compiles its own variant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FamilyKey<K> {
    /// The entity's pipeline key, which supplied the base descriptor.
    pub(crate) key: K,
    /// The resolved draw.
    pub(crate) draw: DrawKey,
}

impl<K: Clone + Hash + Eq + 'static> SpecializerKey for FamilyKey<K> {
    // Every part of the key reaches the descriptor through the base, so
    // distinct keys are distinct descriptors.
    const IS_CANONICAL: bool = true;
    type Canonical = Self;
}

impl<K> From<(K, DrawKey)> for FamilyKey<K> {
    fn from((key, draw): (K, DrawKey)) -> Self {
        Self { key, draw }
    }
}

/// A [Specializer] that rewrites nothing: every key compiles the descriptor its
/// [PipelineKey] reported.
///
/// It is the specializer a one-off custom pipeline uses, where nothing about
/// the draw can change the pipeline. The key type parameter names the
/// [PipelineKey] the family is registered for.
pub struct TrivialSpecializer<K>(PhantomData<fn() -> K>);

impl<K> Default for TrivialSpecializer<K> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<K> Clone for TrivialSpecializer<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for TrivialSpecializer<K> {}

impl<K> core::fmt::Debug for TrivialSpecializer<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TrivialSpecializer").finish_non_exhaustive()
    }
}

impl<K: Clone + Hash + Eq + 'static, T: Specializable> Specializer<T> for TrivialSpecializer<K> {
    type Key = FamilyKey<K>;

    fn specialize(&self, key: FamilyKey<K>, _descriptor: &mut T::Descriptor) -> FamilyKey<K> {
        key
    }
}

/// A [PipelineFactory] for a pipeline that binds nothing beyond what a
/// [PipelineDesc] already carries: it registers the compiled
/// [CachedRenderPipeline] with no global, material or mesh group.
///
/// Paired with [TrivialSpecializer] it is the shortest route from a
/// [RenderPipelineDesc](wgpu_unlit_render::specialize::RenderPipelineDesc) to a
/// registered family, for a caller whose pipeline reads only its vertex
/// buffers.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderPipelineFactory;

impl PipelineFactory<CachedRenderPipeline> for RenderPipelineFactory {
    fn descriptor(
        &self,
        _context: &FamilyContext<'_>,
        value: &CachedRenderPipeline,
    ) -> PipelineDesc {
        PipelineDesc {
            pipeline: value.pipeline.clone(),
            global: None,
            material_layout: None,
            mesh_layout: None,
        }
    }
}

/// Turns a specialized value into the [PipelineDesc] the renderer registers.
pub trait PipelineFactory<T: Specializable> {
    /// The description of the pipeline for `value`, as the renderer should
    /// register it.
    fn descriptor(&self, context: &FamilyContext<'_>, value: &T) -> PipelineDesc;
}

/// A visible entity awaiting its draw command, tagged with everything the
/// scene builder needs to group it.
///
/// Entries are sorted once per frame so a single linear pass can emit every
/// pipeline and material group without intermediate scratch buffers: opaque
/// entries first, keyed by material so shared-state draws stay adjacent, then
/// transparent entries keyed by camera distance so they are drawn
/// back-to-front.
pub(crate) struct VisibleEntry {
    /// The entity the draw came from.
    pub(crate) entity: Entity,
    /// The instance data to pack for the draw.
    pub(crate) instance: MeshInstance,
    /// The concrete pipeline this entity resolves to, an index into the
    /// renderer's pipeline list. Resolved before the sort and never changed by
    /// it.
    pub(crate) pipeline_id: PipelineId,
    /// Groups opaque draws by material, so neighbours share a bind group.
    /// Ignored for transparent entries.
    pub(crate) sort_key: u64,
    /// Distance to the camera, used to order transparent entries back-to-front.
    /// Ignored for opaque entries.
    pub(crate) depth: f32,
    /// True when the entity carries [Transparent].
    pub(crate) transparent: bool,
}

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
    /// The factory that turns a variant into a renderer pipeline.
    factory: F,
    /// registered[v] is the renderer pipeline for core variant v.
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

/// The inputs a family needs to collect and resolve one frame.
///
/// Holding the device and resources here lets a family build the
/// [FamilyContext] its factory sees without borrowing the renderer again.
pub(crate) struct FamilyFrame<'a> {
    /// The world the entities live in.
    pub(crate) world: &'a LocalWorld,
    /// The frame's camera, for the transparent sort.
    pub(crate) camera: &'a Camera,
    /// The frame's render target.
    pub(crate) surface: SurfaceKey,
    /// The camera frustum, for culling.
    pub(crate) frustum: FrustumPlanes,
    /// The device a newly resolved variant is compiled on.
    pub(crate) device: &'a wgpu::Device,
    /// The renderer's global buffers.
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
    /// Resolve every drawing entity into [Self::visible].
    fn collect(&mut self) {
        // Entities with a Transform: the transform places the instance unless
        // an InstanceData component overrides it.
        for (entity, (transform, mesh, pipeline)) in
            self.frame
                .world
                .query::<(&Transform, &GpuMesh, &GpuPipeline<K>)>()
        {
            let instance = match self.frame.world.get::<InstanceData>(entity) {
                Some(data) => MeshInstance::new(data.matrix, data.base_color),
                None => MeshInstance::new(
                    glam::Affine3A::from_mat4(copy_transform(&transform).compute_matrix()),
                    glam::Vec4::new(1.0, 1.0, 1.0, 1.0),
                ),
            };
            if self.culled(entity, &instance) {
                continue;
            }
            self.push(entity, &pipeline, &mesh, instance);
        }

        // Entities with InstanceData but without Transform.
        for (entity, (instance_data, mesh, pipeline)) in
            self.frame
                .world
                .query::<(&InstanceData, &GpuMesh, &GpuPipeline<K>)>()
        {
            if self.frame.world.has::<Transform>(entity) {
                continue;
            }
            let instance = MeshInstance::new(instance_data.matrix, instance_data.base_color);
            if self.culled(entity, &instance) {
                continue;
            }
            self.push(entity, &pipeline, &mesh, instance);
        }
    }

    /// Whether the entity's bounding sphere is outside the camera frustum.
    fn culled(&self, entity: Entity, instance: &MeshInstance) -> bool {
        self.frame
            .world
            .get::<BoundingSphere>(entity)
            .is_some_and(|sphere| {
                let center = affine_from_instance(&instance.model).transform_point3(sphere.center);
                !self.frame.frustum.test_sphere(center, sphere.radius)
            })
    }

    /// Resolve one entity's variant, registering the pipeline on first sight,
    /// and push its [VisibleEntry].
    fn push(
        &mut self,
        entity: Entity,
        pipeline: &GpuPipeline<K>,
        mesh: &GpuMesh,
        instance: MeshInstance,
    ) {
        let draw = DrawKey::for_mesh(self.frame.surface, mesh);
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
            entity,
            instance,
            pipeline_id,
            sort_key,
            depth,
            transparent: self.frame.world.has::<Transparent>(entity),
        });
    }
}

/// A registered family, as the renderer stores it. Type-erased.
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

/// Reconstruct a [glam::Affine3A] from [MeshInstance::model]'s packed columns.
fn affine_from_instance(model: &[glam::Vec4; 3]) -> glam::Affine3A {
    let c0: glam::Vec3A = model[0].truncate().into();
    let c1: glam::Vec3A = model[1].truncate().into();
    let c2: glam::Vec3A = model[2].truncate().into();
    glam::Affine3A {
        matrix3: glam::Mat3A::from_cols(c0, c1, c2),
        translation: glam::Vec3A::new(model[0].w, model[1].w, model[2].w),
    }
}

/// Copy a transform out of its ECS cell.
fn copy_transform(tf: &Transform) -> Transform {
    Transform {
        translation: tf.translation,
        rotation: tf.rotation,
        scale: tf.scale,
    }
}

/// Six frustum planes derived from a clip-space matrix.
pub(crate) struct FrustumPlanes {
    planes: [glam::Vec4; 6],
}

impl FrustumPlanes {
    /// Extract frustum planes from a clip-from-world matrix.
    pub(crate) fn from_clip_from_world(clip_from_world: glam::Mat4) -> Self {
        let m = clip_from_world;
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        Self {
            planes: [
                Self::normalize_plane(r3 + r0),
                Self::normalize_plane(r3 - r0),
                Self::normalize_plane(r3 - r1),
                Self::normalize_plane(r3 + r1),
                Self::normalize_plane(r3 + r2),
                Self::normalize_plane(r3 - r2),
            ],
        }
    }

    fn normalize_plane(row: glam::Vec4) -> glam::Vec4 {
        let len = glam::Vec3::new(row.x, row.y, row.z).length();
        if len > 1e-10 { row / len } else { row }
    }

    /// Test sphere-frustum intersection. Returns true when partially visible.
    pub(crate) fn test_sphere(&self, center: glam::Vec3, radius: f32) -> bool {
        for &plane in &self.planes {
            if plane.x * center.x + plane.y * center.y + plane.z * center.z + plane.w < -radius {
                return false;
            }
        }
        true
    }
}
