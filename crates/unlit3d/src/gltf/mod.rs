//! Load glTF documents and patch them into another world's
//! [`MeshSource`]: upload textures, materials and meshes into an existing
//! resource graph, unload them again, and spawn entities that draw the
//! document's default scene.
//!
//! [`LitGltf`] owns only the slim, replayable state of a loaded document —
//! the parsed [`gltf::Document`], its decoded buffers and images, and each
//! node's world-space transform — plus nothing else. It holds no
//! [`World`](unlit_ecs::World), no [`MeshSource`] and no GPU resources of its
//! own. Everything GPU lives in
//! the graph of the `MeshSource` the caller patches, so a document can be
//! inserted into any number of worlds, and each insertion keeps the handles
//! that unload it again.
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
//! [`SkinPose`] its joints deform by, and spawning such a node creates the
//! pose entity its mesh binds to. A primitive whose mesh declares a morph
//! target that displaces positions uploads those displacements and spawns with
//! a [`MorphBinding`] to a [`MorphWeights`] entity holding the node's starting
//! weights — see [`LitGltf::morph_weights`] — so writing that entity is how
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
//! derivatives infer from its positions. Everything else a glTF document can
//! carry is ignored: no tangents. A mesh whose primitive uses techniques
//! outside this subset still uploads and spawns — it just renders without them.
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

use crate::components::{
    GpuMaterial, GpuMesh, GpuRenderPipeline, MorphBinding, MorphWeights, SkinBinding, SkinPose,
    Transform, ZSortedDrawing,
};
use crate::lit::{InstanceColor, InstanceCutoff, LitMeshDesc, LitPipelineKey};
use crate::mesh::MorphDeltas;
use crate::mesh_source::MeshSource;
use gltf::animation::Property;
use gltf::animation::util::ReadOutputs;
use unlit_ecs::bundle;
use unlit_ecs::{DynamicBundle, Scope, ScopeMut, prelude::Entity};
use unlit_wgpu::pipeline::{BaseColorChannels, LitOptions};
use unlit_wgpu::resources::{ResHandle, TextureExt, TextureView};

mod animation;
mod image;
mod material;
mod scene;

