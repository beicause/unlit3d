//! Load glTF documents and patch them into another world's
//! [`MeshSource`](crate::mesh_source::MeshSource): upload textures, materials
//! and meshes into an existing resource graph, unload them again, and prepare
//! the document's default scene as a reusable [`LitModel`](crate::lit::LitModel).
//!
//! [`LitGltf`] owns only the slim, replayable state of a loaded document —
//! the parsed [`gltf::Document`], its decoded buffers and images, and each
//! node's world-space transform. It holds no
//! [`World`](unlit_ecs::World), no [`MeshSource`](crate::mesh_source::MeshSource)
//! and no GPU resources of its own. Everything GPU lives in
//! the graph of the `MeshSource` the caller patches, so a document can be
//! inserted into any number of worlds, and each insertion keeps the handles
//! that unload it again.
//!
//! A primitive carrying `KHR_draco_mesh_compression` is decoded with
//! `draco-core` while the document is loaded: its stream is written back as
//! ordinary buffer views and accessors, so the document is validated normally
//! and every later reader — morph targets, skins and animations included —
//! sees only accessors. A document without a compressed primitive keeps its
//! original validation error. Decoding happens once at import rather than on
//! each mesh insertion.
//!
//! # Usage
//!
//! ```rust,no_run
//! use unlit3d::gltf::LitGltf;
//! # fn run() -> Result<(), unlit3d::gltf::Error> {
//! let gltf = LitGltf::load("model.glb")?;
//! # Ok(()) }
//! ```
//!
//! Upload and prepare the default scene with [`LitGltf::insert_asset`]:
//!
//! ```rust,no_run
//! # use unlit3d::gltf::LitGltf;
//! # use unlit3d::prelude::MeshSource;
//! use unlit3d::lit::LitModelBundle;
//! # use unlit_ecs::ScopeMut;
//! # fn run(source: &mut MeshSource, world: &mut ScopeMut<'_>) -> Result<(), unlit3d::gltf::Error> {
//! let gltf = LitGltf::load("model.glb")?;
//! let asset = gltf.insert_asset(source, &world.readonly())?;
//! // Spawn under the mesh source that will draw the model.
//! let root = world.spawn(LitModelBundle::new(asset.model));
//! # Ok(()) }
//! ```
//!
//! Use [`LitGltf::insert_asset_with`] to prepare only selected clips, in the
//! order the model should number them. The model retains its draw handles;
//! neither the parsed document nor the resource table must outlive it. Each
//! [`LitModelBundle`](crate::lit::LitModelBundle) instance has its own root
//! [`Transform`] and playback state.
//!
//! To reuse a texture or customize the upload, build a [`GltfResources`] with
//! [`LitGltf::insert_images`], [`LitGltf::insert_materials`] and
//! [`LitGltf::insert_meshes`], then pass it to [`LitGltf::to_model`].
//!
//! A document's collections are addressed by typed ids — [`ImageId`],
//! [`MaterialId`], [`MeshId`], [`PrimitiveId`], [`NodeId`] and
//! [`AnimationId`] — so a node index cannot be handed to an animation or a
//! mesh by mistake. The `images()`, `materials()`, `meshes()`,
//! `primitives()`, `nodes()` and `animations()` iterators yield the ids a
//! document carries.
//!
//! Spawning entities never spawns a camera: a pass draws through the camera it
//! states, or through the first active [`Camera`](crate::components::Camera) a
//! source reaches, so one belongs to the caller.
//!
//! # Supported subset
//!
//! Materials use base-color factors and textures. `KHR_materials_unlit` opts
//! out of lighting; other PBR texture channels are not imported. `BLEND` uses
//! straight alpha and sorts back to front without writing depth; `MASK` uses
//! the declared cutoff (default 0.5). `doubleSided` disables culling. The
//! renderer corrects winding for a mirrored instance at draw time; skinned
//! geometry and its winding use the world-space joint pose, not the mesh
//! entity's transform.
//!
//! Primitives carry positions, optional indices, normals and `COLOR_0`, the
//! base-color texture's `TEXCOORD_n` when present, and paired `JOINTS_0` and
//! `WEIGHTS_0`. Missing UVs leave a primitive untextured. Tangents are ignored.
//! `LINE_LOOP` and `TRIANGLE_FAN` are expanded at upload; Draco triangle
//! strips are normalized to triangle lists at import. Other modes retain
//! their topology.
//!
//! [`LitGltf::skin_pose`] and model import agree on missing inverse bind
//! matrices (identity) and extra matrices (ignored). Morph targets move
//! positions only; normal-only targets retain their weight slots. Initial
//! weights come from the node, then the mesh, with missing entries zeroed.
//! Clips are decoded on demand and cached by the document. Asset preparation
//! decodes the selected clips before spawning; single keyframes give constant
//! channels, and clip duration includes channels targeting omitted nodes.
//! Instances sample at spawn; for subsequent playback see
//! [`LitModelAnimation`](crate::lit::LitModelAnimation).
//!
//! # Texture formats
//!
//! Base-color images are widened to RGBA, replicating luma and supplying opaque
//! alpha where absent. Eight-bit images use `Rgba8UnormSrgb`; 16-bit and 32-bit
//! images use linear `Rgba16Float` and `Rgba32Float`, respectively. Normalized
//! samples are clamped; float uploads preserve HDR within the target format's
//! range and replace NaN with zero. Each upload generates the levels requested
//! by [`MipChain`] from the decoded image, averaging sRGB in linear light.
//! Without `FLOAT32_FILTERABLE`, an `Rgba32Float` image uses a non-filtering
//! sampler; its [`GltfImage::filtering`] value governs both the material binding
//! and the primitive's pipeline.
//!
//! # Errors and resource lifetime
//!
//! [`Error`] distinguishes parsing, malformed streams or images, missing
//! dependencies, and invalid document IDs. Invalid indices, primitives too
//! short for their topology, inconsistent morph counts and unsupported Draco
//! stream references report the affected document IDs. An upload that samples
//! a base-color image requires its handle in the same [`GltfResources`] table.
//! Dropping the last handle permits the next
//! [`MeshSource::maintain`](crate::mesh_source::MeshSource::maintain) to reclaim
//! the resource once no other resource depends on it.

