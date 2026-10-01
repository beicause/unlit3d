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
//! [`Camera`] in the world, so one belongs to the
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
//! A `MASK` material cuts its fragments off below its `alphaCutoff` instead,
//! so it draws binary coverage: opaque where the texel is opaque enough and
//! absent everywhere else, blending with nothing. A primitive carrying both
//! `JOINTS_0` and `WEIGHTS_0` uploads its actual joint stream and draws
//! skinned; [`UnlitGltf::skin_pose`] turns the node's skin into the
//! [`SkinPose`] its joints deform by, and spawning such a node creates the
//! pose entity its mesh binds to. A primitive whose mesh declares a morph
//! target that displaces positions uploads those displacements and spawns with
//! a [`MorphBinding`] to a [`MorphWeights`] entity holding the node's starting
//! weights — see [`UnlitGltf::morph_weights`] — so writing that entity is how
//! a caller morphs the mesh. A `doubleSided` material rasterizes its back
//! faces instead of culling them, so its geometry draws from either side.
//! Everything else a glTF document can carry is ignored: no normals, tangents
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
    Camera, GpuMaterial, GpuMesh, InstanceColor, MorphBinding, MorphWeights, SkinBinding, SkinPose,
    Transform, UnlitPipeline, ZSortedDrawing,
};
use crate::mesh::{UnlitMeshDesc, UnlitMorphTarget};
use crate::mesh_source::{MeshSource, UnlitPipelineKey};
use unlit_ecs::{
    ArchetypeBuilder,
    prelude::{Entity, World},
};
use unlit_wgpu::pipeline::{UnlitFlags, UnlitOptions};
use unlit_wgpu::resources::{ResourceId, TextureExt, TextureView};

/// The cutoff the glTF spec gives a `MASK` material that leaves `alphaCutoff`
/// out.
const DEFAULT_ALPHA_CUTOFF: f32 = 0.5;

