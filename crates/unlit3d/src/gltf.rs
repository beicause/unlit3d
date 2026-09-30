//! Load glTF documents and patch them into another world's
//! [`MeshSource`]: upload textures, materials and meshes into an existing
//! resource graph, unload them again, and spawn entities that draw the
//! document's default scene.
//!
//! [`UnlitGltf`] owns only the slim, replayable state of a loaded document —
//! the parsed [`gltf::Document`], its decoded buffers and images, and each
//! node's world-space transform — plus nothing else. It holds no [`World`],
//! no [`MeshSource`] and no GPU resources of its own. Everything GPU lives in
//! the graph of the `MeshSource` the caller patches, so a document can be
//! inserted into any number of worlds, and each insertion keeps the handles
//! that unload it again.
//!
//! # Usage
//!
//! ```no_run
//! use unlit3d::gltf::UnlitGltf;
//! # fn run() -> Result<(), gltf::Error> {
//! let gltf = UnlitGltf::load("model.glb")?;
//! # Ok(()) }
//! ```
//!
//! The document is inserted piece by piece — images, then materials, then
//! meshes — and the pieces are unloaded the same way:
//!
//! ```no_run
//! # use unlit3d::gltf::UnlitGltf;
//! # use unlit3d::prelude::{MeshSource, World};
//! # fn run(source: &mut MeshSource, world: &mut World) -> Result<(), gltf::Error> {
//! let gltf = UnlitGltf::load("model.glb")?;
//! let images = gltf.insert_images(source, world);
//! let materials = gltf.insert_materials(source, world, &images);
//! let meshes = gltf.insert_meshes(source, world);
//! let entities = gltf.spawn_default_scene(world, &meshes, &materials);
//! // ... render ...
//! gltf.unload_materials(source, world, &materials);
//! gltf.unload_meshes(source, world, &meshes);
//! gltf.unload_images(source, world, &images);
//! # Ok(()) }
//! ```
//!
//! Spawning entities never spawns a camera: the renderer draws with the first
//! [`Camera`](crate::components::Camera) in the world, so one belongs to the
//! caller. A document that ships its own cameras can offer one with
//! [`UnlitGltf::spawn_camera`], which is worth doing only for a world that has
//! none.
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
//! expands it. The built-in unlit shader does, under
//! `BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA`, which
//! [`UnlitGltf::pipeline_key`] sets for exactly those uploads: the texel
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
//! Everything else a glTF document can carry is ignored: no skinning, morph
//! targets, normals, tangents, alpha cutoff (`MASK`), double-sided rendering
//! or animations. A mesh whose primitive uses techniques outside this subset
//! still uploads and spawns — it just renders without them.
//!
//! Every handle returned here must be given back to the matching `unload_*`
//! call when the resource is no longer wanted; the graph does not otherwise
//! drop what a handle names.
//!
//! `UnlitGltf::load("model.glb")` in the examples above opens a local file —
//! see [`UnlitGltf::load`] and [`UnlitGltf::from_buffer`] for how a document
//! reaches the module in the first place.

use std::path::Path;

use crate::components::{
    Camera, GpuMaterial, GpuMesh, InstanceColor, Transform, UnlitPipeline, ZSortedDrawing,
};
use crate::mesh::UnlitMeshDesc;
use crate::mesh_source::{MeshSource, UnlitPipelineKey};
use unlit_ecs::prelude::{Entity, World};
use unlit_wgpu::pipeline::{UnlitFlags, UnlitOptions};
use unlit_wgpu::resources::{ResourceId, TextureExt, TextureView};

/// A loaded glTF document, ready to be patched into another world.
///
/// Construct with [`Self::load`] or [`Self::from_buffer`]. The document is
/// parsed and its buffers and images decoded eagerly; no GPU resource exists
/// until an `insert_*` call uploads one into some world's
/// [`MeshSource`](crate::mesh_source::MeshSource).
pub struct UnlitGltf {
    document: gltf::Document,
    buffers: Vec<gltf::buffer::Data>,
    images: Vec<gltf::image::Data>,
    /// World-space transform of every node, indexed by node index.
    node_transforms: Vec<Transform>,
    /// World-space matrix of every node, indexed by node index.
    ///
    /// Kept beside the decomposed transform because a camera needs the matrix
    /// itself: decomposing drops shear, and the view matrix of a node whose
    /// ancestors shear has to keep it.
    node_matrices: Vec<glam::Mat4>,
}