use hashbrown::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use core::mem::size_of;

use crate::components::{GpuMaterial, GpuMesh, Transform};
use crate::lit::{LitModel, LitPipelineKey};
use unlit_wgpu::resources::{ResHandle, TextureView};

mod animation;
mod draco;
#[cfg(test)]
mod draco_tests;
mod error;
mod ids;
mod image;
pub use self::error::{DracoPrimitiveError, Error};
pub use self::ids::{AnimationId, ImageId, MaterialId, MeshId, NodeId, PrimitiveId};
pub use self::image::MipChain;
mod material;
mod model;
mod playback;
mod primitive;
mod scene;
mod skin;
mod streams;
#[cfg(test)]
mod tests;
mod topology;
mod upload;

use self::scene::{
    mesh_morph_targets, node_children, node_locals, node_order, node_parents, node_world_matrices,
    nodes_morph_targets,
};

/// The cutoff the glTF spec gives a `MASK` material that leaves `alphaCutoff`
/// out.
const DEFAULT_ALPHA_CUTOFF: f32 = 0.5;

/// The widest texel an upload format uses: four 32-bit float channels.
const MAX_TEXEL_BYTES: usize = 4 * size_of::<f32>();

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
    /// World-space matrix of every node, indexed by node index.
    ///
    /// These are the document's own matrices, multiplied down the parent chain
    /// without decomposing anything, so a node whose transform a document
    /// states as a matrix keeps its shear here — including the shear of every
    /// ancestor it inherits. Decomposition happens only where a `Transform` is
    /// needed: [`Self::skin_pose`] reads these matrices directly, while
    /// [`Self::to_model`] states each node's local transform as a
    /// [`Transform`], so a sheared chain reaches skinned geometry intact and a
    /// spawned node loses its own shear (see [`Transform::from_matrix`]).
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
    /// How many morph targets each node's mesh displaces, indexed by node index.
    ///
    /// A weights channel is as long as its targets, so a frame needs the count
    /// to slice its values; keeping the count is what lets it read a node's
    /// targets without building the node's weight vector.
    morph_targets: Vec<usize>,
    /// How many morph targets each mesh declares, indexed by mesh index.
    ///
    /// This is the count both uploads and channels are built from: the
    /// primitive's displacement slots on one side, the weights a channel
    /// states on the other. [`mesh_morph_targets`] refuses a mesh whose
    /// primitives disagree, so one number describes the whole mesh.
    mesh_targets: Vec<usize>,
    /// Whether each node states its transform as a matrix, indexed by node
    /// index.
    ///
    /// The specification gives a node written as a matrix no animatable
    /// properties, so a channel targeting one is dropped as the clip is
    /// decoded rather than sampled and overwritten every frame.
    matrix_nodes: Vec<bool>,
    /// Each animation's channels, decoded on first use, indexed by animation
    /// index.
    ///
    /// Decoding a channel's accessors yields an iterator, and sampling needs
    /// random access to the keyframes, so they are collected once here rather
    /// than on every frame that plays the clip.
    clips: Vec<OnceLock<Vec<crate::lit::LitModelChannel>>>,
    /// How much of a mip chain the document's textures carry.
    mips: MipChain,
}

