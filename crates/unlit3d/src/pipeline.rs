//! The frame's render-pipeline abstraction.
//!
//! Every type here names a *render* pipeline rather than the compute kind, and
//! says so: [RegisteredRenderPipeline], [RenderPipelineKey],
//! [RenderPipelineId], [RenderPipelineFactory] and
//! [GpuRenderPipeline](crate::components::GpuRenderPipeline).
//!
//! A [RegisteredRenderPipeline] is everything the renderer needs to draw with a
//! wgpu render pipeline: the pipeline itself and -- for a pipeline that reads
//! the source's own camera, globals or metadata buffers -- a way to rebuild
//! its global bind group when those buffers change. Nothing here is specific
//! to the built-in unlit shader: the unlit family is registered through the
//! same [register_family](crate::mesh_source::MeshSource::register_family) a
//! caller's own family uses, and supplies its own factory like anyone else.
//!
//! # Render pipeline keys and families
//!
//! A [GpuRenderPipeline](crate::components::GpuRenderPipeline) component does not name a compiled pipeline. It carries a
//! [RenderPipelineKey], the entity's request for one family's variant: which
//! concrete pipeline an entity needs depends on the frame's render target and
//! on the mesh's vertex layout, neither of which is known when the entity is
//! spawned. A *family* closes that gap. It pairs a [Variants](unlit_wgpu::specialize::Variants) cache with a
//! [RenderPipelineFactory], queries the world for the entities that carry its
//! key type, and resolves each to a concrete pipeline. The renderer registers
//! every family under the [TypeId](core::any::TypeId) of its key type;
//! [crate::scene] drives them all once per frame. A family's variant is
//! compiled and registered the first time a key is seen, and later frames
//! reuse it.
//!
//! The entity, not the renderer, chooses its variant: a [RenderPipelineKey]
//! reports -- through [RenderPipelineKey::variant] -- the full, hashable
//! description of the pipeline its entity needs, derived from the entity's
//! own options and from the [DrawContext] the frame hands it. The context is
//! the frame's answer to everything an entity cannot know before it has a
//! mesh: the target, the mesh's vertex layout and index format, and whether a
//! material group is bound. Because the key derives that description rather
//! than carrying a base descriptor, one family serves entities that differ in
//! material or target policy, and the cache key is canonical by construction.
//!
//! Geometry is described by [crate::mesh::MeshDesc], which lists vertex buffers
//! tagged with the slot a pipeline expects them in and carries the layout each
//! buffer has. The renderer assumes no vertex layout, so a mesh can carry any
//! combination of attributes and a family can specialize on it at draw time.

use arrayvec::ArrayVec;
use glam::Affine3A;
use unlit_ecs::{Entity, World};
use unlit_wgpu::mesh::JointMatrix;
use unlit_wgpu::pipeline::{
    CAMERA_BINDING, FRAME_BINDING, GlobalBindings, JOINTS_BINDING, MESH_METADATA_BINDING,
    MORPH_DELTAS_BINDING, MORPH_WEIGHTS_BINDING, global_bind_group_layout,
    supports_storage_buffers,
};
use unlit_wgpu::resources::{ResHandle, Resource, ResourceGraph};
use unlit_wgpu::specialize::{PipelineVariant, SpecializedPipeline, SurfaceKey};
use unlit_wgpu::texel_array::ArrayHandle;

use crate::components::{GpuMaterial, GpuMesh, MorphBinding, MorphWeights, SkinBinding, SkinPose};

pub use unlit_wgpu::resources::Rebuild;