/// A texture uploaded from a glTF image.
///
/// Produced by [`UnlitGltf::insert_image`] / [`UnlitGltf::insert_images`],
/// given back to [`UnlitGltf::unload_image`] / [`UnlitGltf::unload_images`].
#[derive(Clone, Debug)]
pub struct GltfImage {
    /// The index of the image in the glTF document.
    pub image: usize,
    /// The uploaded texture holding the image's decoded pixels.
    pub texture: ResourceId<wgpu::Texture>,
    /// A default view of that texture, for callers that sample it directly.
    pub view: ResourceId<TextureView>,
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
/// Produced by [`UnlitGltf::insert_material`] /
/// [`UnlitGltf::insert_materials`], given back to
/// [`UnlitGltf::unload_material`] / [`UnlitGltf::unload_materials`].
#[derive(Clone, Debug)]
pub struct GltfMaterial {
    /// The index of the material in the glTF document.
    pub material: usize,
    /// The bind group that samples the base-color texture.
    pub bind_group: GpuMaterial,
    /// The texture view the bind group samples.
    pub view: ResourceId<TextureView>,
    /// The sampler the bind group uses.
    pub sampler: ResourceId<wgpu::Sampler>,
}

/// A mesh uploaded from one glTF primitive.
///
/// Produced by [`UnlitGltf::insert_mesh`] / [`UnlitGltf::insert_meshes`],
/// given back to [`UnlitGltf::unload_mesh`] / [`UnlitGltf::unload_meshes`].
///
/// The key is derived from the primitive's own attributes and is the key the
/// mesh was uploaded with; spawning an entity from this handle reuses the same
/// key, so the drawn variant always matches the uploaded vertex layout.
#[derive(Clone, Debug)]
pub struct GltfMesh {
    /// The index of the mesh in the glTF document.
    pub mesh_index: usize,
    /// The index of the primitive inside that mesh.
    pub primitive_index: usize,
    /// The key the mesh was uploaded with.
    pub key: UnlitPipelineKey,
    /// The uploaded GPU mesh.
    pub mesh: GpuMesh,
}

impl UnlitGltf {
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
    pub fn from_buffer(bytes: &[u8]) -> Result<Self, gltf::Error> {
        let (document, buffers, images) = gltf::import_slice(bytes)?;
        Ok(Self::from_parts(document, buffers, images))
    }

    fn from_parts(
        document: gltf::Document,
        buffers: Vec<gltf::buffer::Data>,
        images: Vec<gltf::image::Data>,
    ) -> Self {
        let node_matrices = node_world_matrices(&document);
        let node_transforms = node_matrices.iter().map(|&m| decompose(m)).collect();
        Self {
            document,
            buffers,
            images,
            node_transforms,
            node_matrices,
        }
    }

    /// The [`UnlitPipelineKey`] the primitive renders with.
    ///
    /// The key always carries the position and instance streams. It reads the
    /// UV and base-color-texture streams when the primitive carries
    /// `TEXCOORD_0` *and* its material declares a base-color texture, and the
    /// vertex-color stream when the primitive carries `COLOR_0`. A material
    /// whose `alphaMode` is `BLEND` also blends, so the key is drawn with
    /// [`wgpu::BlendState::ALPHA_BLENDING`]; everything else follows
    /// [`UnlitOptions::standard`], so the key is exactly what
    /// [`Self::insert_mesh`] uploads with.
    pub fn pipeline_key(
        &self,
        device: &wgpu::Device,
        mesh: usize,
        primitive: usize,
    ) -> UnlitPipelineKey {
        let primitive = self.primitive(mesh, primitive);
        let material = primitive.material();
        let mut flags = UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_INSTANCE;
        let texture = material.pbr_metallic_roughness().base_color_texture();
        if primitive.get(&gltf::Semantic::TexCoords(0)).is_some() && texture.is_some() {
            flags |= UnlitFlags::VERTEX_UV | UnlitFlags::BASE_COLOR_TEXTURE;
        }
        if primitive.get(&gltf::Semantic::Colors(0)).is_some() {
            flags |= UnlitFlags::VERTEX_COLOR;
        }
        let mut options = UnlitOptions::standard(device).with_flags(flags);
        // The texture the material samples decides whether the base-color
        // binding is a filtering one: the pipeline and the material bind group
        // have to agree, or the group does not fit the pipeline.
        if let Some(info) = &texture {
            let image = info.texture().source().index();
            options.texture_filtering = self.image_filtering(image, device);
            // A one- or two-channel texture uploads in a format that cannot
            // decode sRGB for itself, so the shader expands the texel and
            // decodes the luminance instead.
            options.flags |= self.image_luminance(image);
        }
        // glTF base colors and textures are straight (non-premultiplied)
        // alpha, which is exactly what `ALPHA_BLENDING` composites.
        if material.alpha_mode() == gltf::material::AlphaMode::Blend {
            options.color_target.blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        }
        UnlitPipelineKey::new(options)
    }

    // -- images ---------------------------------------------------------------

