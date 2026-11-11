//! Load glTF documents and patch them into another world's
//! [`MeshSource`](crate::mesh_source::MeshSource): upload textures, materials
//! and meshes into an existing resource graph, unload them again, and spawn
//! entities that draw the document's default scene.
//!
//! [`LitGltf`] owns only the slim, replayable state of a loaded document —
//! the parsed [`gltf::Document`], its decoded buffers and images, and each
//! node's world-space transform — plus nothing else. It holds no
//! [`World`](unlit_ecs::World), no [`MeshSource`](crate::mesh_source::MeshSource)
//! and no GPU resources of its own. Everything GPU lives in
//! the graph of the `MeshSource` the caller patches, so a document can be
//! inserted into any number of worlds, and each insertion keeps the handles
//! that unload it again.
//!
//! A primitive carrying `KHR_draco_mesh_compression` is decoded with
//! `draco-core` while the document is loaded: its stream is written back as
//! ordinary buffer views and accessors, so the document is validated normally
//! and every later reader -- morph targets, skins and animations included --
//! sees only accessors. A document without a compressed primitive keeps its
//! original validation error. Decoding happens once at import rather than on
//! each mesh insertion.
//!
//! # Usage
//!
//! ```rust,no_run
//! use unlit3d::gltf::LitGltf;
//! # fn run() -> Result<(), gltf::Error> {
//! let gltf = LitGltf::load("model.glb")?;
//! # Ok(()) }
//! ```
//!
//! The document is inserted in one call, which returns every handle that keeps
//! what it uploaded alive:
//!
//! ```rust,no_run
//! # use unlit3d::gltf::LitGltf;
//! # use unlit3d::prelude::MeshSource;
//! # use unlit_ecs::ScopeMut;
//! # fn run(source: &mut MeshSource, world: &mut ScopeMut<'_>) -> Result<(), gltf::Error> {
//! let gltf = LitGltf::load("model.glb")?;
//! let resources = gltf.insert_resources(source, &world.as_scope());
//! let entities = gltf.spawn_default_scene(world, &resources);
//! // ... render ...
//! // Giving the handles up makes their resources collectable.
//! drop(resources);
//! # Ok(()) }
//! ```
//!
//! A caller that wants to insert one kind of resource at a time — to reuse a
//! texture between documents, say — can still call
//! [`LitGltf::insert_images`], [`LitGltf::insert_materials`] and
//! [`LitGltf::insert_meshes`] directly and build a [`GltfResources`] of its
//! own.
//!
//! A document's collections are addressed by typed ids — [`ImageId`],
//! [`MaterialId`], [`MeshId`], [`PrimitiveId`], [`NodeId`] and
//! [`AnimationId`] — so a node index cannot be handed to an animation or a
//! mesh by mistake. The `images()`, `materials()`, `meshes()`,
//! `primitives()`, `nodes()` and `animations()` iterators yield the ids a
//! document carries.
//!
//! Spawning entities never spawns a camera: the renderer draws with the first
//! [`Camera`](crate::components::Camera) in the world, so one belongs to the
//! caller.
//!
//! # Texture formats
//!
//! An image is uploaded in the GPU format closest to the pixels the loader
//! decoded, so a document's textures keep their channels and their precision
//! instead of all being widened to RGBA8: 8-bit layouts upload as
//! `R8Unorm`/`Rg8Unorm`/`Rgba8UnormSrgb`, 16-bit ones as the half-float
//! formats of the same width, and 32-bit float ones as `Rgba32Float`. Two
//! shapes have no format of their own and widen by one channel: the
//! three-channel layouts, because neither an sRGB nor a float format comes in
//! three channels, and a 32-bit float one for the same reason.
//!
//! A one- or two-channel image is the interesting case: WebGPU has no
//! luminance format and no component swizzle, so a grayscale texture samples
//! as `(l, 0, 0, 1)` — the red channel carrying the luma — unless the shader
//! expands it. The built-in lit shader does, under
//! `BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA`, which
//! [`LitGltf::pipeline_key`] sets for exactly those uploads: the texel
//! fans out to RGB (and its second channel to alpha) and the luminance is
//! decoded from sRGB, which is the encoding the glTF spec asks of a base-color
//! texture and the one an RGB upload would have been decoded by the sampler.
//! A caller writing its own shader either sets the same flags or takes the
//! luma from the red channel as it is.
//!
//! `Rgba32Float` is also the only upload a device may refuse to filter: it is
//! `unfilterable-float` without `Features::FLOAT32_FILTERABLE`, and then the
//! material that samples it binds a non-filtering sampler — a different
//! bind-group layout from the filtering one. Every handle carries the answer
//! in [`GltfImage::filtering`], and the two derive it the same way, so a
//! material always fits the pipeline its primitive drew with.
//!
//! # Supported subset
//!
//! The unlit renderer draws static meshes with a base-color texture and an
//! instance tint, and composites a material whose `alphaMode` is `BLEND` —
//! straight alpha, drawn z-sorted so overlapping surfaces layer correctly.
//! A `MASK` material cuts its fragments off below its `alphaCutoff` instead,
//! so it draws binary coverage: opaque where the texel is opaque enough and
//! absent everywhere else, blending with nothing. A primitive carrying both
//! `JOINTS_0` and `WEIGHTS_0` uploads its actual joint stream and draws
//! skinned; [`LitGltf::skin_pose`] turns the node's skin into the
//! [`SkinPose`](crate::components::SkinPose) its joints deform by, and spawning
//! such a node creates the pose entity its mesh binds to. A primitive whose mesh
//! declares a morph target that displaces positions uploads those displacements
//! and spawns with a [`MorphBinding`](crate::components::MorphBinding) to a
//! [`MorphWeights`](crate::components::MorphWeights) entity holding the node's
//! starting weights — see [`LitGltf::morph_weights`] — so writing that entity is how
//! a caller morphs the mesh. A `doubleSided` material rasterizes its back
//! faces instead of culling them, so its geometry draws from either side.
//! A document's animations are sampled on demand:
//! [`LitGltf::animation_count`], [`LitGltf::animation_name`] and
//! [`LitGltf::animation_duration`] describe what the document carries, and
//! [`LitGltf::apply_animation`] samples one at a time into the entities a
//! spawn produced — moving each animated node's children with it, rebuilding
//! the skin poses and writing the morph weights the clip targets. Nothing
//! plays by itself; the caller picks the time.
//! A primitive that carries normals uploads them, so a lit surface is shaded by
//! the normals the document gives it rather than by the ones screen-space
//! derivatives infer from its positions. A primitive compressed with
//! `KHR_draco_mesh_compression` is decoded on import into the same accessors,
//! so morph target displacements, skins and animations apply to it exactly as
//! to an uncompressed one; its `TRIANGLE_STRIP` mode, which the extension
//! permits, is normalized to a triangle list as the stream is decoded. Only
//! the `TRIANGLES` primitive mode is drawn for an ordinary primitive.
//! Everything else a glTF document can carry is ignored: no tangents. A mesh
//! whose primitive uses techniques outside this subset still uploads and
//! spawns — it just renders without them.
//!
//! Every handle returned here keeps what it names alive: the next
//! [`MeshSource::maintain`](crate::mesh_source::MeshSource::maintain) collects
//! a resource once the last handle to it — and the last resource depending on
//! it — is gone, so giving up a handle is the unload.
//!
//! `LitGltf::load("model.glb")` in the examples above opens a local file —
//! see [`LitGltf::load`] and [`LitGltf::from_bytes`] for how a document
//! reaches the module in the first place.