/// The ids of the source's global arrays and buffers.
///
/// These are the resources every pipeline can rely on the renderer keeping up
/// to date: the camera uniform, the frame globals, the mesh-metadata array, and
/// the frame's joint-matrix and morph-weight arrays.
///
/// A factory hands these ids to the [Rebuild] it returns, and the recipe reads
/// the current resource out of the graph each time it runs. That indirection is
/// what makes rebinding lazy: when the source replaces a buffer, its readers
/// only find out at the next maintain, and a [Rebuild] that reads the id never
/// holds a stale buffer handle.
///
/// The three arrays are [`ArrayHandle`]s rather than buffers because which
/// resource holds them follows from the device: a device with storage buffers
/// holds each in one, a device without them -- WebGL2 -- holds the same bytes in
/// a texture. A pipeline that binds them asks the handle for its binding
/// resource and never has to know which it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalResources {
    /// The camera uniform buffer.
    pub camera: ResHandle<wgpu::Buffer>,
    /// The frame-globals uniform buffer.
    pub globals: ResHandle<wgpu::Buffer>,
    /// The mesh-metadata array.
    pub metadata: ResHandle<ArrayHandle>,
    /// The frame's joint matrices: every visible skinned instance's joints,
    /// one array for the whole frame.
    pub joints: ResHandle<ArrayHandle>,
    /// The frame's morph weights: every visible morphed instance's weights, one
    /// array for the whole frame.
    pub morph_weights: ResHandle<ArrayHandle>,
    /// The frame's morph displacements: every morphed mesh's per-vertex
    /// displacements, one array for the whole frame. A mesh names its slice
    /// through its metadata entry's `morph_deltas_offset`.
    pub morph_deltas: ResHandle<ArrayHandle>,
}

impl GlobalResources {
    /// Declare that the group behind `group` was built from every one of these
    /// resources, so replacing any of them marks it dirty.
    ///
    /// A global bind group reads all of them, so it depends on all of them.
    /// They are added one at a time because the uniforms and the arrays are
    /// different kinds.
    pub(crate) fn declare_dependencies(
        &self,
        graph: &mut ResourceGraph,
        group: &ResHandle<wgpu::BindGroup>,
    ) {
        graph.add_dependency(group, &self.camera);
        graph.add_dependency(group, &self.globals);
        graph.add_dependency(group, &self.metadata);
        graph.add_dependency(group, &self.joints);
        graph.add_dependency(group, &self.morph_weights);
        graph.add_dependency(group, &self.morph_deltas);
    }

    /// The layout of the global bind group a pipeline with `bindings` declares.
    ///
    /// The camera and frame globals are always present; the arrays only when
    /// `bindings` names them. The array path is the device's, so it is read
    /// here. A caller's own pipeline builds its layout here and its group
    /// through [`Self::rebuild`], exactly as the built-in unlit family does.
    pub fn layout(&self, device: &wgpu::Device, bindings: GlobalBindings) -> wgpu::BindGroupLayout {
        global_bind_group_layout(device, bindings, !supports_storage_buffers(device))
    }

