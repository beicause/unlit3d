//! Uploading a document's images, materials and meshes into a world.
//!
//! A document is parsed eagerly but uploads nothing until one of these calls
//! names a world's [`MeshSource`]. Uploads store their handles in a
//! [`GltfResources`] table and [`LitGltf::insert_asset`] also prepares a model
//! that retains its own draw handles.

use super::image::{encode_mip_chain, image_format};
use super::material::{alpha_cutoff, base_color_uv_set, sampler_descriptor, textured_key};
use super::streams::from_reader;
use super::{
    AnimationId, GltfAsset, GltfImage, GltfMaterial, GltfMesh, GltfResources, ImageId, LitGltf,
    MaterialId, MeshId, PrimitiveId,
};
use crate::lit::LitPipelineKey;
use crate::mesh_source::MeshSource;
use unlit_ecs::Scope;
use unlit_wgpu::pipeline::LitOptions;

impl LitGltf {
    /// Upload the document and prepare its default scene with every animation.
    ///
    /// The returned value owns the uploaded resources and the model that draws
    /// them. A caller may move out the model before spawning it; each draw keeps
    /// its own mesh and material handles alive.
    ///
    /// # Errors
    ///
    /// Returns the document, upload or model error that prevented preparation.
    pub fn insert_asset(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
    ) -> Result<GltfAsset, super::Error> {
        self.insert_asset_with(source, world, self.animations())
    }

    /// Upload the document and prepare its default scene with selected clips.
    ///
    /// Clip indices in the returned model follow the order of `clips`. Every
    /// id is validated before resources are uploaded; a bad id therefore
    /// cannot leave a partially inserted asset in the source.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::AnimationOutsideDocument`] for an id the
    /// document does not carry, or the document, upload or model error that
    /// prevented preparation.
    pub fn insert_asset_with(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        clips: impl IntoIterator<Item = AnimationId>,
    ) -> Result<GltfAsset, super::Error> {
        let clips: Vec<_> = clips.into_iter().collect();
        for &clip in &clips {
            self.animation_name(clip)?;
        }
        let resources = self.insert_resources(source, world)?;
        let model = self.to_model_with(&resources, clips)?;
        Ok(GltfAsset { model, resources })
    }