    /// Upload `image` into `source`'s resource graph and return its handle.
    ///
    /// The texture holds the image's decoded pixels in the GPU format closest
    /// to them — see the [module docs](self#texture-formats); its sampler is
    /// not uploaded here, because that belongs to the material that samples
    /// it.
    pub fn insert_image(&self, source: &mut MeshSource, world: &World, image: usize) -> GltfImage {
        let data = &self.images[image];
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
    pub fn insert_images(&self, source: &mut MeshSource, world: &World) -> Vec<GltfImage> {
        (0..self.document.images().len())
            .map(|image| self.insert_image(source, world, image))
            .collect()
    }

    /// Unload `image` — the texture and its default view — from the graph.
    ///
    /// Materials sampling the texture are removed as well: they depend on it,
    /// so they cannot outlive it. Unload materials before their images if the
    /// handles must stay valid.
    pub fn unload_image(&self, source: &mut MeshSource, world: &World, image: &GltfImage) {
        MeshSource::graph(world, source.context()).remove(image.texture);
    }

    /// Unload every image named by `images`.
    pub fn unload_images(&self, source: &mut MeshSource, world: &World, images: &[GltfImage]) {
        for image in images {
            self.unload_image(source, world, image);
        }
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
        world: &World,
        material: usize,
        images: &[GltfImage],
    ) -> Option<GltfMaterial> {
        let data = self.material(material);
        let info = data.pbr_metallic_roughness().base_color_texture()?;
        let image = images
            .iter()
            .find(|image| image.image == info.texture().source().index())
            .expect("the base-color texture's image must be inserted before the material");

        // A material group exists only for a variant that samples a base-color
        // texture; build the bind group against exactly that variant. Its
        // layout is the filtering one only when the image can be filtered, and
        // the sampler has to be non-filtering in step with it.
        let key = textured_key(&source.device(world), image.filtering);
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
                let graph = MeshSource::graph(world, source.context());
                graph
                    .get(image.texture)
                    .expect("the image's texture is in the graph")
                    .clone()
            };
            let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
            let mut graph = MeshSource::graph(world, source.context());
            let view = graph.insert_strong(view, None);
            graph.add_dependency(view, image.texture);
            view
        };

        let bind_group = source
            .allocate_unlit_material(world, &key, view, sampler)
            .expect("a base-color variant builds a material bind group");
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
        world: &World,
        images: &[GltfImage],
    ) -> Vec<Option<GltfMaterial>> {
        (0..self.document.materials().len())
            .map(|material| self.insert_material(source, world, material, images))
            .collect()
    }

    /// Unload `material` — its bind group, view and sampler — from the graph.
    pub fn unload_material(&self, source: &mut MeshSource, world: &World, material: &GltfMaterial) {
        source.remove_material(world, material.bind_group.clone());
        let mut graph = MeshSource::graph(world, source.context());
        graph.remove(material.view);
        graph.remove(material.sampler);
    }

    /// Unload every material named by `materials`, skipping the `None` ones.
    pub fn unload_materials(
        &self,
        source: &mut MeshSource,
        world: &World,
        materials: &[Option<GltfMaterial>],
    ) {
        for material in materials.iter().flatten() {
            self.unload_material(source, world, material);
        }
    }

    // -- meshes ---------------------------------------------------------------

    /// Upload the primitive into `source`'s resource graph and return its
    /// handle.
    ///
    /// The key is derived from the primitive's own attributes (see
    /// [`Self::pipeline_key`]): TEXCOORD_0 and a base-color-textured material
    /// enable the UV and base-color-texture streams, COLOR_0 the vertex-color
    /// stream, positions and an instance stream always. A primitive read with
    /// streams its attributes do not declare would panic, so the derivation is
    /// the only safe choice here. Skins, morph targets, normals and tangents
    /// are ignored (see the module docs).
    ///
    /// # Panics
    ///
    /// If the primitive carries no `POSITION` attribute.
    pub fn insert_mesh(
        &self,
        source: &mut MeshSource,
        world: &World,
        mesh: usize,
        primitive: usize,
    ) -> GltfMesh {
        let key = self.pipeline_key(&source.device(world), mesh, primitive);
        let primitive = self.primitive(mesh, primitive);
        let reader = primitive.reader(|buffer| Some(&self.buffers[buffer.index()]));

        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .expect("a glTF primitive carries its POSITION attribute")
            .collect();
        let uvs: Option<Vec<[f32; 2]>> = reader
            .read_tex_coords(0)
            .map(|uvs| uvs.into_f32().collect());
        let colors: Option<Vec<[u8; 4]>> = reader
            .read_colors(0)
            .map(|colors| colors.into_rgba_u8().collect());
        let indices: Option<Vec<u32>> = reader.read_indices().map(|i| i.into_u32().collect());

        let gpu_mesh = source.allocate_unlit_mesh(
            world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: key.options.flags.contains(UnlitFlags::VERTEX_UV).then(|| {
                    uvs.as_deref()
                        .expect("the key reads uvs, so the primitive has TEXCOORD_0")
                }),
                colors: key
                    .options
                    .flags
                    .contains(UnlitFlags::VERTEX_COLOR)
                    .then(|| {
                        colors
                            .as_deref()
                            .expect("the key reads colors, so the primitive has COLOR_0")
                    }),
                indices: indices.as_deref(),
                joints: None,
                weights: None,
                morph_targets: &[],
            },
        );