    /// Assemble the global bind group for `layout` from the current graph.
    ///
    /// # Panics
    ///
    /// If one of the resources `bindings` names is not in the graph.
    pub fn bind_group(
        &self,
        device: &wgpu::Device,
        graph: &ResourceGraph,
        layout: &wgpu::BindGroupLayout,
        bindings: GlobalBindings,
    ) -> wgpu::BindGroup {
        let camera = graph.get(&self.camera).expect("camera buffer exists");
        let globals = graph.get(&self.globals).expect("globals buffer exists");

        // An inline `ArrayVec` rather than a `Vec`: this runs on the frame path
        // whenever a global buffer is replaced, and the optional entries depend
        // on the pipeline, so the group is assembled rather than truncated.
        let mut entries = ArrayVec::<wgpu::BindGroupEntry<'_>, 6>::new();
        entries.push(wgpu::BindGroupEntry {
            binding: CAMERA_BINDING,
            resource: camera.as_entire_binding(),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: FRAME_BINDING,
            resource: globals.as_entire_binding(),
        });
        if bindings.metadata {
            entries.push(wgpu::BindGroupEntry {
                binding: MESH_METADATA_BINDING,
                resource: graph
                    .get(&self.metadata)
                    .expect("metadata array exists")
                    .binding_resource(),
            });
        }
        // The pose arrays are the frame's, not a mesh's: binding them here
        // rather than in the mesh group is what lets two instances of one mesh
        // deform differently.
        if bindings.joints {
            entries.push(wgpu::BindGroupEntry {
                binding: JOINTS_BINDING,
                resource: graph
                    .get(&self.joints)
                    .expect("joints array exists")
                    .binding_resource(),
            });
        }
        if bindings.morphs {
            entries.push(wgpu::BindGroupEntry {
                binding: MORPH_WEIGHTS_BINDING,
                resource: graph
                    .get(&self.morph_weights)
                    .expect("morph weights array exists")
                    .binding_resource(),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: MORPH_DELTAS_BINDING,
                resource: graph
                    .get(&self.morph_deltas)
                    .expect("morph deltas array exists")
                    .binding_resource(),
            });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::global"),
            layout,
            entries: &entries,
        })
    }

    /// A recipe that rebuilds the global bind group for `layout` whenever one
    /// of these resources is replaced.
    ///
    /// The recipe reads each resource out of the graph by id, so a source that
    /// grows or replaces a buffer gets the group rebuilt at the next maintain
    /// without the caller doing anything. This is what a pipeline factory
    /// returns for a pipeline that reads the frame's shared inputs.
    pub fn rebuild(
        &self,
        device: &wgpu::Device,
        layout: wgpu::BindGroupLayout,
        bindings: GlobalBindings,
    ) -> Rebuild {
        let resources = self.clone();
        let device = device.clone();
        Rebuild::new(move |graph| {
            Resource::BindGroup(resources.bind_group(&device, graph, &layout, bindings))
        })
    }
}

/// A pipeline and the global bind group it draws with.
///
/// This is what a [RenderPipelineFactory] returns and what the renderer
/// registers. Every concrete pipeline in the renderer is described this way,
/// the built-in unlit ones included.
pub struct RegisteredRenderPipeline {
    /// The compiled render pipeline.
    pub pipeline: wgpu::RenderPipeline,

    /// How to rebuild the bind group bound at
    /// [GLOBAL_GROUP](unlit_wgpu::pipeline::GLOBAL_GROUP) when the source's
    /// buffers change.
    ///
    /// None for a pipeline that binds nothing at that index -- a shader
    /// with no uniform or storage inputs, say.
    pub global: Option<Rebuild>,
}

impl core::fmt::Debug for RegisteredRenderPipeline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegisteredRenderPipeline")
            .field("pipeline", &self.pipeline)
            .field("global", &self.global)
            .finish()
    }
}

/// The resource id a registered pipeline's global group lives under.
#[derive(Clone)]
pub(crate) struct RegisteredGlobal {
    /// The id in the source's resource graph.
    pub(crate) id: ResHandle<wgpu::BindGroup>,
}

/// What a [RenderPipelineFactory] may read from the source.
pub struct FamilyContext<'a> {
    /// The device the pipeline is compiled on.
    pub device: &'a wgpu::Device,
    /// The ids of the source's global buffers, for a pipeline that binds them.
    pub resources: GlobalResources,
}

/// The pipeline an entity draws with, as the entity's own key describes it.
///
/// A [GpuRenderPipeline](crate::components::GpuRenderPipeline) component carries a key of this type. It selects the family
/// the entity draws with -- the family registered for this key type -- and
/// derives, from the entity's options and the frame's [DrawContext], the full
/// description of the variant it needs. Because the key derives that
/// description rather than reporting a separate base descriptor, the variant
/// it returns is itself the cache key: equal variants compile to equal
/// pipelines, and no canonicalization step is needed.
pub trait RenderPipelineKey: 'static {
    /// The full, hashable description of the pipeline this key resolves to.
    type Variant: PipelineVariant<wgpu::RenderPipeline>;

    /// The variant this key needs for the draw described by `draw`.
    ///
    /// Called once per visible entity per frame, and the result is hashed to
    /// find the compiled pipeline, so it should be cheap and derive every
    /// field the compiled pipeline depends on.
    fn variant(&self, draw: &DrawContext<'_>) -> Self::Variant;
}