use std::path::Path;

use core::mem::size_of;

use crate::components::{GpuMaterial, GpuMesh, Transform};
use crate::lit::LitPipelineKey;
use unlit_ecs::prelude::Entity;
use unlit_wgpu::resources::{ResHandle, TextureView};

mod animation;
mod draco;
#[cfg(test)]
mod draco_tests;
mod image;
mod material;
mod playback;
mod scene;
mod skin;
mod spawn;
mod streams;
#[cfg(test)]
mod tests;
mod upload;

use self::scene::{
    decompose, node_children, node_locals, node_order, node_parents, node_world_matrices,
    nodes_morph_targets,
};

/// The cutoff the glTF spec gives a `MASK` material that leaves `alphaCutoff`
/// out.
const DEFAULT_ALPHA_CUTOFF: f32 = 0.5;

/// The widest texel an upload format uses: four 32-bit float channels.
const MAX_TEXEL_BYTES: usize = 4 * size_of::<f32>();

/// An index into one of a glTF document's collections.
///
/// Each kind of thing a document holds has its own id type, so a node index
/// cannot be passed where an animation or an image is expected. An id is a
/// zero-based position; `LitGltf`'s iterator methods yield the ids a
/// document actually carries.
macro_rules! document_index {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(usize);

        impl $name {
            /// The id at a zero-based position in the document.
            #[must_use]
            pub const fn new(index: usize) -> Self {
                Self(index)
            }

            /// The zero-based position in the document.
            #[must_use]
            pub const fn index(self) -> usize {
                self.0
            }
        }
    };
}