        GltfMesh {
            mesh_index: mesh,
            primitive_index: primitive.index(),
            key,
            mesh: gpu_mesh,
        }
    }

    /// Upload every primitive of every mesh of the document.
    ///
    /// The vector is ordered document-mesh-major, then primitive-minor: first
    /// all of mesh 0's primitives, then mesh 1's, and so on.
    pub fn insert_meshes(&self, source: &mut MeshSource, world: &World) -> Vec<GltfMesh> {
        let mut meshes = Vec::new();
        for mesh in 0..self.document.meshes().len() {
            for primitive in 0..self.mesh(mesh).primitives().count() {
                meshes.push(self.insert_mesh(source, world, mesh, primitive));
            }
        }
        meshes
    }

    /// Unload `mesh` from the graph.
    pub fn unload_mesh(&self, source: &mut MeshSource, world: &World, mesh: &GltfMesh) {
        source.remove_mesh(world, mesh.mesh.clone());
    }

    /// Unload every mesh named by `meshes`.
    pub fn unload_meshes(&self, source: &mut MeshSource, world: &World, meshes: &[GltfMesh]) {
        for mesh in meshes {
            self.unload_mesh(source, world, mesh);
        }
    }

    // -- spawning -------------------------------------------------------------

    /// Spawn the entities that draw `node`'s mesh into `world`.
    ///
    /// Returns one entity per primitive of the node's mesh — each carrying the
    /// node's world-space [`Transform`], the uploaded [`GpuMesh`], the
    /// pipeline for the mesh's key and an [`InstanceColor`] tinted with the
    /// material's base-color factor. A primitive whose key reads a base-color
    /// texture also carries the matching [`GpuMaterial`]. A blended primitive
    /// ([`gltf::material::AlphaMode::Blend`]) also carries
    /// [`ZSortedDrawing`], so it is drawn after the opaque geometry and
    /// composited back-to-front. Children are not spawned; use
    /// [`Self::spawn_default_scene`] for the whole scene.
    ///
    /// # Panics
    ///
    /// If the node's mesh (or a mesh it shares primitives with) was not
    /// inserted, or a primitive's material is `None` in `materials` while its
    /// key reads a base-color texture.
    pub fn spawn_node(
        &self,
        world: &mut World,
        node: usize,
        meshes: &[GltfMesh],
        materials: &[Option<GltfMaterial>],
    ) -> Vec<Entity> {
        let transform = self.node_transforms[node].clone();
        let node = self.node(node);
        let mut entities = Vec::new();
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let mesh_handle = meshes
                    .iter()
                    .find(|handle| {
                        handle.mesh_index == mesh.index()
                            && handle.primitive_index == primitive.index()
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
                let material = mesh_handle
                    .key
                    .options
                    .flags
                    .contains(UnlitFlags::BASE_COLOR_TEXTURE)
                    .then(|| self.primitive_material(primitive, materials));
                let z_sorted = mesh_handle.key.options.color_target.blend.is_some();
                let bundle = (
                    transform.clone(),
                    mesh_handle.mesh.clone(),
                    UnlitPipeline::new(mesh_handle.key.clone()),
                    InstanceColor::new(tint),
                );
                match (material, z_sorted) {
                    (Some(material), true) => entities.push(world.spawn((
                        bundle.0,
                        bundle.1,
                        bundle.2,
                        material.bind_group.clone(),
                        bundle.3,
                        ZSortedDrawing,
                    ))),
                    (Some(material), false) => entities.push(world.spawn((
                        bundle.0,
                        bundle.1,
                        bundle.2,
                        material.bind_group.clone(),
                        bundle.3,
                    ))),
                    (None, true) => entities.push(world.spawn((
                        bundle.0,
                        bundle.1,
                        bundle.2,
                        bundle.3,
                        ZSortedDrawing,
                    ))),
                    (None, false) => entities.push(world.spawn(bundle)),
                }
            }
        }
        entities
    }

    /// Spawn the [`Camera`] a node carries, placed by the node's world
    /// transform.
    ///
    /// Returns `None` when the node has no camera, so a caller can walk a
    /// document's nodes without asking first.
    ///
    /// `aspect_ratio` is the viewport's width over its height; it is used only
    /// when the document leaves the camera's own `aspectRatio` unset, since a
    /// document that states one should keep it.
    ///
    /// # Which camera draws
    ///
    /// The renderer draws with the *first* [`Camera`] in the world, so a world
    /// that already has one keeps drawing through it — spawning a document's
    /// camera beside it changes nothing. A caller that wants the document's
    /// camera to draw must not give the world another one.
    ///
    /// The transform is applied as a view matrix, so the node's local `-Z` is
    /// where the camera looks — glTF's own convention for a camera node.
    pub fn spawn_camera(
        &self,
        world: &mut World,
        node: usize,
        aspect_ratio: f32,
    ) -> Option<Entity> {
        let camera = self.node(node).camera()?;
        let matrix = self.node_matrices[node];
        // Building the view from the decomposed rotation and translation
        // rather than inverting the node's matrix keeps a degenerate node
        // (a zero scale on some axis) from producing a matrix full of NaNs —
        // a camera has no geometry to collapse.
        let transform = decompose(matrix);
        let position = transform.translation;
        let view = glam::Mat4::from_rotation_translation(transform.rotation, position).inverse();
        let clip_from_world = projection_matrix(&camera, aspect_ratio) * view;
        Some(world.spawn((Camera {
            clip_from_world,
            position,
            active: true,
        },)))
    }

    /// Spawn one entity per primitive of every node reachable from the
    /// document's default scene.
    ///
    /// Nodes are visited root-first and each subtree fully; every node with a
    /// mesh contributes its primitives exactly where its world-space transform
    /// sits in the hierarchy.
    ///
    /// # Panics
    ///
    /// If the document declares no default scene, or any spawn fails (see
    /// [`Self::spawn_node`]).
    pub fn spawn_default_scene(
        &self,
        world: &mut World,
        meshes: &[GltfMesh],
        materials: &[Option<GltfMaterial>],
    ) -> Vec<Entity> {
        let scene = self
            .document
            .default_scene()
            .expect("the glTF document declares a default scene");
        let mut entities = Vec::new();
        let mut stack: Vec<usize> = scene.nodes().map(|node| node.index()).collect();
        while let Some(node) = stack.pop() {
            entities.extend(self.spawn_node(world, node, meshes, materials));
            stack.extend(self.node(node).children().map(|child| child.index()));
        }
        entities
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
            None => panic!("a primitive whose key samples a texture must name a material"),
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
    /// a texture that already carries RGB, and the matching luminance flag for
    /// one carrying a single luma channel, or luma and alpha.
    ///
    /// Both bit depths are affected. Neither `R8Unorm` nor `R16Float` can
    /// decode sRGB for itself — WebGPU attaches the transfer function only to
    /// four-channel formats — so the shader does it, on the assumption the
    /// grayscale source is sRGB-encoded just as an RGB one would be.
    fn image_luminance(&self, image: usize) -> UnlitFlags {
        match self.images[image].format {
            gltf::image::Format::R8 | gltf::image::Format::R16 => UnlitFlags::BASE_COLOR_LUMINANCE,
            gltf::image::Format::R8G8 | gltf::image::Format::R16G16 => {
                UnlitFlags::BASE_COLOR_LUMINANCE_ALPHA
            }
            _ => UnlitFlags::empty(),
        }
    }

    fn material(&self, material: usize) -> gltf::Material<'_> {
        self.document
            .materials()
            .nth(material)
            .expect("glTF material index in bounds")
    }
}