/// Everything about a draw that an entity's [RenderPipelineKey] cannot know
/// before the frame resolves it.
///
/// A key is a component, spawned before it has a mesh; the concrete pipeline it
/// needs, though, depends on the mesh's vertex layout and index format, on the
/// frame's render target, and on whether the draw binds a material group. The
/// frame fills this context in and hands it to [RenderPipelineKey::variant], so
/// the entity can derive its variant from what the frame actually draws with.
///
/// Every field is read-only: a key derives its variant from the context, never
/// mutates it.
pub struct DrawContext<'a> {
    /// The frame's render target.
    pub surface: SurfaceKey,
    /// The device the pipeline is compiled on.
    pub device: &'a wgpu::Device,
    /// The world the entity is drawn from, for per-entity components a variant
    /// depends on -- an alpha cutoff, say.
    pub world: &'a World,
    /// The entity being drawn.
    pub entity: Entity,
    /// The mesh the entity is drawn with.
    pub mesh: &'a GpuMesh,
    /// The material the draw binds, or `None` when it binds no material group.
    pub material: Option<&'a GpuMaterial>,
}

/// Where a family's per-instance vertex stream binds and how wide one record
/// is.
///
/// A family declares one through [InstanceData::stream]; the source gives it a
/// reused [InstanceBuffer](unlit_wgpu::instance_stream::InstanceBuffer) shaped
/// by this description and binds it for the family's draws. A stride of zero
/// means the family binds no instance stream at all.
pub use unlit_wgpu::instance_stream::InstanceStreamDesc;

/// Everything a family may read -- and the frame arrays it may append to --
/// while it writes one instance record.
///
/// The record is the family's own: the built-in unlit family builds a
/// [MeshInstance](unlit_wgpu::mesh::MeshInstance), while a caller's family writes whatever its pipeline's
/// instance-step attributes declare, reading the components it needs from the
/// world. Culling resolved only the entity and its placement; what else a draw
/// carries, the family decides here.
///
/// Skin poses and morph weights are two independent things, each with its own
/// frame array and its own pack method. A family that reads neither calls
/// neither; a family that needs one packs only that one.
pub struct InstanceContext<'a> {
    /// The world the entity is drawn from, for per-entity components.
    pub world: &'a World,
    /// The entity the record is for.
    pub entity: Entity,
    /// The entity's world transform, resolved by culling.
    pub world_from_local: Affine3A,
    /// The frame's joint matrices, appended to by [Self::pack_joints].
    joints: &'a mut Vec<JointMatrix>,
    /// The frame's morph weights, appended to by [Self::pack_morph_weights].
    morph_weights: &'a mut Vec<f32>,
}

impl<'a> InstanceContext<'a> {
    /// Build a context for one entity from the frame's two growing arrays.
    pub(crate) fn new(
        world: &'a World,
        entity: Entity,
        world_from_local: Affine3A,
        joints: &'a mut Vec<JointMatrix>,
        morph_weights: &'a mut Vec<f32>,
    ) -> Self {
        Self {
            world,
            entity,
            world_from_local,
            joints,
            morph_weights,
        }
    }

    /// Append this entity's joint matrices to the frame's array and return the
    /// index its slice starts at.
    ///
    /// The matrices come from the [`SkinPose`] of the entity this one names
    /// with a [`SkinBinding`]. A family calls this only for a skinned mesh, and
    /// puts the returned base into the record field its shader reads.
    ///
    /// # Panics
    ///
    /// If the entity names no pose entity, if that entity carries no
    /// [`SkinPose`], or if the pose is empty.
    pub fn pack_joints(&mut self) -> u32 {
        let binding = self
            .world
            .get::<SkinBinding>(self.entity)
            .unwrap_or_else(|| {
                panic!(
                    "a skinned mesh needs a `SkinBinding` naming the entity that holds \
                 its `SkinPose`"
                )
            });
        let pose = self
            .world
            .get::<SkinPose>(binding.pose)
            .unwrap_or_else(|| panic!("the entity a skinned mesh binds to carries no `SkinPose`"));
        assert!(
            !pose.matrices.is_empty(),
            "a skinned mesh's pose needs at least one joint matrix"
        );
        let base = self.joints.len() as u32;
        self.joints.extend_from_slice(&pose.matrices);
        base
    }