/// A loaded glTF document, ready to be patched into another world.
///
/// Construct with [`Self::load`] or [`Self::from_buffer`]. The document is
/// parsed and its buffers and images decoded eagerly; no GPU resource exists
/// until an `insert_*` call uploads one into some world's
/// [`MeshSource`].
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
    /// `TEXCOORD_0` *and* its material declares a base-color texture, the
    /// vertex-color stream when the primitive carries `COLOR_0`, the joint
    /// stream when it carries both `JOINTS_0` and `WEIGHTS_0`, and the morph
    /// displacement stream when it declares a target that displaces
    /// positions. A material
    /// whose `alphaMode` is `BLEND` also blends, so the key is drawn with
    /// [`wgpu::BlendState::ALPHA_BLENDING`], and one whose `alphaMode` is
    /// `MASK` carries [`UnlitFlags::ALPHA_CUTOFF`] so its fragments are
    /// discarded below the cutoff. A `doubleSided` material turns back-face
    /// culling off, so its geometry is drawn from either side; everything else
    /// follows [`UnlitOptions::standard`], so the key is exactly what
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
        // The joint stream is only useful with a skin, and a document can
        // carry `JOINTS_0`/`WEIGHTS_0` without one. Requiring both keeps a
        // mesh whose node names no skin in the opaque, undeformed path.
        if primitive.get(&gltf::Semantic::Joints(0)).is_some()
            && primitive.get(&gltf::Semantic::Weights(0)).is_some()
        {
            flags |= UnlitFlags::VERTEX_JOINTS;
        }
        // Only position displacements are read, so a target that carries
        // none is skipped rather than drawn as a no-op: a mesh whose targets
        // all displace normals deforms nothing here.
        if morph_target_count(&primitive) > 0 {
            flags |= UnlitFlags::MORPH_POSITIONS;
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
        // A `MASK` material is not a translucent one: its fragments are either
        // drawn whole or discarded, so it neither blends nor needs the
        // back-to-front sort a blended one does. The cutoff itself is one
        // material's own value, bound by the material rather than baked into
        // the variant — see [`Self::insert_material`].
        if texture.is_some() && alpha_cutoff(&material).is_some() {
            options.flags |= UnlitFlags::ALPHA_CUTOFF;
        }
        // A `doubleSided` material is visible from behind, so its back faces
        // are rasterized rather than culled. Nothing else about the variant
        // changes: the unlit fragment shader reads no normal, so there is no
        // back-face normal to reverse, and the geometry simply draws from both
        // sides.
        if material.double_sided() {
            options.primitive.cull_mode = None;
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
        let cutoff = alpha_cutoff(&data);
        let key = textured_key(&source.device(world), image.filtering, cutoff.is_some());
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

        let bind_group = match cutoff {
            Some(cutoff) => {
                source.allocate_unlit_material_with_cutoff(world, &key, view, sampler, cutoff)
            }
            None => source.allocate_unlit_material(world, &key, view, sampler),
        }
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
        let joints: Option<Vec<[u16; 4]>> = reader
            .read_joints(0)
            .map(|joints| joints.into_u16().collect());
        let weights: Option<Vec<[f32; 4]>> = reader
            .read_weights(0)
            .map(|weights| weights.into_f32().collect());
        // The displacements a morph target applies, one flat array per target
        // that displaces positions — the count the key was built from, so a
        // target of normals only is dropped here just as it was there.
        let morphs: Vec<Vec<[f32; 3]>> = reader
            .read_morph_targets()
            .filter_map(|(positions, _, _)| positions.map(|deltas| deltas.collect::<Vec<_>>()))
            .collect();
        let morph_targets: Vec<UnlitMorphTarget<'_>> = morphs
            .iter()
            .map(|positions| UnlitMorphTarget { positions })
            .collect();

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
                joints: key
                    .options
                    .flags
                    .contains(UnlitFlags::VERTEX_JOINTS)
                    .then(|| {
                        joints
                            .as_deref()
                            .expect("the key reads joints, so the primitive has JOINTS_0")
                    }),
                weights: key
                    .options
                    .flags
                    .contains(UnlitFlags::VERTEX_JOINTS)
                    .then(|| {
                        weights
                            .as_deref()
                            .expect("the key reads joints, so the primitive has WEIGHTS_0")
                    }),
                morph_targets: &morph_targets,
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
    pub fn skin_pose(&self, node: usize) -> SkinPose {
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
        let world_to_mesh = self.node_matrices[node].inverse();
        let matrices: Vec<glam::Mat4> = skin
            .joints()
            .enumerate()
            .map(|(joint, joint_node)| {
                let joint_world = self.node_matrices[joint_node.index()];
                let bind = inverse_bind.as_ref().map_or(glam::Mat4::IDENTITY, |ibm| {
                    *ibm.get(joint).expect("one inverse bind matrix per joint")
                });
                world_to_mesh * joint_world * bind
            })
            .collect();
        SkinPose::new(matrices)
    }

    /// The number of joints a node's skin has.
    ///
    /// `None` when the node names no skin, so a caller can walk a document
    /// without asking first.
    pub fn skin_joint_count(&self, node: usize) -> Option<usize> {
        Some(self.node(node).skin()?.joints().count())
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
    pub fn morph_weights(&self, node: usize) -> Vec<f32> {
        let Some(mesh) = self.node(node).mesh() else {
            return Vec::new();
        };
        let node_weights = self.node(node).weights();
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
    /// composited back-to-front. A skinned primitive (one whose key reads
    /// joints) carries a [`SkinBinding`] to the node's [`SkinPose`], which is
    /// spawned on an entity of its own so several meshes can share it — see
    /// [`Self::skin_pose`]. Children are not spawned; use
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
        // One pose entity per skinned node, shared by every primitive it
        // draws: the joints are the node's skin, and two primitives of one
        // mesh deform together. The morph weights get an entity of their own
        // for the same reason — they are the node's, not the primitive's.
        let mut pose: Option<Entity> = None;
        let mut morphs: Option<Entity> = None;
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
                let skinned = mesh_handle
                    .key
                    .options
                    .flags
                    .contains(UnlitFlags::VERTEX_JOINTS);
                let binding = skinned.then(|| {
                    let pose =
                        *pose.get_or_insert_with(|| world.spawn((self.skin_pose(node.index()),)));
                    SkinBinding::new(pose)
                });
                let morphed = mesh_handle
                    .key
                    .options
                    .flags
                    .contains(UnlitFlags::MORPH_POSITIONS);
                let morph_binding = morphed.then(|| {
                    let weights = *morphs.get_or_insert_with(|| {
                        world.spawn((MorphWeights::new(self.morph_weights(node.index())),))
                    });
                    MorphBinding::new(weights)
                });
                // The components a primitive carries vary by material,
                // skin and blend mode, so the bundle is assembled rather than
                // written as a tuple: an absent material is a component left
                // out, not a `None` the renderer would have to look through.
                let mut bundle = ArchetypeBuilder::new();
                bundle.push(transform.clone());
                bundle.push(mesh_handle.mesh.clone());
                bundle.push(UnlitPipeline::new(mesh_handle.key.clone()));
                bundle.push(InstanceColor::new(tint));
                if let Some(material) = material {
                    bundle.push(material.bind_group.clone());
                }
                if let Some(binding) = binding {
                    bundle.push(binding);
                }
                if let Some(binding) = morph_binding {
                    bundle.push(binding);
                }
                if z_sorted {
                    bundle.push(ZSortedDrawing);
                }
                entities.push(world.spawn(bundle));
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
/// Only `BASE_COLOR_TEXTURE`, `texture_filtering` and `ALPHA_CUTOFF` shape the
/// material group's layout, so any key carrying them works; this is the one a
/// base-color-textured primitive's [`UnlitGltf::pipeline_key`] derives to.
fn textured_key(device: &wgpu::Device, filtering: bool, cutoff: bool) -> UnlitPipelineKey {
    let mut flags = UnlitFlags::VERTEX_POSITION
        | UnlitFlags::VERTEX_INSTANCE
        | UnlitFlags::VERTEX_UV
        | UnlitFlags::BASE_COLOR_TEXTURE;
    flags.set(UnlitFlags::ALPHA_CUTOFF, cutoff);
    UnlitPipelineKey::new(UnlitOptions {
        texture_filtering: filtering,
        ..UnlitOptions::standard(device).with_flags(flags)
    })
}

/// How many of a primitive's morph targets displace positions.
///
/// A target may displace only normals or tangents; those are ignored here, and
/// a target that carries no positions at all would contribute nothing to a
/// displacement array, so it is not counted and the mesh is not uploaded as
/// morphing. The count is therefore of the targets that *do* displace, in the
/// order they appear — which is the order the shader's weights are indexed in,
/// not the document's own target indices.
fn morph_target_count(primitive: &gltf::Primitive<'_>) -> usize {
    primitive
        .morph_targets()
        .filter(|target| target.positions().is_some())
        .count()
}

/// The alpha a `MASK` material's fragments are cut off at.
///
/// Returns `None` for every other `alphaMode`, for which there is nothing to
/// cut. The value is the material's own `alphaCutoff`, or the spec's default
/// when the material leaves it out.
fn alpha_cutoff(material: &gltf::Material<'_>) -> Option<f32> {
    (material.alpha_mode() == gltf::material::AlphaMode::Mask)
        .then(|| material.alpha_cutoff().unwrap_or(DEFAULT_ALPHA_CUTOFF))
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

    /// A GLB holding one skinned triangle: node 0 draws mesh 0 under skin 0,
    /// whose single joint is node 1 at `translation`.
    ///
    /// The buffer carries the positions, the joint indices and weights that
    /// name the joint, the skin's inverse bind matrix (the identity, so the
    /// joint's own transform is what moves the vertex) and the index buffer.
    /// The JSON is written out by hand rather than through a serializer,
    /// because the crate carries no JSON dependency beyond the loader's own.
    fn skinned_document(translation: [f32; 3]) -> Vec<u8> {
        let mut bin = Vec::new();
        let mut views: Vec<String> = Vec::new();
        let mut add = |bin: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
            views.push(format!(
                r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
                bin.len(),
                bytes.len()
            ));
            bin.extend_from_slice(bytes);
            views.len() - 1
        };

        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let position_view = add(&mut bin, &positions, Some(34962));
        let joints: Vec<u8> = (0..3)
            .flat_map(|_| [0u16, 0, 0, 0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let joint_view = add(&mut bin, &joints, Some(34962));
        let weights: Vec<u8> = (0..3)
            .flat_map(|_| [1.0f32, 0.0, 0.0, 0.0])
            .flat_map(f32::to_le_bytes)
            .collect();
        let weight_view = add(&mut bin, &weights, Some(34962));
        let identity: Vec<u8> = [
            1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
        let bind_view = add(&mut bin, &identity, None);
        let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_view = add(&mut bin, &indices, Some(34963));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }

        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
                "scene":0,
                "scenes":[{{"nodes":[0]}}],
                "nodes":[{{"mesh":0,"skin":0}},
                         {{"name":"joint","translation":[{x},{y},{z}]}}],
                "skins":[{{"joints":[1],"inverseBindMatrices":3}}],
                "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"JOINTS_0":1,"WEIGHTS_0":2}},
                                            "indices":4}}]}}],
                "accessors":[{{"bufferView":{position_view},"componentType":5126,"count":3,
                               "type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]}},
                             {{"bufferView":{joint_view},"componentType":5123,"count":3,"type":"VEC4"}},
                             {{"bufferView":{weight_view},"componentType":5126,"count":3,"type":"VEC4"}},
                             {{"bufferView":{bind_view},"componentType":5126,"count":1,"type":"MAT4"}},
                             {{"bufferView":{index_view},"componentType":5123,"count":3,"type":"SCALAR"}}],
                "bufferViews":[{}],
                "buffers":[{{"byteLength":{}}}]}}"#,
            views.join(", "),
            bin.len(),
            x = translation[0],
            y = translation[1],
            z = translation[2],
        );
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    /// A GLB holding one triangle whose mesh declares `targets` positions
    /// morph targets and states `mesh_weights` on the mesh, optionally
    /// overridden by `node_weights` on the node that draws it.
    ///
    /// Each target displaces a single vertex by `[0, 1, 0]`-ish amounts so a
    /// wrong target stride reads the wrong displacement. A target passed as
    /// `None` declares no positions at all, which is how a normals-only target
    /// is written here.
    fn morphed_document(
        mesh_weights: Option<&[f32]>,
        node_weights: Option<&[f32]>,
        targets: &[Option<[f32; 3]>],
    ) -> Vec<u8> {
        let mut bin = Vec::new();
        let mut views: Vec<String> = Vec::new();
        let mut add = |bin: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
            views.push(format!(
                r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
                bin.len(),
                bytes.len()
            ));
            bin.extend_from_slice(bytes);
            views.len() - 1
        };

        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let mut accessors = vec![format!(
            r#"{{ "bufferView": {}, "componentType": 5126, "count": 3, "type": "VEC3",
                 "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#,
            add(&mut bin, &positions, Some(34962))
        )];
        // One accessor per target that displaces, in target order.
        let mut displacement_accessors = Vec::new();
        for target in targets {
            let Some(delta) = target else {
                continue;
            };
            let mut deltas = [0.0f32; 9];
            deltas[3] = delta[0];
            deltas[4] = delta[1];
            deltas[5] = delta[2];
            let bytes: Vec<u8> = deltas.iter().flat_map(|f| f.to_le_bytes()).collect();
            let view = add(&mut bin, &bytes, None);
            displacement_accessors.push(accessors.len());
            accessors.push(format!(
                r#"{{ "bufferView": {view}, "componentType": 5126, "count": 3, "type": "VEC3" }}"#
            ));
        }
        let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_accessor = accessors.len();
        accessors.push(format!(
            r#"{{ "bufferView": {}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#,
            add(&mut bin, &indices, Some(34963))
        ));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }

        // A target that carries no positions is declared as an empty object,
        // which is what glTF allows and what the loader must skip.
        let target_json: Vec<String> = targets
            .iter()
            .enumerate()
            .map(|(index, target)| match target {
                Some(_) => format!(r#"{{"POSITION":{}}}"#, displacement_accessors[index]),
                None => "{}".to_string(),
            })
            .collect();
        let mesh_weights = mesh_weights
            .map(|weights| {
                let list: Vec<String> = weights.iter().map(|w| w.to_string()).collect();
                format!(r#","weights":[{}]"#, list.join(","))
            })
            .unwrap_or_default();
        let node_weights = node_weights
            .map(|weights| {
                let list: Vec<String> = weights.iter().map(|w| w.to_string()).collect();
                format!(r#","weights":[{}]"#, list.join(","))
            })
            .unwrap_or_default();
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
                "scene":0,
                "scenes":[{{"nodes":[0]}}],
                "nodes":[{{"mesh":0{node_weights}}}],
                "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},
                                            "indices":{index_accessor},
                                            "targets":[{}]}}]{mesh_weights}}}],
                "accessors":[{}],
                "bufferViews":[{}],
                "buffers":[{{"byteLength":{}}}]}}"#,
            target_json.join(", "),
            accessors.join(", "),
            views.join(", "),
            bin.len(),
        );
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    /// A GLB holding one triangle whose material states `doubleSided` or not.
    fn sided_document(double_sided: bool) -> Vec<u8> {
        let mut bin = Vec::new();
        let mut views: Vec<String> = Vec::new();
        let mut add = |bin: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
            views.push(format!(
                r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
                bin.len(),
                bytes.len()
            ));
            bin.extend_from_slice(bytes);
            views.len() - 1
        };
        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let position_view = add(&mut bin, &positions, Some(34962));
        let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_view = add(&mut bin, &indices, Some(34963));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let sided = if double_sided {
            r#","doubleSided":true"#
        } else {
            ""
        };
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
                "scene":0,
                "scenes":[{{"nodes":[0]}}],
                "nodes":[{{"mesh":0}}],
                "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},
                                            "indices":1,
                                            "material":0}}]}}],
                "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1.0,1.0,1.0,1.0]}}{sided}}}],
                "accessors":[
                    {{"bufferView":{position_view},"componentType":5126,"count":3,"type":"VEC3",
                      "min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]}},
                    {{"bufferView":{index_view},"componentType":5123,"count":3,"type":"SCALAR"}}],
                "bufferViews":[{}],
                "buffers":[{{"byteLength":{}}}]}}"#,
            views.join(", "),
            bin.len(),
        );
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    #[test]
    fn a_double_sided_material_culls_nothing() {
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());

        let single = UnlitGltf::from_buffer(&sided_document(false))
            .expect("the single-sided document parses");
        assert_eq!(
            single
                .pipeline_key(&device, 0, 0)
                .options
                .primitive
                .cull_mode,
            Some(wgpu::Face::Back),
            "a material that is not double-sided culls back faces"
        );

        let double =
            UnlitGltf::from_buffer(&sided_document(true)).expect("the sided document parses");
        assert_eq!(
            double
                .pipeline_key(&device, 0, 0)
                .options
                .primitive
                .cull_mode,
            None,
            "a double-sided material culls nothing"
        );
    }

    #[test]
    fn culling_alone_distinguishes_the_two_sided_variants() {
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let single = UnlitGltf::from_buffer(&sided_document(false))
            .expect("the single-sided document parses")
            .pipeline_key(&device, 0, 0);
        let double = UnlitGltf::from_buffer(&sided_document(true))
            .expect("the double-sided document parses")
            .pipeline_key(&device, 0, 0);
        assert_ne!(
            single, double,
            "the two sides must be separate variants so each keeps its own pipeline"
        );
        assert_eq!(
            single.options.flags, double.options.flags,
            "nothing but the cull mode sets the two apart"
        );
    }

    #[test]
    fn a_morph_target_makes_the_key_read_displacements() {
        let gltf = UnlitGltf::from_buffer(&morphed_document(None, None, &[Some([0.0, 1.0, 0.0])]))
            .expect("the morphed document parses");
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let key = gltf.pipeline_key(&device, 0, 0);
        assert!(key.options.flags.contains(UnlitFlags::MORPH_POSITIONS));
    }

    #[test]
    fn a_target_without_positions_does_not_make_the_mesh_morph() {
        // A normals-only target displaces nothing this loader reads, so the
        // mesh stays in the rigid path rather than drawing an empty morph.
        let gltf = UnlitGltf::from_buffer(&morphed_document(None, None, &[None]))
            .expect("the morphed document parses");
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let key = gltf.pipeline_key(&device, 0, 0);
        assert!(!key.options.flags.contains(UnlitFlags::MORPH_POSITIONS));
        assert!(gltf.morph_weights(0).is_empty());
    }

    #[test]
    fn a_node_overrides_the_mesh_weights() {
        let gltf = UnlitGltf::from_buffer(&morphed_document(
            Some(&[0.25]),
            Some(&[0.75]),
            &[Some([0.0, 1.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(0), vec![0.75]);

        let gltf = UnlitGltf::from_buffer(&morphed_document(
            Some(&[0.25]),
            None,
            &[Some([1.0, 0.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(0), vec![0.25]);
    }

    #[test]
    fn morph_weights_are_padded_to_the_target_count() {
        // A document that states fewer weights than it has targets would make
        // the renderer reject the mesh, so the shortfall is padded with the
        // undeformed weight rather than passed on.
        let gltf = UnlitGltf::from_buffer(&morphed_document(
            Some(&[0.5]),
            None,
            &[Some([0.0, 1.0, 0.0]), Some([1.0, 0.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(0), vec![0.5, 0.0]);

        // And one that states more is truncated, for the same reason.
        let gltf = UnlitGltf::from_buffer(&morphed_document(
            Some(&[0.5, 1.0]),
            None,
            &[Some([0.0, 1.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(0), vec![0.5]);
    }

    #[test]
    fn a_mesh_without_targets_starts_undeformed() {
        let gltf = UnlitGltf::from_buffer(&morphed_document(None, None, &[]))
            .expect("the morphed document parses");
        assert!(gltf.morph_weights(0).is_empty());
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        assert!(
            !gltf
                .pipeline_key(&device, 0, 0)
                .options
                .flags
                .contains(UnlitFlags::MORPH_POSITIONS)
        );
    }

    #[test]
    fn a_rest_skin_pose_deforms_nothing() {
        // The joint sits at rest, so its world matrix is the inverse bind
        // matrix's inverse and every joint matrix comes out the identity: the
        // pose a document uploads at rest has to leave the mesh exactly where
        // it is.
        let gltf = UnlitGltf::from_buffer(&skinned_document([0.0, 0.0, 0.0]))
            .expect("the skinned document parses");
        let pose = gltf.skin_pose(0);
        assert_eq!(pose.matrices.len(), 1, "the skin has one joint");
        assert!(
            pose.matrices[0].abs_diff_eq(glam::Mat4::IDENTITY, 1e-6),
            "{}",
            pose.matrices[0]
        );
    }

    #[test]
    fn a_joint_away_from_its_bind_pose_moves_the_mesh() {
        // An inverse bind matrix records where a joint was *bound*, so a joint
        // node that sits somewhere else pulls the vertices it weights to
        // itself: the pose is the joint's world matrix against its bind
        // matrix, not the identity.
        let gltf = UnlitGltf::from_buffer(&skinned_document([3.0, -2.0, 1.0]))
            .expect("the skinned document parses");
        let pose = gltf.skin_pose(0);
        assert!(
            pose.matrices[0].abs_diff_eq(
                glam::Mat4::from_translation(glam::Vec3::new(3.0, -2.0, 1.0)),
                1e-6
            ),
            "{}",
            pose.matrices[0]
        );
    }

    #[test]
    fn a_skinned_primitive_uploads_its_joint_stream() {
        // The key reads joints and the mesh is uploaded with them, so the
        // variant the primitive draws never asks for a stream it does not have.
        let gltf = UnlitGltf::from_buffer(&skinned_document([0.0, 0.0, 0.0]))
            .expect("the skinned document parses");
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let key = gltf.pipeline_key(&device, 0, 0);
        assert!(key.options.flags.contains(UnlitFlags::VERTEX_JOINTS));
        assert!(key.options.flags.contains(UnlitFlags::VERTEX_POSITION));
        assert!(!key.options.flags.contains(UnlitFlags::VERTEX_UV));
    }

    #[test]
    fn a_node_without_a_skin_has_no_pose() {
        let gltf = UnlitGltf::from_buffer(&skinned_document([0.0, 0.0, 0.0]))
            .expect("the skinned document parses");
        assert_eq!(gltf.skin_joint_count(0), Some(1));
        assert_eq!(gltf.skin_joint_count(1), None);
        assert!(gltf.skin_pose(1).matrices.is_empty());
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