impl LitGltf {
    /// Set how much of a mip chain the document's textures carry.
    ///
    /// The default is [`MipChain::Full`]: every texture builds the chain down
    /// to one texel, so a minified texture does not alias. Pass
    /// [`MipChain::Off`] to upload base levels only, or [`MipChain::Levels`]
    /// to cap the chain — for a caller that knows how far its textures are
    /// minified, or wants the upload itself cheap.
    pub fn mip_chain(mut self, mips: MipChain) -> Self {
        self.mips = mips;
        self
    }

    /// [`Self::mip_chain`] on a document that is already loaded.
    ///
    /// The choice is read when an image is uploaded, so setting it before
    /// [`Self::insert_resources`] is what makes every texture of the document
    /// carry the chain asked for.
    pub fn set_mip_chain(&mut self, mips: MipChain) {
        self.mips = mips;
    }
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
    /// An 8- or 16-bit upload always filters; the one upload that may not is
    /// 32-bit float — `Rgba32Float` is `unfilterable-float` unless the
    /// device has `Features::FLOAT32_FILTERABLE`, and then it can only be
    /// bound to a non-filtering sampler. A material built from this image has
    /// to declare the matching bind-group layout, which is what this flag
    /// decides.
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

/// A prepared glTF model and the resources its draws name.
///
/// `LitModel` owns the handles each draw needs, so a caller may move the model
/// out and drop this value once the model is installed. The resource table is
/// useful while inspecting what was uploaded or when a caller also needs the
/// document-indexed image, material and primitive handles.
#[derive(Clone, Debug)]
pub struct GltfAsset {
    /// The default scene and selected animation clips, ready to spawn.
    pub model: LitModel,
    /// The uploaded resources, indexed by their document ids.
    pub resources: GltfResources,
}

/// GPU resources for one document and one mesh source, keyed by document id.
///
/// Unuploaded ids remain absent regardless of insertion order. An uploaded
/// material without a base-color texture has an entry but no bind group.
/// Dropping this table releases its handles; a prepared model retains the
/// handles its draws need.
///
/// Use the same source for uploads into this table and for drawing its models.
/// A caller that reuses an uploaded resource for a different document must
/// key that resource by the id in the destination document.
#[derive(Clone, Debug, Default)]
pub struct GltfResources {
    images: HashMap<ImageId, GltfImage>,
    materials: HashMap<MaterialId, Option<GltfMaterial>>,
    meshes: HashMap<(MeshId, PrimitiveId), GltfMesh>,
    samplers: Vec<(wgpu::SamplerDescriptor<'static>, ResHandle<wgpu::Sampler>)>,
}

impl GltfResources {
    /// Empty resources, for a caller that uploads the pieces itself.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Place an uploaded image under the id it carries, returning the previous handle.
    pub fn insert_image(&mut self, image: GltfImage) -> Option<GltfImage> {
        self.images.insert(image.image, image)
    }

    /// Place an uploaded material under `material`, replacing any previous entry.
    ///
    /// `entry` is `None` for a material with no base-color texture. A textured
    /// handle is rekeyed to `material` when reused from another document.
    pub fn insert_material(&mut self, material: MaterialId, mut entry: Option<GltfMaterial>) {
        if let Some(entry) = &mut entry {
            entry.material = material;
        }
        self.materials.insert(material, entry);
    }