use self::animation::{interpolation_stride, sample_rotation, sample_vec3, sample_weights};
use self::image::{align_up, encode_rows, image_format};
use self::material::{
    alpha_cutoff, morph_target_count, sampler_descriptor, samples_base_color_texture, textured_key,
};
use self::scene::{decompose, node_locals, node_order, node_parents, node_world_matrices};

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
/// [`MeshSource`].
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
/// transform, its [`SkinPose`] and its [`MorphWeights`] through the entities
/// this names.
#[derive(Clone, Debug)]
pub struct GltfNode {
    /// The node in the glTF document these entities draw.
    pub node: NodeId,
    /// One entity per primitive of the node's mesh; empty for a node with no
    /// mesh.
    pub entities: Vec<Entity>,
    /// The entity holding the node's [`SkinPose`], when the node is skinned.
    pub pose: Option<Entity>,
    /// The entity holding the node's [`MorphWeights`], when the node's mesh
    /// has morph targets.
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
/// lets the next [`MeshSource::maintain`] collect them.
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
        let (document, buffers, images) = gltf::import(path)?;
        Ok(Self::from_parts(document, buffers, images))
    }

    /// Parse a glTF document from `bytes`.
    ///
    /// The bytes may be either glTF JSON or a binary GLB container. Buffers
    /// arrive as embedded data URIs or, for a GLB, the `BIN` chunk; images
    /// must be embedded too (as data URIs or buffer views), because there is
    /// no base URI to resolve external references against.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, gltf::Error> {
        let (document, buffers, images) = gltf::import_slice(bytes)?;
        Ok(Self::from_parts(document, buffers, images))
    }

    fn from_parts(
        document: gltf::Document,
        buffers: Vec<gltf::buffer::Data>,
        images: Vec<gltf::image::Data>,
    ) -> Self {
        let parents = node_parents(&document);
        let locals = node_locals(&document);
        let node_matrices = node_world_matrices(&document, &parents);
        let node_transforms = node_matrices.iter().map(|&m| decompose(m)).collect();
        Self {
            document,
            buffers,
            images,
            node_transforms,
            node_matrices,
            parents,
            locals,
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

    /// The [`LitPipelineKey`] the primitive renders with.
    ///
    /// The key carries only what a caller picks: the texture filtering the
    /// material's base-color texture needs, the luminance expansion that
    /// texture's format calls for, whether a `BLEND` material blends without
    /// writing depth and whether a `MASK` one cuts. Everything geometric —
    /// which vertex channels the primitive carries, whether it samples its
    /// base-color texture, whether it skins or morphs — is a property of the
    /// draw, so [`Self::insert_mesh`] uploads the streams the primitive's own
    /// attributes declare and the variant a draw resolves derives the rest
    /// from the mesh it draws. A `doubleSided` material turns back-face
    /// culling off, so its geometry is drawn from either side; everything
    /// else follows [`LitOptions::standard`].
    pub fn pipeline_key(
        &self,
        device: &wgpu::Device,
        mesh: MeshId,
        primitive: PrimitiveId,
    ) -> LitPipelineKey {
        let primitive = self.primitive(mesh.index(), primitive.index());
        let material = primitive.material();
        // A glTF scene states no lights of its own, so its draws shade
        // nothing: the base colour it imports is what the file says.
        let mut options = LitOptions {
            unlit: true,
            ..LitOptions::standard()
        };
        // The texture the material samples decides whether the base-color
        // binding is a filtering one: the pipeline and the material bind group
        // have to agree, or the group does not fit the pipeline.
        let texture = material.pbr_metallic_roughness().base_color_texture();
        if samples_base_color_texture(&primitive)
            && let Some(info) = &texture
        {
            let image = info.texture().source().index();
            options.texture_filtering = self.image_filtering(image, device);
            // A one- or two-channel texture uploads in a format that cannot
            // decode sRGB for itself, so the shader expands the texel and
            // decodes the luminance instead.
            options.base_color = self.image_luminance(image);
        }
        // glTF base colors and textures are straight (non-premultiplied)
        // alpha, which is exactly what `ALPHA_BLENDING` composites. A blended
        // fragment covers a surface only partly, so its depth is no
        // measurement of where that surface is: writing it would let a
        // fragment discard the ones the same surface draws behind it, which is
        // the one overlap the back-to-front sort of `ZSortedDrawing` cannot
        // reorder away.
        if material.alpha_mode() == gltf::material::AlphaMode::Blend {
            options.blend = Some(wgpu::BlendState::ALPHA_BLENDING);
            options.depth_stencil.depth_write_enabled = false;
        }
        // A `MASK` material is not a translucent one: its fragments are either
        // drawn whole or discarded, so it neither blends nor needs the
        // back-to-front sort a blended one does. The cutoff itself is
        // per-instance state, so it rides the instance stream rather than the
        // material group — see [`Self::spawn_node`].
        if alpha_cutoff(&material).is_some() {
            options.alpha_cutoff = true;
        }
        // A `doubleSided` material is visible from behind, so its back faces
        // are rasterized rather than culled. Nothing else about the variant
        // changes: the unlit fragment shader reads no normal, so there is no
        // back-face normal to reverse, and the geometry simply draws from both
        // sides.
        if material.double_sided() {
            options.primitive.cull_mode = None;
        }
        LitPipelineKey::new(options)
    }

    // -- images ---------------------------------------------------------------

    /// Upload `image` into `source`'s resource graph and return its handle.
    ///
    /// The texture holds the image's decoded pixels in the GPU format closest
    /// to them — see the [module docs](self#texture-formats); its sampler is
    /// not uploaded here, because that belongs to the material that samples
    /// it.
    pub fn insert_image(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        image: ImageId,
    ) -> GltfImage {
        let data = &self.images[image.index()];
        let device = source.device(world);
        let format = image_format(data, &device);
        let (width, height) = (data.width, data.height);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // `encode_rows` pads every row to the 256-byte stride a buffer-to-texture
        // copy needs; the padding bytes stay zero.
        let bytes_per_row = align_up(
            width * format.texel_bytes(),
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
        );
        let rows = encode_rows(data, format.format);
        source.queue(world).write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rows,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let (texture, view) = source.register_texture_and_default_view(world, texture);
        GltfImage {
            image,
            texture,
            view,
            filtering: format.filtering,
        }
    }

    /// Upload every image of the document.
    ///
    /// The returned vector is aligned with the document's image indices: the
    /// handle for image `i` sits at index `i`.
    pub fn insert_images(&self, source: &mut MeshSource, world: &Scope<'_>) -> Vec<GltfImage> {
        self.images()
            .map(|image| self.insert_image(source, world, image))
            .collect()
    }

    // -- materials ------------------------------------------------------------

    /// Build the material bind group for `material` and return its handle.
    ///
    /// Returns `None` when the material declares no base-color texture: such a
    /// material has nothing to bind and spawns with no `GpuMaterial`.
    ///
    /// # Panics
    ///
    /// If the material's base-color texture names an image that is not in
    /// `images`.
    pub fn insert_material(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        material: MaterialId,
        images: &[GltfImage],
    ) -> Option<GltfMaterial> {
        let data = self.material(material.index());
        let info = data.pbr_metallic_roughness().base_color_texture()?;
        let image = images
            .iter()
            .find(|image| image.image == ImageId::new(info.texture().source().index()))
            .expect("the base-color texture's image must be inserted before the material");

        // A material group exists only for a variant that samples a base-color
        // texture; build the bind group against exactly that variant. Its
        // layout is the filtering one only when the image can be filtered, and
        // the sampler has to be non-filtering in step with it.
        let key = textured_key(image.filtering);
        let descriptor = sampler_descriptor(&info.texture().sampler());
        let descriptor = if image.filtering {
            descriptor
        } else {
            wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..descriptor
            }
        };
        let sampler = source.register_sampler(world, Some(descriptor));
        let view = {
            let texture = {
                let mut graph_row = source.context().graph(world);
                let graph = graph_row.get();
                graph
                    .get(&image.texture)
                    .expect("the image's texture is in the graph")
                    .clone()
            };
            let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
            let mut graph_row = source.context().graph(world);
            let graph = graph_row.get();
            let view = graph.insert(view, None);
            graph.add_dependency(&view, &image.texture);
            view
        };

        let bind_group =
            source.allocate_lit_material(world, &key.options, view.clone(), sampler.clone());
        Some(GltfMaterial {
            material,
            bind_group,
            view,
            sampler,
        })
    }

    /// Build the bind group for every material of the document.
    ///
    /// The returned vector is aligned with the document's material indices:
    /// the entry for material `i` sits at index `i`, and is `None` for a
    /// material with no base-color texture.
    ///
    /// # Panics
    ///
    /// If a material's base-color texture names an image that is not in
    /// `images`.
    pub fn insert_materials(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        images: &[GltfImage],
    ) -> Vec<Option<GltfMaterial>> {
        self.materials()
            .map(|material| self.insert_material(source, world, material, images))
            .collect()
    }

    // -- meshes ---------------------------------------------------------------

    /// Upload the primitive into `source`'s resource graph and return its
    /// handle.
    ///
    /// Which streams are uploaded follows the primitive's own attributes:
    /// `TEXCOORD_0` with a base-color-textured material enables the UV stream,
    /// `COLOR_0` the vertex-color stream, `JOINTS_0` and `WEIGHTS_0` the joint
    /// stream, and a position-displacing morph target the displacement stream.
    /// A primitive read with streams its attributes do not declare would
    /// panic, so the derivation is the only safe choice here. Normals and
    /// tangents are ignored (see the module docs).
    ///
    /// # Panics
    ///
    /// If the primitive carries no `POSITION` attribute.
    pub fn insert_mesh(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        mesh: MeshId,
        primitive: PrimitiveId,
    ) -> GltfMesh {
        let key = self.pipeline_key(&source.device(world), mesh, primitive);
        let primitive_id = primitive;
        let primitive = self.primitive(mesh.index(), primitive_id.index());
        let reader = primitive.reader(|buffer| Some(&self.buffers[buffer.index()]));

        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .expect("a glTF primitive carries its POSITION attribute")
            .collect();
        // Read only the streams the primitive declares: an attribute it does
        // not carry is not uploaded, so decoding it would be wasted work.
        let uvs: Option<Vec<[f32; 2]>> = samples_base_color_texture(&primitive).then(|| {
            reader
                .read_tex_coords(0)
                .expect("the primitive samples a texture, so it has TEXCOORD_0")
                .into_f32()
                .collect()
        });
        let colors: Option<Vec<[u8; 4]>> = primitive
            .get(&gltf::Semantic::Colors(0))
            .is_some()
            .then(|| {
                reader
                    .read_colors(0)
                    .expect("the primitive has COLOR_0")
                    .into_rgba_u8()
                    .collect()
            });
        // A primitive that carries normals describes its own surface, which is
        // smoother than the one screen-space derivatives infer from the
        // positions; one without them falls back to those derivatives.
        let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|normals| normals.collect());
        let indices: Option<Vec<u32>> = reader.read_indices().map(|i| i.into_u32().collect());
        let skinned = primitive.get(&gltf::Semantic::Joints(0)).is_some()
            && primitive.get(&gltf::Semantic::Weights(0)).is_some();
        let joints: Option<Vec<[u16; 4]>> = skinned.then(|| {
            reader
                .read_joints(0)
                .expect("the primitive has JOINTS_0")
                .into_u16()
                .collect()
        });
        let weights: Option<Vec<[f32; 4]>> = skinned.then(|| {
            reader
                .read_weights(0)
                .expect("the primitive has WEIGHTS_0")
                .into_f32()
                .collect()
        });
        // The displacements the mesh's morph stream reads, packed the way a
        // mesh wants them: for each vertex, every position-displacing target in
        // order, three components each. Writing straight into the packed array
        // keeps one allocation for the whole thing, however many targets the
        // primitive declares; a target of normals alone is skipped here.
        let morph_deltas = (morph_target_count(&primitive) > 0).then(|| {
            let target_count = morph_target_count(&primitive);
            let mut deltas = vec![0.0; positions.len() * target_count * 3];
            let mut target = 0;
            for (displacements, _, _) in reader.read_morph_targets() {
                let Some(displacements) = displacements else {
                    continue;
                };
                for (vertex, displacement) in displacements.enumerate() {
                    let start = (vertex * target_count + target) * 3;
                    deltas[start..start + 3].copy_from_slice(&displacement);
                }
                target += 1;
            }
            MorphDeltas {
                deltas,
                target_count: target_count as u32,
            }
        });

        let gpu_mesh = source.allocate_lit_mesh(
            world,
            &key.options,
            LitMeshDesc {
                positions: &positions,
                uvs: uvs.as_deref(),
                colors: colors.as_deref(),
                normals: normals.as_deref(),
                indices: indices.as_deref(),
                joints: joints.as_deref(),
                weights: weights.as_deref(),
                morph_deltas,
            },
        );

        GltfMesh {
            mesh_index: mesh,
            primitive_index: primitive_id,
            key,
            mesh: gpu_mesh,
        }
    }

    /// Upload every primitive of every mesh of the document.
    ///
    /// The vector is ordered document-mesh-major, then primitive-minor: first
    /// all of mesh 0's primitives, then mesh 1's, and so on.
    pub fn insert_meshes(&self, source: &mut MeshSource, world: &Scope<'_>) -> Vec<GltfMesh> {
        let count: usize = self
            .document
            .meshes()
            .map(|mesh| mesh.primitives().len())
            .sum();
        let mut meshes = Vec::with_capacity(count);
        for mesh in self.meshes() {
            for primitive in self.primitives(mesh) {
                meshes.push(self.insert_mesh(source, world, mesh, primitive));
            }
        }
        meshes
    }

    /// Insert the whole document and return every handle that keeps it alive.
    ///
    /// This is the one call that uploads a document: it inserts the images,
    /// builds the materials from them and uploads every primitive, and hands
    /// back a [`GltfResources`] holding all of it. Pass that to
    /// [`Self::spawn_node`] or [`Self::spawn_default_scene`] to draw the
    /// document; keep it alive for as long as the entities live.
    ///
    /// A caller that inserts the pieces itself — to share a texture between
    /// documents, say — can call [`Self::insert_images`],
    /// [`Self::insert_materials`] and [`Self::insert_meshes`] instead and
    /// build a [`GltfResources`] of its own.
    pub fn insert_resources(&self, source: &mut MeshSource, world: &Scope<'_>) -> GltfResources {
        let images = self.insert_images(source, world);
        let materials = self.insert_materials(source, world, &images);
        let meshes = self.insert_meshes(source, world);
        GltfResources {
            images,
            materials,
            meshes,
        }
    }

    // -- skins ----------------------------------------------------------------

    /// The [`SkinPose`] of a node's skin, at the node's rest transform.
    ///
    /// A document carries a skin as a list of joint nodes and one inverse bind
    /// matrix per joint; the pose a renderer wants is a matrix per joint, which
    /// is what this builds:
    ///
    /// ```text
    /// joint matrix = inverse(mesh node world) * joint node world * inverse bind matrix
    /// ```
    ///
    /// The pose is the *rest* pose — the document's node transforms as loaded,
    /// which is the transform each joint was bound in, so the deformation is
    /// the identity everywhere. A caller that animates joints moves the joint
    /// nodes and rebuilds the pose, or writes the matrices itself; what this
    /// gives is a valid pose to start from and the joint count the mesh's
    /// joint indices address.
    ///
    /// `node` is the node the mesh is attached to, whose skin the pose comes
    /// from. A node with no skin, or a skin with no `inverseBindMatrices`, gets
    /// matrices that deform nothing: the identity per joint, which is what a
    /// skin bound at rest means.
    ///
    /// The returned pose has one matrix per joint of the skin, in the order the
    /// mesh's joint indices address them.
    pub fn skin_pose(&self, node: NodeId) -> SkinPose {
        self.skin_pose_with_matrices(node.index(), &self.node_matrices)
    }

    /// The [`SkinPose`] a node's skin deforms by, evaluated against a given
    /// set of node world matrices.
    ///
    /// [`Self::skin_pose`] evaluates the rest pose; [`Self::apply_animation`]
    /// evaluates one where the joints have been animated.
    fn skin_pose_with_matrices(&self, node: usize, matrices: &[glam::Mat4]) -> SkinPose {
        let node_data = self.node(node);
        let Some(skin) = node_data.skin() else {
            return SkinPose::default();
        };
        let reader = skin.reader(|buffer| Some(&self.buffers[buffer.index()]));
        let inverse_bind: Option<Vec<glam::Mat4>> = reader.read_inverse_bind_matrices().map(|m| {
            m.map(|matrix| glam::Mat4::from_cols_array_2d(&matrix))
                .collect()
        });
        // A skinned mesh is deformed in its own space, so the mesh node's world
        // matrix has to come back out of the joint's: a joint's inverse bind
        // matrix already encodes the joint's rest transform, and the mesh node
        // moves the whole skinned result.
        let world_to_mesh = matrices[node].inverse();
        let poses: Vec<glam::Mat4> = skin
            .joints()
            .enumerate()
            .map(|(joint, joint_node)| {
                let joint_world = matrices[joint_node.index()];
                let bind = inverse_bind.as_ref().map_or(glam::Mat4::IDENTITY, |ibm| {
                    *ibm.get(joint).expect("one inverse bind matrix per joint")
                });
                world_to_mesh * joint_world * bind
            })
            .collect();
        SkinPose { matrices: poses }
    }

    /// The number of joints a node's skin has.
    ///
    /// `None` when the node names no skin, so a caller can walk a document
    /// without asking first.
    pub fn skin_joint_count(&self, node: NodeId) -> Option<usize> {
        Some(self.node(node.index()).skin()?.joints().count())
    }

    /// The morph weights a node's mesh starts at, one per target that
    /// displaces positions.
    ///
    /// glTF lets a node override the mesh's own weights, so a node states them
    /// first and the mesh's `weights` is the fallback; a mesh that states
    /// neither starts at zero, which is to say undeformed. The vector is as
    /// long as the deforming target list, so it is always the right length for
    /// the [`MorphWeights`] a spawned mesh binds to — a document that states
    /// fewer weights than it has targets is padded, and one that states more is
    /// truncated, because the renderer rejects a mismatch.
    ///
    /// Returns an empty vector for a node whose mesh has no positional morph
    /// targets.
    pub fn morph_weights(&self, node: NodeId) -> Vec<f32> {
        let Some(mesh) = self.node(node.index()).mesh() else {
            return Vec::new();
        };
        let node_weights = self.node(node.index()).weights();
        let mut weights = node_weights
            .or_else(|| mesh.weights())
            .map(<[f32]>::to_vec)
            .unwrap_or_default();
        let count = mesh
            .primitives()
            .map(|primitive| morph_target_count(&primitive))
            .max()
            .unwrap_or(0);
        weights.resize(count, 0.0);
        weights
    }

    // -- animation ------------------------------------------------------------

    /// The number of animations the document carries.
    pub fn animation_count(&self) -> usize {
        self.document.animations().count()
    }

    /// The name of an animation, or `None` when it carries none.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub fn animation_name(&self, animation: AnimationId) -> Option<&str> {
        self.animation(animation.index()).name()
    }

    /// The duration of an animation in seconds: the latest input keyframe
    /// across all of its samplers.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub fn animation_duration(&self, animation: AnimationId) -> f32 {
        let animation = self.animation(animation.index());
        let mut duration: f32 = 0.0;
        for channel in animation.channels() {
            let reader = channel.reader(|buffer| Some(&self.buffers[buffer.index()]));
            if let Some(inputs) = reader.read_inputs()
                && let Some(last) = inputs.last()
            {
                duration = duration.max(last);
            }
        }
        duration
    }

    /// Drive an animation to `time` seconds, writing what it moves back into
    /// the world.
    ///
    /// `nodes` is the list [`Self::spawn_node`] /
    /// [`Self::spawn_default_scene`] returned: for every node it names, the
    /// animated world transform is written into the entity's [`Transform`],
    /// a skinned node's [`SkinPose`] is rebuilt against the animated joints,
    /// and a morphed node's [`MorphWeights`] are set to the sampled weights.
    ///
    /// `world` is borrowed shared, not mutable: components are written through
    /// the world's interior mutability, so the same call site can keep reading
    /// the world while it animates.
    ///
    /// A channel's inputs outside its keyframe range clamp to the nearest end,
    /// and translation, scale and rotation interpolate by the sampler's
    /// `interpolation`: `STEP` holds the previous keyframe, `LINEAR` blends
    /// (with a spherical blend for a rotation) and `CUBICSPLINE` evaluates the
    /// Hermite spline through the frame's tangents. A node no channel targets
    /// keeps its rest transform, and a node that is neither spawned nor an
    /// ancestor of one costs nothing.
    ///
    /// # Panics
    ///
    /// If `animation` is out of bounds.
    pub fn apply_animation(
        &self,
        world: &Scope<'_>,
        animation: AnimationId,
        time: f32,
        nodes: &[GltfNode],
    ) {
        let animation = self.animation(animation.index());
        let count = self.parents.len();
        let mut locals = self.locals.clone();
        let mut animated = vec![false; count];
        let mut weights: Vec<Option<Vec<f32>>> = vec![None; count];

        for channel in animation.channels() {
            let node = channel.target().node().index();
            let property = channel.target().property();
            let interpolation = channel.sampler().interpolation();
            let reader = channel.reader(|buffer| Some(&self.buffers[buffer.index()]));
            let Some(inputs) = reader.read_inputs() else {
                continue;
            };
            let times: Vec<f32> = inputs.collect();
            // Interpolation needs an interval to work in, and the spec
            // requires at least two keyframes anyway.
            if times.len() < 2 {
                continue;
            }
            let Some(outputs) = reader.read_outputs() else {
                continue;
            };
            let required = times.len() * interpolation_stride(interpolation);
            match (property, outputs) {
                (Property::Translation, ReadOutputs::Translations(values)) => {
                    let values: Vec<[f32; 3]> = values.collect();
                    if values.len() < required {
                        continue;
                    }
                    locals[node].translation =
                        sample_vec3(interpolation, &times, &values, time).into();
                    animated[node] = true;
                }
                (Property::Scale, ReadOutputs::Scales(values)) => {
                    let values: Vec<[f32; 3]> = values.collect();
                    if values.len() < required {
                        continue;
                    }
                    locals[node].scale = sample_vec3(interpolation, &times, &values, time).into();
                    animated[node] = true;
                }
                (Property::Rotation, ReadOutputs::Rotations(rotations)) => {
                    let values: Vec<[f32; 4]> = rotations.into_f32().collect();
                    if values.len() < required {
                        continue;
                    }
                    locals[node].rotation = sample_rotation(interpolation, &times, &values, time);
                    animated[node] = true;
                }
                (Property::MorphTargetWeights, ReadOutputs::MorphTargetWeights(outputs)) => {
                    let values: Vec<f32> = outputs.into_f32().collect();
                    let targets = self.morph_weights(NodeId::new(node)).len();
                    let needed = times.len() * targets * interpolation_stride(interpolation);
                    if targets == 0 || values.len() < needed {
                        continue;
                    }
                    weights[node] = Some(sample_weights(
                        interpolation,
                        &times,
                        &values,
                        targets,
                        time,
                    ));
                }
                _ => {}
            }
        }

        if !animated.iter().any(|&moved| moved) && weights.iter().all(Option::is_none) {
            return;
        }

        // A node whose ancestor moved has moved too, even when no channel
        // names it: mark the descendants before recomposing.
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); count];
        for (node, parent) in self.parents.iter().enumerate() {
            if let Some(parent) = parent {
                children[*parent].push(node);
            }
        }
        let mut dirty = animated.clone();
        let mut stack: Vec<usize> = (0..count).filter(|&node| dirty[node]).collect();
        while let Some(node) = stack.pop() {
            for &child in &children[node] {
                if !dirty[child] {
                    dirty[child] = true;
                    stack.push(child);
                }
            }
        }

        let mut matrices = self.node_matrices.clone();
        for &node in &node_order(&self.document, &self.parents) {
            if !dirty[node] {
                continue;
            }
            let local = glam::Mat4::from(locals[node].compute_matrix());
            matrices[node] = match self.parents[node] {
                Some(parent) => matrices[parent] * local,
                None => local,
            };
        }

        for handle in nodes {
            let node = handle.node.index();
            if dirty[node] {
                let transform = decompose(matrices[node]);
                for &entity in &handle.entities {
                    let _ = world
                        .query_one::<&mut Transform>(entity)
                        .map(|mut current| {
                            let current = current.get();
                            {
                                *current = transform.clone();
                            }
                        });
                }
            }
            if let Some(entity) = handle.pose {
                let pose = self.skin_pose_with_matrices(node, &matrices);
                let _ = world.query_one::<&mut SkinPose>(entity).map(|mut current| {
                    let current = current.get();
                    *current = pose.clone()
                });
            }
            if let (Some(entity), Some(sampled)) = (handle.weights, weights[node].as_ref()) {
                let _ = world
                    .query_one::<&mut MorphWeights>(entity)
                    .map(|mut current| {
                        let current = current.get();
                        {
                            current.weights = sampled.clone();
                        }
                    });
            }
        }
    }

    fn animation(&self, animation: usize) -> gltf::animation::Animation<'_> {
        self.document
            .animations()
            .nth(animation)
            .expect("glTF animation index in bounds")
    }

    // -- spawning -------------------------------------------------------------

    /// Spawn the entities that draw `node`'s mesh into `world`.
    ///
    /// Returns a [`GltfNode`] naming one entity per primitive of the node's
    /// mesh — each carrying the
    /// node's world-space [`Transform`], the uploaded [`GpuMesh`], the
    /// pipeline for the mesh's key and an [`InstanceColor`] tinted with the
    /// material's base-color factor. A primitive that samples its material's
    /// base-color texture also carries the matching [`GpuMaterial`]. A blended
    /// primitive ([`gltf::material::AlphaMode::Blend`]) also carries
    /// [`ZSortedDrawing`], so it is drawn after the opaque geometry and
    /// composited back-to-front, and its pipeline writes no depth: each of its
    /// fragments covers a surface only partly, and the ones behind it are
    /// still to be drawn. A skinned primitive (one whose mesh carries a
    /// joint stream) carries a [`SkinBinding`] to the node's [`SkinPose`], which is
    /// spawned on an entity of its own so several meshes can share it — see
    /// [`Self::skin_pose`]. Children are not spawned; use
    /// [`Self::spawn_default_scene`] for the whole scene.
    ///
    /// # Panics
    ///
    /// If the node's mesh (or a mesh it shares primitives with) was not
    /// inserted into `resources`, or a primitive's material is `None` there
    /// while it samples a base-color texture.
    pub fn spawn_node(
        &self,
        world: &mut ScopeMut<'_>,
        node: NodeId,
        resources: &GltfResources,
    ) -> GltfNode {
        let index = node.index();
        let transform = self.node_transforms[index].clone();
        let node = self.node(index);
        let mut entities = Vec::new();
        // One pose entity per skinned node, shared by every primitive it
        // draws: the joints are the node's skin, and two primitives of one
        // mesh deform together. The morph weights get an entity of their own
        // for the same reason — they are the node's, not the primitive's.
        let mut pose: Option<Entity> = None;
        let mut morphs: Option<Entity> = None;
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let mesh_handle = resources
                    .meshes
                    .iter()
                    .find(|handle| {
                        handle.mesh_index == MeshId::new(mesh.index())
                            && handle.primitive_index == PrimitiveId::new(primitive.index())
                    })
                    .expect("a document mesh must be inserted before the node that draws it");
                // The base-color factor tints the instance whether or not the
                // material samples a texture; a primitive naming no material
                // falls back to the glTF default, opaque white.
                let tint: glam::Vec4 = primitive
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_factor()
                    .into();
                // The `alphaMode: MASK` cutoff the shader compares against is
                // read before the primitive is handed to the material lookup,
                // which takes it by value.
                let cutoff = alpha_cutoff(&primitive.material());
                let material = samples_base_color_texture(&primitive)
                    .then(|| self.primitive_material(primitive, &resources.materials));
                let z_sorted = mesh_handle.key.options.blend.is_some();
                let skinned = mesh_handle.mesh.skinned;
                let binding = skinned.then(|| {
                    let pose = *pose.get_or_insert_with(|| {
                        world.spawn_named(
                            "gltf skin pose",
                            bundle![self.skin_pose(NodeId::new(node.index()))],
                        )
                    });
                    SkinBinding { pose }
                });
                let morphed = mesh_handle.mesh.morph_targets > 0;
                let morph_binding = morphed.then(|| {
                    let weights = *morphs.get_or_insert_with(|| {
                        world.spawn_named(
                            "gltf morph weights",
                            bundle![MorphWeights {
                                weights: self.morph_weights(NodeId::new(node.index()))
                            }],
                        )
                    });
                    MorphBinding { weights }
                });
                // The components a primitive carries vary by material,
                // skin and blend mode, so the bundle is assembled rather than
                // written as a tuple: an absent material is a component left
                // out, not a `None` the renderer would have to look through.
                let mut bundle = DynamicBundle::new().named("gltf primitive");
                bundle.push_component(transform.clone());
                bundle.push_component(mesh_handle.mesh.clone());
                bundle.push_component(GpuRenderPipeline::new(mesh_handle.key.clone()));
                bundle.push_component(InstanceColor::new(tint));
                // The cutoff is per-instance state, so the entity carries it
                // and the shader reads it out of the instance stream.
                if let Some(cutoff) = cutoff {
                    bundle.push_component(InstanceCutoff::new(cutoff));
                }
                if let Some(material) = material {
                    bundle.push_component(material.bind_group.clone());
                }
                if let Some(binding) = binding {
                    bundle.push_component(binding);
                }
                if let Some(binding) = morph_binding {
                    bundle.push_component(binding);
                }
                if z_sorted {
                    bundle.push_component(ZSortedDrawing);
                }
                entities.push(world.spawn(bundle));
            }
        }
        GltfNode {
            node: NodeId::new(index),
            entities,
            pose,
            weights: morphs,
        }
    }

    /// Spawn every node reachable from the document's default scene.
    ///
    /// Returns one [`GltfNode`] per visited node, in the order the walk
    /// reaches them; a node with no mesh has none of its own but is still
    /// returned, so the list maps a document node to its entities. Nodes are
    /// visited root-first and each subtree fully; every node with a mesh
    /// contributes its primitives exactly where its world-space transform sits
    /// in the hierarchy.
    ///
    /// # Panics
    ///
    /// If the document declares no default scene, or any spawn fails (see
    /// [`Self::spawn_node`]).
    pub fn spawn_default_scene(
        &self,
        world: &mut ScopeMut<'_>,
        resources: &GltfResources,
    ) -> Vec<GltfNode> {
        let scene = self
            .document
            .default_scene()
            .expect("the glTF document declares a default scene");
        let mut nodes = Vec::new();
        let mut stack: Vec<NodeId> = scene
            .nodes()
            .map(|node| NodeId::new(node.index()))
            .collect();
        while let Some(node) = stack.pop() {
            nodes.push(self.spawn_node(world, node, resources));
            stack.extend(
                self.node(node.index())
                    .children()
                    .map(|child| NodeId::new(child.index())),
            );
        }
        nodes
    }

    /// The inserted material a primitive draws with.
    ///
    /// A primitive without a material index, or whose material is `None` in
    /// `materials` (no base-color texture), draws untextured.
    fn primitive_material<'m>(
        &self,
        primitive: gltf::Primitive<'_>,
        materials: &'m [Option<GltfMaterial>],
    ) -> &'m GltfMaterial {
        match primitive.material().index() {
            Some(index) => materials
                .get(index)
                .and_then(Option::as_ref)
                .expect("every material a spawned mesh reads must have been inserted"),
            None => panic!("a primitive that samples a texture must name a material"),
        }
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

    /// Whether image `image` uploads as a texture a filtering sampler can
    /// sample — the same answer [`Self::insert_image`] bakes into the handle.
    fn image_filtering(&self, image: usize, device: &wgpu::Device) -> bool {
        image_format(&self.images[image], device).filtering
    }

    /// How image `image`'s texels have to be expanded once sampled: nothing for
    /// a texture that already carries RGB, and the matching base-color layout
    /// for one carrying a single luma channel, or luma and alpha.
    ///
    /// Both bit depths are affected. Neither `R8Unorm` nor `R16Float` can
    /// decode sRGB for itself — WebGPU attaches the transfer function only to
    /// four-channel formats — so the shader does it, on the assumption the
    /// grayscale source is sRGB-encoded just as an RGB one would be.
    fn image_luminance(&self, image: usize) -> BaseColorChannels {
        match self.images[image].format {
            gltf::image::Format::R8 | gltf::image::Format::R16 => BaseColorChannels::Luminance,
            gltf::image::Format::R8G8 | gltf::image::Format::R16G16 => {
                BaseColorChannels::LuminanceAlpha
            }
            _ => BaseColorChannels::Rgba,
        }
    }

    fn material(&self, material: usize) -> gltf::Material<'_> {
        self.document
            .materials()
            .nth(material)
            .expect("glTF material index in bounds")
    }
}