    /// The [`LitPipelineKey`] the primitive renders with.
    ///
    /// A material carrying `KHR_materials_unlit` shades nothing and draws its
    /// base colour as the file states it; every other material is lit and
    /// shades under the frame's lights. The rest of the key is what a caller
    /// picks: the texture filtering the material's base-color texture needs,
    /// whether a `BLEND` material blends without writing depth and whether a
    /// `MASK` one cuts. Everything geometric —
    /// which vertex channels the primitive carries, whether it samples its
    /// base-color texture, whether it skins or morphs — is a property of the
    /// draw, so [`Self::insert_mesh`] uploads the streams the primitive's own
    /// attributes declare and the variant a draw resolves derives the rest
    /// from the mesh it draws. A `doubleSided` material turns back-face
    /// culling off, so its geometry is drawn from either side; everything
    /// else follows [`LitOptions::standard`].
    ///
    /// # Errors
    ///
    /// If the primitive does not exist or samples a base-color image missing
    /// from `resources`. The filtering that decides the bind-group layout is
    /// read off that uploaded handle rather than recomputed from the document,
    /// so a key and the group a material builds always agree on one image.
    pub fn pipeline_key(
        &self,
        resources: &GltfResources,
        mesh: MeshId,
        primitive: PrimitiveId,
    ) -> Result<LitPipelineKey, super::Error> {
        let primitive = self
            .primitive(mesh, primitive)
            .ok_or(super::Error::MeshOutsideDocument { mesh, primitive })?;
        let material = primitive.material();
        // A material carrying `KHR_materials_unlit` is drawn unlit, its base
        // colour exactly as the file states it; every other material shades
        // under the frame's lights, which is the glTF default a document opts
        // out of.
        let mut options = LitOptions {
            unlit: material.unlit(),
            ..LitOptions::standard()
        };
        // The texture the material samples decides whether the base-color
        // binding is a filtering one: the pipeline and the material bind group
        // have to agree, or the group does not fit the pipeline.
        let texture = material.pbr_metallic_roughness().base_color_texture();
        if base_color_uv_set(&primitive).is_some()
            && let Some(info) = &texture
            && let Some(material_index) = material.index()
        {
            let image = super::ImageId::new(info.texture().source().index());
            let filtering = resources
                .image(image)
                .ok_or(super::Error::ImageNotInserted {
                    material: super::MaterialId::new(material_index),
                    image,
                })?
                .filtering;
            options.texture_filtering = filtering;
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
        // material group — see [`LitModelBundle`](crate::lit::LitModelBundle).
        if alpha_cutoff(&material).is_some() {
            options.alpha_cutoff = true;
        }
        // A `doubleSided` material is visible from behind, so its back faces
        // are rasterized rather than culled. Nothing else about the variant
        // changes: a back face takes the geometric normal turned towards the
        // eye, which a normal channel or the screen-space fallback supplies,
        // so both sides shade by the same equation.
        if material.double_sided() {
            options.primitive.cull_mode = None;
        }
        // The mode the primitive states decides how the rasterizer assembles
        // its vertices; a mode the device has no topology for is drawn as the
        // list its indices were expanded into, which `from_reader` did at
        // upload. Two modes that expand to the same list therefore share a
        // pipeline, which is the point of expanding rather than of drawing
        // them as something else.
        options.primitive.topology = super::topology::topology(primitive.mode());
        Ok(LitPipelineKey::new(options))
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
        resources: &mut GltfResources,
    ) -> Result<GltfImage, super::Error> {
        let data = self
            .images
            .get(image.index())
            .ok_or(super::Error::ImageOutsideDocument { image })?;
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
        let uploaded = GltfImage {
            image,
            texture,
            view,
            filtering: format.filtering,
        };
        resources.insert_image(uploaded.clone());
        Ok(uploaded)
    }

    /// Upload every image of the document.
    ///
    /// Each handle is written into `resources` at the position its id names,
    /// so a piecewise upload and a whole-document one leave the resources in
    /// the same state.
    ///
    /// # Errors
    ///
    /// If a document image cannot be uploaded; see [`Self::insert_image`].
    pub fn insert_images(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        resources: &mut GltfResources,
    ) -> Result<Vec<GltfImage>, super::Error> {
        let images = self.images().collect::<Vec<_>>();
        images
            .into_iter()
            .map(|image| self.insert_image(source, world, image, resources))
            .collect()
    }

    /// Build the material bind group for `material` and write it into
    /// `resources`.
    ///
    /// Returns `None` when the material declares no base-color texture: such a
    /// material has nothing to bind and spawns with no `GpuMaterial`. The
    /// resources still carry an entry for it, which is what tells a primitive
    /// that samples a texture the material does not have
    /// ([`Error::MaterialWithoutTexture`](super::Error::MaterialWithoutTexture))
    /// apart from one whose material was never inserted.
    ///
    /// # Errors
    ///
    /// If the material's base-color texture names an image that is not in
    /// `resources`. Inserting a document piecewise means inserting its images
    /// before the materials that sample them.
    pub fn insert_material(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        material: MaterialId,
        resources: &mut GltfResources,
    ) -> Result<Option<GltfMaterial>, super::Error> {
        let entry = self.build_material(source, world, material, resources)?;
        resources.insert_material(material, entry.clone());
        Ok(entry)
    }

    /// Build the material bind group, reusing the samplers the resources
    /// already hold.
    ///
    /// The cache lives in [`GltfResources`], so a caller inserting materials
    /// one at a time reuses a sampler an earlier call uploaded rather than
    /// creating a second one the graph would keep alive beside it.
    fn build_material(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        material: MaterialId,
        resources: &mut GltfResources,
    ) -> Result<Option<GltfMaterial>, super::Error> {
        let data = self
            .material(material)
            .ok_or(super::Error::MaterialOutsideDocument { material })?;
        let Some(info) = data.pbr_metallic_roughness().base_color_texture() else {
            return Ok(None);
        };
        // The image a texture names is the caller's to insert: a document
        // inserted piecewise inserts its images first, so one that is missing
        // here is a call made in the wrong order rather than a document that
        // says something it cannot mean.
        let image = ImageId::new(info.texture().source().index());
        let filtering = resources
            .image(image)
            .ok_or(super::Error::ImageNotInserted { material, image })?
            .filtering;

        // A material group exists only for a variant that samples a base-color
        // texture; build the bind group against exactly that variant. Its
        // layout is the filtering one only when the image can be filtered, and
        // the sampler has to be non-filtering in step with it.
        let key = textured_key(filtering);
        let descriptor = sampler_descriptor(&info.texture().sampler());
        let descriptor = if filtering {
            descriptor
        } else {
            wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..descriptor
            }
        };
        let sampler = resources.sampler(source, world, descriptor);
        let view = resources
            .image(image)
            .ok_or(super::Error::ImageNotInserted { material, image })?
            .view
            .clone();

        let bind_group =
            source.allocate_lit_material(world, &key.options, view.clone(), sampler.clone());
        Ok(Some(GltfMaterial {
            material,
            bind_group,
            view,
            sampler,
        }))
    }

