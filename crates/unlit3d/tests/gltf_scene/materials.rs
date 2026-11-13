//! How a material's alpha mode and sidedness reach the frame: blending, depth writes, masking and back faces.

use super::documents::*;
use super::*;

/// A `doubleSided` material draws the geometry's back faces too: a quad wound
/// away from the camera is culled when single-sided and covered when not.
///
/// Nothing in the unlit fragment shader reads a normal, so double-sidedness is
/// the cull mode alone — the back faces are rasterized as they are rather than
/// with a reversed normal.
pub(super) async fn a_double_sided_material_draws_back_faces() {
    let ctx = Ctx::headless().await;

    let render = |double_sided: bool| {
        let ctx = &ctx;
        async move {
            let mut world = World::new();
            let gpu = TestGpu::new(&mut world, ctx);
            let gltf = LitGltf::from_bytes(&reversed_document(double_sided))
                .expect("the embedded document parses");

            let key = gltf.pipeline_key(&ctx.device, MeshId::new(0), PrimitiveId::new(0));
            if double_sided {
                assert_eq!(
                    key.options.primitive.cull_mode, None,
                    "a double-sided material culls nothing"
                );
            } else {
                assert_eq!(
                    key.options.primitive.cull_mode,
                    Some(wgpu::Face::Back),
                    "a single-sided material culls back faces"
                );
            }

            let resources =
                gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
            spawn_scene(&gpu, &mut world, &gltf, &resources);
            gpu.spawn(&mut world, bundle![camera()]);

            let target = gpu.bind_offscreen_target(&world, "test::gltf_double_sided");
            gpu.render(&world);
            unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await
        }
    };

    let single = render(false).await;
    let double = render(true).await;

    // The single-sided pass culled every triangle, so the frame is untouched.
    assert_eq!(
        count_colour(&single, [255, 0, 0], 4),
        0,
        "a quad wound away from the camera is culled when single-sided"
    );
    // Turning the culling off lets the same geometry cover the frame.
    let red = count_colour(&double, [255, 0, 0], 4);
    assert!(
        red > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the same quad covers the frame once culling is off: {red} red texels"
    );
}

/// A material with `alphaMode: BLEND` turns on blending and z-sorting rather
/// than writing opaque geometry: the quad composites over the background
/// instead of covering it with white.
pub(super) async fn a_blended_material_composites_over_the_frame() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: false,
        blend: true,
        mask: None,
        double_sided: false,
    }))
    .expect("the embedded document parses");

    // The primitive's key carries the blend state...
    let key = gpu.with_mesh_source(&world, |source, world| {
        gltf.pipeline_key(&source.device(world), MeshId::new(0), PrimitiveId::new(0))
    });
    assert!(
        key.options.blend.is_some(),
        "an alphaMode BLEND material blends"
    );

    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    let entities = spawn_scene(&gpu, &mut world, &gltf, &resources);

    // ...and the spawned entity is marked for back-to-front compositing.
    let z_sorted: Vec<Entity> = world
        .root_scope()
        .query::<(&ZSortedDrawing, Entity)>()
        .iter()
        .map(|(_zsorted_drawing, entity)| entity)
        .collect();
    let spawned: Vec<Entity> = entities
        .iter()
        .flat_map(|node| node.entities.iter().copied())
        .collect();
    assert_eq!(
        z_sorted, spawned,
        "a blended primitive spawns with ZSortedDrawing"
    );

    gpu.spawn(&mut world, bundle![camera()]);
    let target = gpu.bind_offscreen_target(&world, "test::gltf_blended_quad");
    gpu.render(&world);
    let px = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    let quarter = (WIDTH * HEIGHT / 4) as usize;
    // Half-opaque white over the background: almost no texel reaches fully
    // opaque white, which is exactly what the unblended draw would produce.
    let white = count_colour(&px, [255, 255, 255], 1);
    assert!(
        white < quarter / 10,
        "a blended quad composites rather than writing opaque white ({white} texels)"
    );
    let covered = count_pixels_off_background(&px, CLEAR, 2);
    assert!(
        covered > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the blended quad still covers the frame: {covered} texels"
    );
}

