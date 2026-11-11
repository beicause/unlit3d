use super::*;

impl EguiIntegration {
    /// The material bind group for (`id`, `options`), if it exists.
    pub(super) fn material<'a>(
        &self,
        graph: &'a ResourceGraph,
        texture: TextureKey,
        options: egui::TextureOptions,
    ) -> Option<&'a wgpu::BindGroup> {
        let key = MaterialKey { texture, options };
        let index = self.materials.iter().position(|(seen, _)| *seen == key)?;
        graph.get(&self.materials[index].1)
    }

    /// Build the material bind group for (`id`, `options`) if it does not
    /// exist, registered in the graph as a dependent of its texture's view
    /// and its sampler.
    ///
    /// The group is built once here and rebuilt by
    /// [`ResourceGraph::maintain`] whenever the view or the sampler is
    /// replaced: the recipe reads both back out of the graph by id, so a
    /// texture which patches itself into a new handle is picked up without
    /// the material being rebuilt by hand.
    pub(super) fn build_material(
        &mut self,
        graph: &mut ResourceGraph,
        texture: TextureKey,
        options: egui::TextureOptions,
    ) {
        let key = MaterialKey { texture, options };
        if self.materials.iter().any(|(seen, _)| *seen == key) {
            return;
        }
        let Some(slot) = self.textures.get(&texture).cloned() else {
            return;
        };
        let sampler_id = self.sampler(graph, options);
        let layout = self
            .pipeline
            .descriptor()
            .bind_group_layouts(&self.device)
            .material
            .expect("the UI variant always samples a base-color texture");
        let device = self.device.clone();
        let view_id = slot.view.clone();
        let sampler_for_build = sampler_id.clone();
        let build = move |graph: &ResourceGraph| {
            let view = graph
                .get(&view_id)
                .expect("the texture view is in the graph")
                .view()
                .clone();
            let sampler = graph
                .get(&sampler_for_build)
                .expect("the sampler is in the graph")
                .clone();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ui::material"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: BASE_COLOR_TEXTURE_BINDING,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: BASE_COLOR_SAMPLER_BINDING,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let group = build(graph);
        let rebuild = Rebuild::new(move |graph| Resource::BindGroup(build(graph)));
        let id = graph.insert(group, Some(rebuild));
        graph.add_dependency(&id, &slot.view);
        graph.add_dependency(&id, &sampler_id);
        self.materials.push((key, id));
    }

    /// The sampler for `options`, created and registered on first use.
    fn sampler(
        &mut self,
        graph: &mut ResourceGraph,
        options: egui::TextureOptions,
    ) -> ResHandle<wgpu::Sampler> {
        if let Some((_, id)) = self.samplers.iter().find(|(seen, _)| *seen == options) {
            return id.clone();
        }
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui::sampler"),
            address_mode_u: address_mode(options.wrap_mode),
            address_mode_v: address_mode(options.wrap_mode),
            mag_filter: texture_filter(options.magnification),
            min_filter: texture_filter(options.minification),
            ..Default::default()
        });
        let id = graph.insert(sampler, None);
        self.samplers.push((options, id.clone()));
        id
    }

    pub(super) fn apply_textures(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        source: Source,
        delta: &egui::TexturesDelta,
    ) {
        for (&id, deltas) in &delta.set {
            let key = TextureKey { source, id };
            for image in deltas {
                // Every delta for a texture states its sampling options, and
                // the last one wins: they describe the texture as a whole, not
                // the patch being written.
                self.texture_options.insert(key, image.options);
                self.upload_texture(graph, queue, key, image);
            }
        }
        for &id in &delta.free {
            // Dropping the slot drops the ids to the texture and its view, so
            // the next [`ResourceGraph::maintain`] collects them — and the
            // materials that were built from the view, since the view's
            // reference count reaches zero with the slot's. The materials'
            // own entries go here, before that pass, so egui reusing the
            // (texture, options) pair rebuilds a material rather than finding
            // a stale one.
            //
            // The key carries the source: two contexts both free id zero at
            // some point, and the other context's texture is not this one's to
            // drop.
            let key = TextureKey { source, id };
            self.textures.remove(&key);
            self.texture_options.remove(&key);
            self.materials.retain(|(seen, _)| seen.texture != key);
        }
    }

    fn upload_texture(
        &mut self,
        graph: &mut ResourceGraph,
        queue: &wgpu::Queue,
        key: TextureKey,
        delta: &egui::epaint::ImageDelta,
    ) {
        let [width, height] = delta.image.size();
        // The image's own bytes, borrowed rather than copied into a fresh
        // `Vec`: a font atlas can be megabytes, and this is the whole of it.
        let pixels: &[u8] = match &delta.image {
            egui::ImageData::Color(image) => image.as_raw(),
        };
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width as u32 * texture_bytes()),
            rows_per_image: Some(height as u32),
        };
        let size = wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        };

        match delta.pos {
            // A patch written into the texture already there.
            Some([x, y]) => {
                let Some(slot) = self.textures.get(&key) else {
                    return;
                };
                let Some(texture) = graph.get(&slot.texture) else {
                    return;
                };
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: x as u32,
                            y: y as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    pixels,
                    layout,
                    size,
                );
            }
            None => {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("ui::texture"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: TEXTURE_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(texture.as_image_copy(), pixels, layout, size);
                let view =
                    TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
                let texture_id = graph.insert(texture, None);
                let view_id = graph.insert(view, None);
                graph.add_dependency(&view_id, &texture_id);
                self.textures.insert(
                    key,
                    TextureSlot {
                        texture: texture_id,
                        view: view_id,
                    },
                );
            }
        }
    }
}