/// Round `value` up to the next multiple of `alignment`.
fn align_up(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

/// The uploaded form of a glTF image: its texture format, and whether a
/// sampler is allowed to filter it.
///
/// glTF images arrive in ten pixel layouts — one to four channels of 8-bit,
/// 16-bit or 32-bit float — and there is a GPU format of nearly the same shape
/// for each, so an image keeps its channels and its precision instead of being
/// widened to RGBA8.
///
/// `filtering` is what the material's sampler and bind-group layout are built
/// around: a 32-bit float texture is an `unfilterable-float` unless the device
/// has `Features::FLOAT32_FILTERABLE`, and an unfilterable texture may only be
/// bound to a non-filtering sampler, which is a different bind-group layout
/// from a filtering one.
#[derive(Clone, Copy, Debug)]
struct ImageFormat {
    format: wgpu::TextureFormat,
    filtering: bool,
}

impl ImageFormat {
    /// The bytes one texel of the format occupies.
    fn texel_bytes(self) -> u32 {
        texel_bytes(self.format)
    }
}

/// The bytes one texel of an upload format occupies.
fn texel_bytes(format: wgpu::TextureFormat) -> u32 {
    format
        .block_copy_size(Some(wgpu::TextureAspect::All))
        .expect("a sampled color format has a texel block size")
}

/// Pick the upload format for a decoded glTF image.
///
/// The 8-bit layouts keep their channel count, and their pixels are uploaded
/// in the sRGB format of that width so sampling decodes them — which is what
/// the glTF spec asks of a base-color texture. `R8G8B8` widens to
/// `Rgba8UnormSrgb` because there is no three-channel sRGB format; `R8G8`
/// stays two channels, which is the luma/alpha pair the glTF loader decodes a
/// grayscale-alpha PNG into.
///
/// The 16-bit and 32-bit layouts upload as half and single precision floats.
/// Half float needs no device feature at all — unlike `R16Unorm` and friends,
/// which need `Features::TEXTURE_FORMAT_16BIT_NORM` — so an image never has to
/// fall back to a narrower format. `Rgba32Float` is the only upload whose
/// filtering depends on the device.
fn image_format(data: &gltf::image::Data, device: &wgpu::Device) -> ImageFormat {
    use gltf::image::Format as F;
    use wgpu::TextureFormat as T;
    let format = match data.format {
        F::R8 => T::R8Unorm,
        F::R8G8 => T::Rg8Unorm,
        // There is no three-channel sRGB format, so the three-channel layouts
        // widen by one opaque channel.
        F::R8G8B8 => T::Rgba8UnormSrgb,
        F::R8G8B8A8 => T::Rgba8UnormSrgb,
        F::R16 => T::R16Float,
        F::R16G16 => T::Rg16Float,
        // Nor a three-channel float one.
        F::R16G16B16 => T::Rgba16Float,
        F::R16G16B16A16 => T::Rgba16Float,
        F::R32G32B32FLOAT => T::Rgba32Float,
        F::R32G32B32A32FLOAT => T::Rgba32Float,
    };
    let filtering = matches!(
        format.sample_type(Some(wgpu::TextureAspect::All), Some(device.features())),
        Some(wgpu::TextureSampleType::Float { filterable: true })
    );
    ImageFormat { format, filtering }
}

/// Encode `data`'s pixels as the rows of a `format` texture.
///
/// A channel the source does not carry — the alpha of a three-channel image,
/// say — is filled with an opaque one, and every value is clamped to `0..=1`
/// on the way in, which is all the wider float formats need. Each row is
/// padded to the 256-byte stride a buffer-to-texture copy asks for.
fn encode_rows(data: &gltf::image::Data, format: wgpu::TextureFormat) -> Vec<u8> {
    let (width, height) = (data.width as usize, data.height as usize);
    let channels = source_channels(data.format);
    let texel = texel_bytes(format) as usize;
    let bytes_per_row = align_up(
        width as u32 * texel as u32,
        wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
    ) as usize;
    let mut rows = vec![0u8; bytes_per_row * height];
    let mut scratch = [0u8; 16];

    for (y, out) in rows.chunks_exact_mut(bytes_per_row).enumerate() {
        for x in 0..width {
            let pixel = &data.pixels[(y * width + x) * channels..];
            let bytes = encode_texel(data.format, format, pixel, &mut scratch);
            out[x * texel..][..texel].copy_from_slice(bytes);
        }
    }
    rows
}

/// The number of bytes one channel of a glTF pixel format occupies.
fn source_channel_bytes(format: gltf::image::Format) -> usize {
    match format {
        gltf::image::Format::R8
        | gltf::image::Format::R8G8
        | gltf::image::Format::R8G8B8
        | gltf::image::Format::R8G8B8A8 => 1,
        gltf::image::Format::R16
        | gltf::image::Format::R16G16
        | gltf::image::Format::R16G16B16
        | gltf::image::Format::R16G16B16A16 => 2,
        gltf::image::Format::R32G32B32FLOAT | gltf::image::Format::R32G32B32A32FLOAT => 4,
    }
}

/// The number of channels a glTF pixel format stores per texel.
fn source_channels(format: gltf::image::Format) -> usize {
    match format {
        gltf::image::Format::R8 | gltf::image::Format::R16 => 1,
        gltf::image::Format::R8G8 | gltf::image::Format::R16G16 => 2,
        gltf::image::Format::R8G8B8 | gltf::image::Format::R16G16B16 => 3,
        gltf::image::Format::R8G8B8A8 | gltf::image::Format::R16G16B16A16 => 4,
        gltf::image::Format::R32G32B32FLOAT => 3,
        gltf::image::Format::R32G32B32A32FLOAT => 4,
    }
}

/// The number of channels an upload format stores per texel.
fn format_channels(format: wgpu::TextureFormat) -> usize {
    match format {
        wgpu::TextureFormat::R8Unorm | wgpu::TextureFormat::R16Float => 1,
        wgpu::TextureFormat::Rg8Unorm | wgpu::TextureFormat::Rg16Float => 2,
        wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Rgba16Float
        | wgpu::TextureFormat::Rgba32Float => 4,
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

/// Encode one source texel into `format`, returning its bytes.
///
/// `scratch` backs the result, so a caller can reuse it across texels.
fn encode_texel<'a>(
    source: gltf::image::Format,
    format: wgpu::TextureFormat,
    pixel: &[u8],
    scratch: &'a mut [u8; 16],
) -> &'a [u8] {
    let channel = source_channel_bytes(source);
    let wanted = format_channels(format);
    let available = source_channels(source);
    for slot in 0..wanted {
        // A channel the source does not carry is an opaque one.
        let value = if slot < available {
            read_channel(source, &pixel[slot * channel..])
        } else {
            1.0
        };
        write_channel(format, slot, value, scratch);
    }
    &scratch[..texel_bytes(format) as usize]
}

/// One channel of a glTF pixel, as a `0..=1` float.
fn read_channel(format: gltf::image::Format, bytes: &[u8]) -> f32 {
    match format {
        gltf::image::Format::R8
        | gltf::image::Format::R8G8
        | gltf::image::Format::R8G8B8
        | gltf::image::Format::R8G8B8A8 => bytes[0] as f32 / 255.0,
        gltf::image::Format::R16
        | gltf::image::Format::R16G16
        | gltf::image::Format::R16G16B16
        | gltf::image::Format::R16G16B16A16 => {
            u16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 65535.0
        }
        gltf::image::Format::R32G32B32FLOAT | gltf::image::Format::R32G32B32A32FLOAT => {
            f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }
    }
}

/// Write one `0..=1` channel of `format`'s `slot` into `scratch`.
fn write_channel(format: wgpu::TextureFormat, slot: usize, value: f32, scratch: &mut [u8; 16]) {
    // `f32::clamp` leaves NaN alone, and a NaN texel would poison every sample
    // it reaches, so fold it to the bottom of the range instead.
    let clamped = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    };
    match format {
        wgpu::TextureFormat::R8Unorm
        | wgpu::TextureFormat::Rg8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb => {
            scratch[slot] = (clamped * 255.0).round() as u8;
        }
        wgpu::TextureFormat::R16Float
        | wgpu::TextureFormat::Rg16Float
        | wgpu::TextureFormat::Rgba16Float => {
            let bits = half::f16::from_f32(clamped).to_bits();
            scratch[slot * 2..][..2].copy_from_slice(&bits.to_le_bytes());
        }
        wgpu::TextureFormat::Rgba32Float => {
            scratch[slot * 4..][..4].copy_from_slice(&clamped.to_le_bytes());
        }
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

/// The key a material bind group is built against: the base-color variant.
///
/// Only `BASE_COLOR_TEXTURE` and `texture_filtering` shape the material group's
/// layout, so any key carrying them works; this is the one a
/// base-color-textured primitive's [`UnlitGltf::pipeline_key`] derives to.
fn textured_key(device: &wgpu::Device, filtering: bool) -> UnlitPipelineKey {
    UnlitPipelineKey::new(UnlitOptions {
        texture_filtering: filtering,
        ..UnlitOptions::standard(device).with_flags(
            UnlitFlags::VERTEX_POSITION
                | UnlitFlags::VERTEX_INSTANCE
                | UnlitFlags::VERTEX_UV
                | UnlitFlags::BASE_COLOR_TEXTURE,
        )
    })
}

/// Translate a glTF sampler to wgpu terms.
///
/// Unspecified filters default to `Linear`, unspecified wrap modes default to
/// `Repeat` — the glTF spec defaults.
fn sampler_descriptor(sampler: &gltf::texture::Sampler<'_>) -> wgpu::SamplerDescriptor<'static> {
    use gltf::texture::{MagFilter, MinFilter};
    let (min_filter, mipmap_filter) = match sampler.min_filter() {
        Some(MinFilter::Nearest) | Some(MinFilter::NearestMipmapNearest) => {
            (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest)
        }
        Some(MinFilter::NearestMipmapLinear) => {
            (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Linear)
        }
        Some(MinFilter::LinearMipmapNearest) => {
            (wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Nearest)
        }
        Some(MinFilter::Linear) | Some(MinFilter::LinearMipmapLinear) | None => {
            (wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear)
        }
    };
    let mag_filter = match sampler.mag_filter() {
        Some(MagFilter::Nearest) => wgpu::FilterMode::Nearest,
        Some(MagFilter::Linear) | None => wgpu::FilterMode::Linear,
    };
    wgpu::SamplerDescriptor {
        label: None,
        address_mode_u: address_mode(sampler.wrap_s()),
        address_mode_v: address_mode(sampler.wrap_t()),
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter,
        min_filter,
        mipmap_filter,
        ..wgpu::SamplerDescriptor::default()
    }
}

fn address_mode(mode: gltf::texture::WrappingMode) -> wgpu::AddressMode {
    use gltf::texture::WrappingMode;
    match mode {
        WrappingMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        WrappingMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        WrappingMode::Repeat => wgpu::AddressMode::Repeat,
    }
}

/// The projection matrix a glTF camera looks through.
///
/// Both projections are built in the convention the unlit pipeline draws with:
/// right-handed, Y-up, clip depth `0..=1` — WebGPU's NDC — so the frustum the
/// renderer culls against matches the one the camera describes.
///
/// A perspective camera becomes an *infinite* reverse projection: glTF's
/// `zfar` is dropped and the far plane goes to infinity. The built-in pipeline
/// draws with a reversed depth buffer (`CompareFunction::Greater`, cleared to
/// `0.0`), so a finite far plane would clip distant geometry that the pipeline
/// is set up to keep.
///
/// `aspect_ratio` is the viewport's width over its height, used when the
/// document leaves the camera's own `aspectRatio` unset.
fn projection_matrix(camera: &gltf::camera::Camera<'_>, aspect_ratio: f32) -> glam::Mat4 {
    use gltf::camera::Projection;
    match camera.projection() {
        Projection::Perspective(perspective) => {
            let aspect = perspective.aspect_ratio().unwrap_or(aspect_ratio);
            glam::camera::rh::proj::directx::perspective_infinite_reverse(
                perspective.yfov(),
                aspect,
                perspective.znear(),
            )
        }
        Projection::Orthographic(orthographic) => glam::camera::rh::proj::directx::orthographic(
            -orthographic.xmag(),
            orthographic.xmag(),
            -orthographic.ymag(),
            orthographic.ymag(),
            orthographic.znear(),
            orthographic.zfar(),
        ),
    }
}

/// The world-space matrix of every node, indexed by node index.
///
/// The parent table is built from each node's children list (glTF nodes have
/// no parent handle), then world matrices are accumulated root-first so a
/// child always sees its parent's matrix before its own is computed.
/// Hierarchies are traversed iteratively so arbitrarily deep scenes cannot
/// overflow the stack.
fn node_world_matrices(document: &gltf::Document) -> Vec<glam::Mat4> {
    let mut parents = vec![None; document.nodes().len()];
    for node in document.nodes() {
        for child in node.children() {
            parents[child.index()] = Some(node.index());
        }
    }

    let mut world = vec![glam::Mat4::IDENTITY; document.nodes().len()];
    let roots: Vec<usize> = (0..document.nodes().len())
        .filter(|&index| parents[index].is_none())
        .collect();
    for root in roots {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let node_data = document.nodes().nth(node).expect("node index in bounds");
            let local = glam::Mat4::from_cols_array_2d(&node_data.transform().matrix());
            world[node] = match parents[node] {
                Some(parent) => world[parent] * local,
                None => local,
            };
            stack.extend(node_data.children().map(|child| child.index()));
        }
    }

    world
}

/// Split a world-space matrix into the parts an entity's
/// [`Transform`] stores: translation, rotation and scale.
///
/// The decomposition is lossy for sheared matrices, and an axis scaled to
/// zero keeps an arbitrary rotation (there is no rotation to preserve along a
/// collapsed axis). Both are fine for an unlit renderer.
fn decompose(matrix: glam::Mat4) -> Transform {
    let translation = matrix.w_axis.truncate();
    let x = matrix.x_axis.truncate();
    let y = matrix.y_axis.truncate();
    let z = matrix.z_axis.truncate();
    let (sx, sy, sz) = (x.length(), y.length(), z.length());
    let rotation = glam::Quat::from_mat3(&glam::Mat3::from_cols(
        axis_or_identity(x, sx),
        axis_or_identity(y, sy),
        axis_or_identity(z, sz),
    ));
    Transform {
        translation,
        rotation,
        scale: glam::Vec3::new(sx, sy, sz),
    }
}

/// A unit-length axis, keeping its direction; identity when collapsed.
fn axis_or_identity(axis: glam::Vec3, length: f32) -> glam::Vec3 {
    if length > f32::EPSILON {
        axis / length
    } else {
        glam::Vec3::X // direction is meaningless along a zero-length axis
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-by-one image of `pixels` in the given format.
    fn image(format: gltf::image::Format, pixels: Vec<u8>) -> gltf::image::Data {
        gltf::image::Data {
            pixels,
            format,
            width: 1,
            height: 1,
        }
    }

    /// The texel `data` encodes into `format`, without its row padding.
    fn encode(format: wgpu::TextureFormat, data: &gltf::image::Data) -> Vec<u8> {
        let texel = texel_bytes(format) as usize;
        let mut out = Vec::new();
        let mut scratch = [0u8; 16];
        out.extend_from_slice(encode_texel(
            data.format,
            format,
            &data.pixels,
            &mut scratch,
        ));
        assert_eq!(out.len(), texel);
        out
    }

    #[test]
    fn a_luma_alpha_image_keeps_luma_in_every_channel_slot_it_has() {
        use gltf::image::Format as F;
        // The loader decodes a grayscale-alpha PNG into two channels, luma then
        // alpha, and `Rg8Unorm` keeps both — the bug this replaces wrote them
        // out as red and green.
        let data = image(F::R8G8, vec![0x40, 0x80]);
        assert_eq!(
            encode(wgpu::TextureFormat::Rg8Unorm, &data),
            vec![0x40, 0x80]
        );
    }

    #[test]
    fn a_three_channel_image_gains_an_opaque_alpha() {
        use gltf::image::Format as F;
        let data = image(F::R8G8B8, vec![1, 2, 3]);
        assert_eq!(
            encode(wgpu::TextureFormat::Rgba8UnormSrgb, &data),
            vec![1, 2, 3, 255]
        );
    }

    #[test]
    fn a_sixteen_bit_image_narrows_to_half_float() {
        use gltf::image::Format as F;
        let data = image(F::R16G16B16A16, vec![0x00, 0x80, 0x00, 0x40, 0, 0, 0, 0]);
        let encoded = encode(wgpu::TextureFormat::Rgba16Float, &data);
        // `32768 / 65535` is a half above a half, `16384 / 65535` a quarter,
        // and the trailing zeroes stay zero.
        let channels = encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| half::f16::from_bits(u16::from_le_bytes(*bytes)).to_f32())
            .collect::<Vec<_>>();
        assert!((channels[0] - 0.5).abs() < 1e-3, "{channels:?}");
        assert!((channels[1] - 0.25).abs() < 1e-3, "{channels:?}");
        assert_eq!(&channels[2..], &[0.0, 0.0]);
    }

    #[test]
    fn a_float_image_out_of_range_clamps() {
        use gltf::image::Format as F;
        let data = image(F::R32G32B32A32FLOAT, {
            let mut pixels = Vec::new();
            pixels.extend_from_slice(&(-1.0f32).to_le_bytes());
            pixels.extend_from_slice(&0.5f32.to_le_bytes());
            pixels.extend_from_slice(&2.0f32.to_le_bytes());
            pixels.extend_from_slice(&f32::NAN.to_le_bytes());
            pixels
        });
        let encoded = encode(wgpu::TextureFormat::Rgba32Float, &data);
        let channels = encoded
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect::<Vec<_>>();
        assert_eq!(channels, vec![0.0, 0.5, 1.0, 0.0]);
    }

    #[test]
    fn rows_are_padded_to_the_copy_stride() {
        use gltf::image::Format as F;
        let data = gltf::image::Data {
            pixels: vec![7; 2 * 4],
            format: F::R8G8B8A8,
            width: 2,
            height: 1,
        };
        let rows = encode_rows(&data, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(rows.len(), wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
        assert_eq!(&rows[..8], &[7; 8]);
        assert_eq!(&rows[8..], &vec![0; 256 - 8][..]);
    }
}