    /// Append this entity's morph weights to the frame's array and return the
    /// index its slice starts at.
    ///
    /// The weights come from the [`MorphWeights`] of the entity this one names
    /// with a [`MorphBinding`]. `targets` is the mesh's target count, which
    /// the weights must match. A family calls this only for a morphed mesh, and
    /// puts the returned base into the record field its shader reads.
    ///
    /// # Panics
    ///
    /// If the entity names no weights entity, if that entity carries no
    /// [`MorphWeights`], or if the weight count does not match `targets`.
    pub fn pack_morph_weights(&mut self, targets: u32) -> u32 {
        let binding = self
            .world
            .get::<MorphBinding>(self.entity)
            .unwrap_or_else(|| {
                panic!(
                    "a mesh with morph targets needs a `MorphBinding` naming the entity \
                 that holds its `MorphWeights`"
                )
            });
        let weights = self
            .world
            .get::<MorphWeights>(binding.weights)
            .unwrap_or_else(|| {
                panic!("the entity a morphed mesh binds to carries no `MorphWeights`")
            });
        assert_eq!(
            weights.weights.len() as u32,
            targets,
            "a mesh's morph weights must hold one weight per morph target"
        );
        let base = self.morph_weights.len() as u32;
        self.morph_weights.extend_from_slice(&weights.weights);
        base
    }
}

/// The per-instance vertex data one family's draws read.
///
/// A family owns its own instance stream: every frame the source packs one
/// record per visible instance into a buffer shaped by [Self::stream] and binds
/// it at the declared slot. That is what lets a caller's family carry its own
/// per-instance data -- a tint, a transform palette, a material parameter --
/// without sharing a record layout or a buffer with the built-in unlit family,
/// and without the built-in family having any privilege over it.
///
/// A family that reads no per-instance state passes the unit type.
pub trait InstanceData: 'static {
    /// Where the family's stream binds and how wide one record is.
    ///
    /// Called once when the family is registered, so it should be constant.
    fn stream(&self) -> InstanceStreamDesc;

    /// Write one record into `out`, which is exactly
    /// `stream().array_stride` bytes long.
    ///
    /// Called once per visible instance per frame, in visible order.
    fn write(&mut self, context: &mut InstanceContext<'_>, out: &mut [u8]);
}

/// A family that binds no per-instance stream.
impl InstanceData for () {
    fn stream(&self) -> InstanceStreamDesc {
        InstanceStreamDesc {
            slot: 0,
            array_stride: 0,
        }
    }

    fn write(&mut self, _context: &mut InstanceContext<'_>, _out: &mut [u8]) {}
}

/// A concrete pipeline's position in a source's own pipeline list.
///
/// A lower value draws before a higher one. Opaque so the index is only ever
/// compared with another [RenderPipelineId], never a plain integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderPipelineId(u32);

impl RenderPipelineId {
    /// The id for the pipeline registered at `index`.
    pub(crate) fn new(index: u32) -> Self {
        Self(index)
    }

    /// The id as a `usize`, for indexing the source's pipeline list.
    pub(crate) fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Turns a specialized pipeline into the [RegisteredRenderPipeline] the renderer
/// registers.
///
/// The type parameter is the descriptor the family specializes, so a factory
/// reads the layouts a variant declares from the descriptor the pipeline was
/// compiled from.
pub trait RenderPipelineFactory<D> {
    /// The description of the pipeline for `value`, as the renderer should
    /// register it.
    fn descriptor(
        &self,
        context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, D>,
    ) -> RegisteredRenderPipeline;
}