    /// Place an uploaded primitive under the mesh and primitive it draws.
    ///
    /// Returns the handle it displaced, if that primitive was already there.
    pub fn insert_mesh(&mut self, mesh: GltfMesh) -> Option<GltfMesh> {
        self.meshes
            .insert((mesh.mesh_index, mesh.primitive_index), mesh)
    }

    /// The uploaded image at `image`, or `None` when it was never inserted.
    #[must_use]
    pub fn image(&self, image: ImageId) -> Option<&GltfImage> {
        self.images.get(&image)
    }

    /// Every uploaded image, in no particular order.
    pub fn images(&self) -> impl Iterator<Item = &GltfImage> {
        self.images.values()
    }

    /// The uploaded material at `material`, or `None` when it was never
    /// inserted or names no base-color texture.
    ///
    /// [`Self::holds_material`] separates those two answers.
    #[must_use]
    pub fn material(&self, material: MaterialId) -> Option<&GltfMaterial> {
        self.materials.get(&material)?.as_ref()
    }

    /// Whether the resources carry an entry for `material` at all.
    ///
    /// An entry that was uploaded without a base-color texture answers `true`
    /// and [`Self::material`] answers `None`, which is the difference between
    /// a material a document states untextured and one that was never
    /// inserted.
    #[must_use]
    pub fn holds_material(&self, material: MaterialId) -> bool {
        self.materials.contains_key(&material)
    }

    /// The uploaded primitive of `mesh`, or `None` when it was never inserted.
    #[must_use]
    pub fn mesh(&self, mesh: MeshId, primitive: PrimitiveId) -> Option<&GltfMesh> {
        self.meshes.get(&(mesh, primitive))
    }

    /// Every uploaded primitive, in no particular order.
    pub fn meshes(&self) -> impl Iterator<Item = &GltfMesh> {
        self.meshes.values()
    }

    /// How many primitives were uploaded.
    #[must_use]
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// The samplers uploaded for this document's materials, keyed by their
    /// descriptors.
    ///
    /// A sampler is an immutable GPU object and documents repeat one across
    /// their materials, so an upload that has already built a descriptor's
    /// sampler reuses it rather than creating a second one the graph would keep
    /// alive beside it. The cache lives as long as the resources, so a caller
    /// inserting materials one at a time gets the same reuse a whole-document
    /// upload does.
    #[must_use]
    pub fn samplers(&self) -> &[(wgpu::SamplerDescriptor<'static>, ResHandle<wgpu::Sampler>)] {
        &self.samplers
    }

    /// Look up or upload the sampler a descriptor asks for.
    pub fn sampler(
        &mut self,
        source: &mut crate::mesh_source::MeshSource,
        world: &unlit_ecs::Scope<'_>,
        descriptor: wgpu::SamplerDescriptor<'static>,
    ) -> ResHandle<wgpu::Sampler> {
        let known = self.samplers.iter().find(|(built, _)| *built == descriptor);
        match known {
            Some((_, sampler)) => sampler.clone(),
            None => {
                let sampler = source.register_sampler(world, Some(descriptor.clone()));
                self.samplers.push((descriptor, sampler.clone()));
                sampler
            }
        }
    }
}

impl LitGltf {
    /// Load a glTF document from `path`, decoding its buffers and images.
    ///
    /// External URIs resolve against the file's directory. Fails with
    /// [`Error`] when the file cannot be read or parsed, or a referenced
    /// buffer or image cannot be decoded.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let imported = match gltf::import(path) {
            Ok(imported) => imported,
            Err(error) => Self::import_draco(
                error,
                || {
                    gltf::Gltf::from_reader_without_validation(std::io::BufReader::new(
                        std::fs::File::open(path)?,
                    ))
                },
                path.parent(),
            )?,
        };
        Self::from_parts(imported.0, imported.1, imported.2)
    }