/// Two coplanar blended quads composite over each other only if the first one
/// leaves no depth behind: a second fragment at the same depth fails a
/// `Greater` comparison, so a writing pipeline shows one quad where the
/// surface should show both layers.
pub(super) async fn a_blended_material_writes_no_depth() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: false,
        blend: true,
        mask: None,
        double_sided: false,
    }))
    .expect("the embedded document parses");

    // The key turns blending on and depth writes off together: a blended
    // fragment covers its surface only partly, so the depth it would write is
    // no measurement of where that surface is.
    let key = gpu.with_mesh_source(&world, |source, world| {
        gltf.pipeline_key(&source.device(world), MeshId::new(0), PrimitiveId::new(0))
    });
    assert!(
        key.options.blend.is_some(),
        "an alphaMode BLEND material blends"
    );
    assert!(
        !key.options.depth_stencil.depth_write_enabled,
        "a blended material writes no depth"
    );

    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);
    let single = gpu.bind_offscreen_target(&world, "test::gltf_blend_single");
    gpu.render(&world);
    let one = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &single).await;

    // A second draw of the same surface lands on exactly the depth the first
    // one left, so it either composites or is discarded there.
    spawn_scene(&gpu, &mut world, &gltf, &resources);
    let doubled = gpu.bind_offscreen_target(&world, "test::gltf_blend_doubled");
    gpu.render(&world);
    let two = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &doubled).await;

    // The quad fills the frame, so one texel of it says what the whole surface
    // does: half-opaque white over black once, three quarters of it twice.
    let centre = |px: &[u8]| px.as_chunks::<4>().0[((HEIGHT / 2) * WIDTH + WIDTH / 2) as usize];
    let (one, two) = (centre(&one), centre(&two));
    assert!(
        two[0] > one[0] + 16,
        "the second blended quad composites over the first: {one:?} then {two:?}"
    );
}

/// A material with `alphaMode: MASK` draws binary coverage: every fragment
/// whose alpha falls below the cutoff is discarded, so the quad disappears
/// whole when the cutoff clears its alpha and covers the frame when it does
/// not. Nothing is composited either way, so the quad needs no z-sort.
pub(super) async fn a_masked_material_discards_fragments_below_its_cutoff() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    let mut render_with_cutoff = async |cutoff: f32| -> Vec<u8> {
        let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
            translation: [0.0, 0.0, 0.0],
            textured: true,
            blend: false,
            mask: Some(cutoff),
            double_sided: false,
        }))
        .expect("the embedded document parses");

        // The cutoff is per-instance state the spawned entity carries, not a
        // uniform the material binds: the key only declares that fragments are
        // cut off, and `spawn_node` writes each instance's own value.
        let key = gpu.with_mesh_source(&world, |source, world| {
            gltf.pipeline_key(&source.device(world), MeshId::new(0), PrimitiveId::new(0))
        });
        assert!(
            key.options.alpha_cutoff,
            "an alphaMode MASK material cuts its fragments off"
        );
        assert!(
            key.options.blend.is_none(),
            "a cut-off material does not blend"
        );

        let resources =
            gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
        let entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
        assert!(
            world
                .root_scope()
                .query::<&ZSortedDrawing>()
                .iter()
                .next()
                .is_none(),
            "a cut-off material draws opaque coverage, so it needs no sort"
        );

        let target_handle = gpu.bind_offscreen_target(&world, "test::gltf_masked_quad");
        gpu.spawn(&mut world, bundle![camera()]);
        gpu.render(&world);
        let px =
            unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target_handle).await;
        for node in entities {
            for entity in node.entities {
                gpu.despawn(&mut world, entity);
            }
        }
        // Giving up the handles is the unload; the next maintain collects.
        drop(resources);
        gpu.maintain(&world);
        px
    };

    // The material's factor is half opaque, so a cutoff above it discards the
    // whole quad and one below it keeps every fragment.
    let drawn = render_with_cutoff(0.25).await;
    let quarter = (WIDTH * HEIGHT / 4) as usize;
    let red = count_colour(&drawn, [255, 0, 0], 0);
    assert!(
        red > quarter - 1_024 && red < quarter + 1_024,
        "a cutoff below the quad's alpha keeps the red quarter: {red} texels"
    );
    let discarded = render_with_cutoff(0.75).await;
    // The frame clears to black with no `RenderLoadOps` in the world, so a
    // quad that drew nothing leaves the whole frame on the background.
    assert_eq!(
        count_pixels_off_background(&discarded, [0.0, 0.0, 0.0], 2),
        0,
        "a cutoff above the quad's alpha discards every fragment"
    );
}