document_index!(
    /// An index into a document's images.
    ImageId
);
document_index!(
    /// An index into a document's materials.
    MaterialId
);
document_index!(
    /// An index into a document's meshes.
    MeshId
);
document_index!(
    /// An index into one mesh's primitives.
    PrimitiveId
);
document_index!(
    /// An index into a document's nodes.
    NodeId
);
document_index!(
    /// An index into a document's animations.
    AnimationId
);

/// A loaded glTF document, ready to be patched into another world.
///
/// Construct with [`Self::load`] or [`Self::from_bytes`]. The document is
/// parsed and its buffers and images decoded eagerly; no GPU resource exists
/// until an `insert_*` call uploads one into some world's
/// [`MeshSource`](crate::mesh_source::MeshSource).
pub struct LitGltf {
    document: gltf::Document,
    buffers: Vec<gltf::buffer::Data>,
    images: Vec<gltf::image::Data>,
    /// World-space transform of every node, indexed by node index.
    node_transforms: Vec<Transform>,
    /// World-space matrix of every node, indexed by node index.
    ///
    /// Kept beside the decomposed transform because animation recomposes the
    /// matrices from the node's own local transform and its parents, and a
    /// shear in that chain has to survive the recomposition.
    node_matrices: Vec<glam::Mat4>,
    /// The parent of every node, indexed by node index; `None` at a root.
    ///
    /// glTF nodes list their children rather than their parent, so the table
    /// is built by walking every `children` list once. Animation recomposes
    /// the world matrices through it.
    parents: Vec<Option<usize>>,
    /// The local-space transform of every node, indexed by node index.
    ///
    /// This is what an animation channel overwrites: a channel targets one
    /// node's local translation, rotation or scale, and the world matrices are
    /// then recomposed from these locals and [`Self::parents`].
    locals: Vec<Transform>,
    /// The children of every node, indexed by node index.
    ///
    /// An animation walks it to mark what a moved node carries.
    children: Vec<Vec<usize>>,
    /// Every node index, parent before child: the order the world matrices are
    /// recomposed in, so a node always sees its parent's matrix.
    order: Vec<usize>,
    /// How many morph targets each node's mesh displaces, indexed by node index.
    ///
    /// A weights channel is as long as its targets, so a frame needs the count
    /// to slice its values; keeping the count is what lets it read a node's
    /// targets without building the node's weight vector.
    morph_targets: Vec<usize>,
    /// Each animation's channels, decoded on first use, indexed by animation
    /// index.
    ///
    /// Decoding a channel's accessors yields an iterator, and sampling needs
    /// random access to the keyframes, so they are collected once here rather
    /// than on every frame that plays the clip.
    clips: Vec<std::sync::OnceLock<Vec<animation::Channel>>>,
}

/// A texture uploaded from a glTF image.
///
/// Produced by [`LitGltf::insert_resources`] or
/// [`LitGltf::insert_image`] / [`LitGltf::insert_images`]. Dropping the
/// handle makes the texture collectable by the next maintain.
#[derive(Clone, Debug)]
pub struct GltfImage {
    /// The image in the glTF document this handle came from.
    pub image: ImageId,
    /// The uploaded texture holding the image's decoded pixels.
    pub texture: ResHandle<wgpu::Texture>,
    /// A default view of that texture, for callers that sample it directly.
    pub view: ResHandle<TextureView>,
    /// Whether the texture may be sampled with a filtering sampler.
    ///
    /// An image is uploaded in the GPU format closest to its own pixels, and
    /// some of those — 32-bit float, unless the device has
    /// `Features::FLOAT32_FILTERABLE` — can only be bound to a non-filtering
    /// sampler. A material built from this image has to declare the matching
    /// bind-group layout, which is what this flag decides.
    pub filtering: bool,
}

/// A material bind group built from a glTF material's base-color texture.
///
/// Produced by [`LitGltf::insert_resources`] or
/// [`LitGltf::insert_material`] / [`LitGltf::insert_materials`]. Dropping
/// the handle makes the bind group collectable; the view and sampler it names
/// are the caller's own handles.
#[derive(Clone, Debug)]
pub struct GltfMaterial {
    /// The material in the glTF document this handle came from.
    pub material: MaterialId,
    /// The bind group that samples the base-color texture.
    pub bind_group: GpuMaterial,
    /// The texture view the bind group samples.
    pub view: ResHandle<TextureView>,
    /// The sampler the bind group uses.
    pub sampler: ResHandle<wgpu::Sampler>,
}