    /// Build the bind group of every material of the document.
    ///
    /// The resources carry an entry per document material, indexed by its own
    /// id, and the returned vector is aligned with them.
    ///
    /// # Errors
    ///
    /// If a material's base-color texture names an image that is not in
    /// `resources`.
    pub fn insert_materials(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        resources: &mut GltfResources,
    ) -> Result<Vec<Option<GltfMaterial>>, super::Error> {
        let materials = self.materials().collect::<Vec<_>>();
        let mut inserted = Vec::with_capacity(materials.len());
        for material in materials {
            inserted.push(self.insert_material(source, world, material, resources)?);
        }
        Ok(inserted)
    }

    /// Upload the primitive into `source`'s resource graph and write its handle
    /// into `resources`.
    ///
    /// Which streams are uploaded follows the primitive's own attributes:
    /// the UV set its material samples a base-color texture through enables the
    /// UV stream, `COLOR_0` the vertex-color stream, `JOINTS_0` and
    /// `WEIGHTS_0` the joint stream, and a `targets` array the displacement
    /// stream — one slot per declared target, with a target that states no
    /// positions contributing zeros rather than no slot. A primitive read with
    /// streams its attributes do not declare would panic, so the derivation is
    /// the only safe choice here. Normals are uploaded; tangents are ignored
    /// (see the module docs).
    ///
    /// A mode the device has no topology for is expanded into the list that
    /// draws the same primitives, so its index stream is not the document's.
    ///
    /// The morph-target count the displacement stream is packed with is the
    /// one [`mesh_morph_targets`](super::scene::mesh_morph_targets) validated
    /// for the whole mesh, so every primitive of a mesh uploads the same
    /// slots.
    ///
    /// # Errors
    ///
    /// If the document carries no such primitive, the primitive carries no
    /// `POSITION` attribute, states fewer vertices than its topology needs,
    /// indexes a vertex it does not carry, or samples a base-color texture
    /// whose image is not in `resources`.
    pub fn insert_mesh(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        resources: &mut GltfResources,
        mesh: MeshId,
        primitive: PrimitiveId,
    ) -> Result<GltfMesh, super::Error> {
        let key = self.pipeline_key(resources, mesh, primitive)?;
        let data = self
            .primitive(mesh, primitive)
            .ok_or(super::Error::MeshOutsideDocument { mesh, primitive })?;
        let targets = self
            .mesh_targets
            .get(mesh.index())
            .copied()
            .ok_or(super::Error::MeshOutsideDocument { mesh, primitive })?;
        let mut streams = from_reader(&self.buffers, mesh, primitive, &data, targets)?;
        let gpu_mesh = source.allocate_lit_mesh(world, streams.desc());

        let uploaded = GltfMesh {
            mesh_index: mesh,
            primitive_index: primitive,
            key,
            mesh: gpu_mesh,
        };
        resources.insert_mesh(uploaded.clone());
        Ok(uploaded)
    }

    /// Upload every primitive of every mesh of the document.
    ///
    /// # Errors
    ///
    /// If a primitive of the document cannot be uploaded; see
    /// [`Self::insert_mesh`].
    pub fn insert_meshes(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
        resources: &mut GltfResources,
    ) -> Result<Vec<GltfMesh>, super::Error> {
        let meshes = self.meshes().collect::<Vec<_>>();
        let mut uploaded = Vec::new();
        for mesh in meshes {
            let primitives = self
                .primitives(mesh)
                .ok_or(super::Error::MeshOutsideDocument {
                    mesh,
                    primitive: PrimitiveId::new(0),
                })?
                .collect::<Vec<_>>();
            for primitive in primitives {
                uploaded.push(self.insert_mesh(source, world, resources, mesh, primitive)?);
            }
        }
        Ok(uploaded)
    }

    /// Insert the whole document and return every handle that keeps it alive.
    ///
    /// This is the one call that uploads a document: it inserts the images,
    /// builds the materials from them and uploads every primitive, and hands
    /// back a [`GltfResources`] holding all of it. Pass that to
    /// [`Self::to_model`] to turn the default scene into model data; keep it
    /// alive for as long as the entities that draw it live.
    ///
    /// A caller that inserts the pieces itself — to share a texture between
    /// documents, say — can call [`Self::insert_images`],
    /// [`Self::insert_materials`] and [`Self::insert_meshes`] on a
    /// [`GltfResources::new`] instead. The meshes read their texture filtering
    /// off the image handles, so they are inserted after the images they
    /// sample.
    ///
    /// # Errors
    ///
    /// If a material's base-color texture names an image the document does
    /// not declare, which the images just inserted cannot contain, or if a
    /// primitive cannot be uploaded; see [`Self::insert_mesh`].
    pub fn insert_resources(
        &self,
        source: &mut MeshSource,
        world: &Scope<'_>,
    ) -> Result<GltfResources, super::Error> {
        let mut resources = GltfResources::new();
        self.insert_images(source, world, &mut resources)?;
        self.insert_materials(source, world, &mut resources)?;
        self.insert_meshes(source, world, &mut resources)?;
        Ok(resources)
    }
}
