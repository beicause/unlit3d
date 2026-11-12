//! Uploading a document's images, materials and meshes into a world.
//!
//! A document is parsed eagerly but uploads nothing until one of these calls
//! names a world's [`MeshSource`]: each handle they return is what keeps the
//! GPU resource alive, so a caller holds them for as long as the entities that
//! draw them live. [`LitGltf::insert_resources`] does the whole document in one
//! call; the per-kind calls are for a caller that shares textures or materials
//! between documents.

use super::image::{encode_mip_chain, image_format};
use super::material::{alpha_cutoff, sampler_descriptor, samples_base_color_texture, textured_key};
use super::streams::from_reader;
use super::{
    GltfImage, GltfMaterial, GltfMesh, GltfResources, ImageId, LitGltf, MaterialId, MeshId,
    PrimitiveId,
};
use crate::lit::LitPipelineKey;
use crate::mesh_source::MeshSource;
use unlit_ecs::Scope;
use unlit_wgpu::pipeline::LitOptions;
use unlit_wgpu::resources::TextureExt;

impl LitGltf {
    /// The [`LitPipelineKey`] the primitive renders with.
    ///
    /// The key carries only what a caller picks: the texture filtering the
    /// material's base-color texture needs, whether a `BLEND` material blends
    /// without writing depth and whether a `MASK` one cuts. Everything geometric —
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

    /// Upload `image` into `source`'s resource graph and return its handle.
    ///
    /// The texture holds the image's decoded pixels widened into the
    /// four-channel format of their bit depth — see the
    /// [module docs](super#texture-formats) — and carries as much of a mip
    /// chain as the document's [`MipChain`](crate::gltf::MipChain) asks for. Its
    /// sampler is not
    /// uploaded here, because that belongs to the material that samples it.
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
        let mips = encode_mip_chain(data, format.format, self.mips);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Every level's rows are padded to the 256-byte stride a
        // buffer-to-texture copy asks for; the padding bytes stay zero.
        for (level, mip) in mips.iter().enumerate() {
            source.queue(world).write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.rows,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip.bytes_per_row),
                    rows_per_image: Some(mip.height),
                },
                wgpu::Extent3d {
                    width: mip.width,
                    height: mip.height,
                    depth_or_array_layers: 1,
                },
            );
        }

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

    /// Upload the primitive into `source`'s resource graph and return its
    /// handle.
    ///
    /// Which streams are uploaded follows the primitive's own attributes:
    /// `TEXCOORD_0` with a base-color-textured material enables the UV stream,
    /// `COLOR_0` the vertex-color stream, `JOINTS_0` and `WEIGHTS_0` the joint
    /// stream, and a position-displacing morph target the displacement stream.
    /// A primitive read with streams its attributes do not declare would
    /// panic, so the derivation is the only safe choice here. Normals are
    /// uploaded; tangents are ignored (see the module docs).
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
        let mut streams = from_reader(&self.buffers, &primitive);
        let gpu_mesh = source.allocate_lit_mesh(world, &key.options, streams.desc());

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

    /// Whether image `image` uploads as a texture a filtering sampler can
    /// sample — the same answer [`Self::insert_image`] bakes into the handle.
    fn image_filtering(&self, image: usize, device: &wgpu::Device) -> bool {
        image_format(&self.images[image], device).filtering
    }
}