/// A mesh uploaded from one glTF primitive.
///
/// Produced by [`LitGltf::insert_resources`] or [`LitGltf::insert_mesh`] /
/// [`LitGltf::insert_meshes`]. Dropping the handle makes the mesh
/// collectable by the next maintain.
///
/// The key is derived from the primitive's own attributes and is the key the
/// mesh was uploaded with; spawning an entity from this handle reuses the same
/// key, so the drawn variant always matches the uploaded vertex layout.
#[derive(Clone, Debug)]
pub struct GltfMesh {
    /// The mesh in the glTF document this primitive came from.
    pub mesh_index: MeshId,
    /// The primitive inside that mesh.
    pub primitive_index: PrimitiveId,
    /// The key the mesh was uploaded with.
    pub key: LitPipelineKey,
    /// The uploaded GPU mesh.
    pub mesh: GpuMesh,
}

/// The entities a spawned glTF node consists of.
///
/// Produced by [`LitGltf::spawn_node`] / [`LitGltf::spawn_default_scene`]
/// and given back to [`LitGltf::apply_animation`], which drives the node's
/// transform, its [`SkinPose`](crate::components::SkinPose) and its
/// [`MorphWeights`](crate::components::MorphWeights) through the entities this
/// names.
#[derive(Clone, Debug)]
pub struct GltfNode {
    /// The node in the glTF document these entities draw.
    pub node: NodeId,
    /// One entity per primitive of the node's mesh; empty for a node with no
    /// mesh.
    pub entities: Vec<Entity>,
    /// The entity holding the node's [`SkinPose`](crate::components::SkinPose),
    /// when the node is skinned.
    pub pose: Option<Entity>,
    /// The entity holding the node's [`MorphWeights`](crate::components::MorphWeights),
    /// when the node's mesh has morph targets.
    pub weights: Option<Entity>,
}

/// Every resource one document uploaded, in document order.
///
/// Produced by [`LitGltf::insert_resources`], which uploads the whole
/// document in one call, and taken by [`LitGltf::spawn_node`] and
/// [`LitGltf::spawn_default_scene`] to draw it. Each vector is in document
/// order — `images[i]` came from image `i`, `materials[m]` from material
/// `m` (`None` when it names no base-color texture), and `meshes` holds every
/// primitive of every mesh, document-mesh-major and then primitive-minor —
/// and every handle carries the typed id of the document item it came from.
///
/// Holding the struct is what keeps the uploaded resources alive: dropping it
/// lets the next [`MeshSource::maintain`](crate::mesh_source::MeshSource::maintain)
/// collect them.
///
/// A caller that inserts the pieces itself — to share textures between
/// documents, say — builds one of these to spawn with.
#[derive(Clone, Debug, Default)]
pub struct GltfResources {
    /// Every uploaded image, indexed by document image.
    pub images: Vec<GltfImage>,
    /// Every uploaded material, indexed by document material.
    pub materials: Vec<Option<GltfMaterial>>,
    /// Every uploaded primitive, document-mesh-major then primitive-minor.
    pub meshes: Vec<GltfMesh>,
}