    /// Parse a glTF document from `bytes`.
    ///
    /// The bytes may be either glTF JSON or a binary GLB container. Buffers
    /// arrive as embedded data URIs or, for a GLB, the `BIN` chunk; images
    /// must be embedded too (as data URIs or buffer views), because there is
    /// no base URI to resolve external references against.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let (document, buffers, images) = match gltf::import_slice(bytes) {
            Ok(imported) => imported,
            Err(error) => Self::import_draco(
                error,
                || gltf::Gltf::from_slice_without_validation(bytes),
                None,
            )?,
        };
        Self::from_parts(document, buffers, images)
    }

    /// Try to repair a document that failed validation by decoding its Draco
    /// primitives back into ordinary accessors.
    ///
    /// glTF validation rejects the Draco extension and the accessors whose data
    /// lives only in the compressed stream. Re-parsing without validation,
    /// decoding each stream into real buffer views and validating again lets
    /// the rest of the loader read one uniform document. A document with no
    /// compressed primitive keeps the original error; a stream that cannot be
    /// decoded is reported as [`Error::Draco`], carrying the reader's own
    /// report rather than restating it as a glTF validation failure.
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
        Error,
    > {
        if !matches!(error, gltf::Error::Validation(_)) {
            return Err(error.into());
        }
        let gltf::Gltf { document, blob } = parse()?;
        let mut buffers = gltf::import_buffers(&document, base, blob)?;
        let mut root = document.into_json();
        if !draco::fill_accessors(&mut root, &mut buffers)? {
            return Err(error.into());
        }
        let document = gltf::Document::from_json(root)?;
        let images = gltf::import_images(&document, base, &buffers)?;
        Ok((document, buffers, images))
    }

    fn from_parts(
        document: gltf::Document,
        buffers: Vec<gltf::buffer::Data>,
        images: Vec<gltf::image::Data>,
    ) -> Result<Self, Error> {
        let mesh_targets = mesh_morph_targets(&document)?;
        let parents = node_parents(&document);
        let locals = node_locals(&document);
        let children = node_children(&document);
        let order = node_order(&children, &parents);
        let node_matrices = node_world_matrices(&document, &parents, &order);
        let morph_targets = nodes_morph_targets(&document, &mesh_targets);
        let matrix_nodes = document
            .nodes()
            .map(|node| matches!(node.transform(), gltf::scene::Transform::Matrix { .. }))
            .collect();
        let clips = std::iter::repeat_with(OnceLock::new)
            .take(document.animations().count())
            .collect();
        for (index, data) in images.iter().enumerate() {
            // A short pixel buffer would be read past its end while the image
            // is encoded, so the document is refused here, where the image's
            // own index is still known.
            let (texel, expected) = image::pixel_bytes(data);
            if data.pixels.len() != expected {
                return Err(Error::ImagePixelsShort {
                    image: ImageId::new(index),
                    width: data.width,
                    height: data.height,
                    texel,
                    expected,
                    actual: data.pixels.len(),
                });
            }
        }
        Ok(Self {
            document,
            buffers,
            images,
            node_matrices,
            parents,
            locals,
            morph_targets,
            mesh_targets,
            matrix_nodes,
            clips,
            mips: MipChain::default(),
        })
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
    ///
    /// `None` when the document carries no such mesh, so a caller that builds
    /// ids itself cannot read a mesh list it guessed at.
    pub fn primitives(&self, mesh: MeshId) -> Option<impl Iterator<Item = PrimitiveId>> {
        let count = self.mesh(mesh)?.primitives().count();
        Some((0..count).map(PrimitiveId::new))
    }

    /// The ids of every node in the document, in document order.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> {
        (0..self.document.nodes().len()).map(NodeId::new)
    }

    /// The ids of every animation in the document, in document order.
    pub fn animations(&self) -> impl Iterator<Item = AnimationId> {
        (0..self.document.animations().count()).map(AnimationId::new)
    }

    pub(super) fn node(&self, node: NodeId) -> Option<gltf::scene::Node<'_>> {
        self.document.nodes().nth(node.index())
    }

    pub(super) fn mesh(&self, mesh: MeshId) -> Option<gltf::Mesh<'_>> {
        self.document.meshes().nth(mesh.index())
    }

    pub(super) fn primitive(
        &self,
        mesh: MeshId,
        primitive: PrimitiveId,
    ) -> Option<gltf::Primitive<'_>> {
        self.mesh(mesh)?.primitives().nth(primitive.index())
    }

    pub(super) fn material(&self, material: MaterialId) -> Option<gltf::Material<'_>> {
        self.document.materials().nth(material.index())
    }
}