impl LitGltf {
    /// Load a glTF document from `path`, decoding its buffers and images.
    ///
    /// External URIs resolve against the file's directory. Fails with
    /// [`gltf::Error`] when the file cannot be read or parsed, or a referenced
    /// buffer or image cannot be decoded.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, gltf::Error> {
        let path = path.as_ref();
        let base = path.parent().unwrap_or_else(|| Path::new("./"));
        let imported = gltf::import(path).or_else(|error| {
            Self::import_draco(
                error,
                || {
                    gltf::Gltf::from_reader_without_validation(std::io::BufReader::new(
                        std::fs::File::open(path)?,
                    ))
                },
                Some(base),
            )
        })?;
        Ok(Self::from_parts(imported.0, imported.1, imported.2))
    }

    /// Parse a glTF document from `bytes`.
    ///
    /// The bytes may be either glTF JSON or a binary GLB container. Buffers
    /// arrive as embedded data URIs or, for a GLB, the `BIN` chunk; images
    /// must be embedded too (as data URIs or buffer views), because there is
    /// no base URI to resolve external references against.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, gltf::Error> {
        let (document, buffers, images) = gltf::import_slice(bytes).or_else(|error| {
            Self::import_draco(
                error,
                || gltf::Gltf::from_slice_without_validation(bytes),
                None,
            )
        })?;
        Ok(Self::from_parts(document, buffers, images))
    }

    /// Try to repair a document that failed validation by decoding its Draco
    /// primitives back into ordinary accessors.
    ///
    /// glTF validation rejects the Draco extension and the accessors whose data
    /// lives only in the compressed stream. Re-parsing without validation,
    /// decoding each stream into real buffer views and validating again lets
    /// the rest of the loader read one uniform document. A document with no
    /// compressed primitive keeps the original error; a failure to decode is
    /// reported as a validation error naming the extension.
    fn import_draco(
        error: gltf::Error,
        parse: impl FnOnce() -> Result<gltf::Gltf, gltf::Error>,
        base: Option<&Path>,
    ) -> Result<
        (
            gltf::Document,
            Vec<gltf::buffer::Data>,
            Vec<gltf::image::Data>,
        ),
        gltf::Error,
    > {
        if !matches!(error, gltf::Error::Validation(_)) {
            return Err(error);
        }
        let gltf::Gltf { document, blob } = parse()?;
        let mut buffers = gltf::import_buffers(&document, base, blob)?;
        let mut root = document.into_json();
        if !draco::fill_accessors(&mut root, &mut buffers)? {
            return Err(error);
        }
        let document = gltf::Document::from_json(root)?;
        let images = gltf::import_images(&document, base, &buffers)?;
        Ok((document, buffers, images))
    }

    fn from_parts(
        document: gltf::Document,
        buffers: Vec<gltf::buffer::Data>,
        images: Vec<gltf::image::Data>,
    ) -> Self {
        let parents = node_parents(&document);
        let locals = node_locals(&document);
        let children = node_children(&document);
        let order = node_order(&children, &parents);
        let node_matrices = node_world_matrices(&document, &parents, &order);
        let node_transforms = node_matrices.iter().map(|&m| decompose(m)).collect();
        let morph_targets = nodes_morph_targets(&document);
        let clips = std::iter::repeat_with(std::sync::OnceLock::new)
            .take(document.animations().count())
            .collect();
        Self {
            document,
            buffers,
            images,
            node_transforms,
            node_matrices,
            parents,
            locals,
            children,
            order,
            morph_targets,
            clips,
        }
    }

    /// The ids of every image in the document, in document order.
    pub fn images(&self) -> impl Iterator<Item = ImageId> {
        (0..self.document.images().len()).map(ImageId::new)
    }

    /// The ids of every material in the document, in document order.
    pub fn materials(&self) -> impl Iterator<Item = MaterialId> {
        (0..self.document.materials().len()).map(MaterialId::new)
    }

    /// The ids of every mesh in the document, in document order.
    pub fn meshes(&self) -> impl Iterator<Item = MeshId> {
        (0..self.document.meshes().len()).map(MeshId::new)
    }

    /// The ids of every primitive of `mesh`, in document order.
    pub fn primitives(&self, mesh: MeshId) -> impl Iterator<Item = PrimitiveId> {
        (0..self.mesh(mesh.index()).primitives().count()).map(PrimitiveId::new)
    }

    /// The ids of every node in the document, in document order.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> {
        (0..self.document.nodes().len()).map(NodeId::new)
    }

    /// The ids of every animation in the document, in document order.
    pub fn animations(&self) -> impl Iterator<Item = AnimationId> {
        (0..self.document.animations().count()).map(AnimationId::new)
    }

    fn node(&self, node: usize) -> gltf::scene::Node<'_> {
        self.document
            .nodes()
            .nth(node)
            .expect("glTF node index in bounds")
    }

    fn mesh(&self, mesh: usize) -> gltf::Mesh<'_> {
        self.document
            .meshes()
            .nth(mesh)
            .expect("glTF mesh index in bounds")
    }

    fn primitive(&self, mesh: usize, primitive: usize) -> gltf::Primitive<'_> {
        self.mesh(mesh)
            .primitives()
            .nth(primitive)
            .expect("glTF primitive index in bounds")
    }

    fn material(&self, material: usize) -> gltf::Material<'_> {
        self.document
            .materials()
            .nth(material)
            .expect("glTF material index in bounds")
    }
}
