//! What spawning a document's default scene puts in the frame: textures, hierarchy, flat colour, and unloading.

use super::documents::*;
use super::*;

/// A textured quad covers the frame in four colour quarters: the rows of
/// the 4x4 PNG (red, green, blue, white) map one each.
pub(super) async fn textured_quad_renders_four_colours() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: true,
        blend: false,
        mask: None,
        double_sided: false,
    }))
    .expect("the embedded document parses");

    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    let _entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);

    let target = gpu.bind_offscreen_target(&world, "test::gltf_textured_quad");
    gpu.render(&world);
    let px = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    let quarter = (WIDTH * HEIGHT / 4) as usize;
    for (colour, name) in [
        ([255, 0, 0], "red"),
        ([0, 255, 0], "green"),
        ([0, 0, 255], "blue"),
        ([255, 255, 255], "white"),
    ] {
        let count = count_colour(&px, colour, 0);
        // A coarse window around `quarter`: multisampled edges and the quad's
        // exact fit cost each region no more than a few hundred texels.
        assert!(
            count > quarter - 1_024 && count < quarter + 1_024,
            "the {name} quarter covers {count} texels, expected ~{quarter}"
        );
    }
    let off = count_pixels_off_background(&px, CLEAR, 2);
    assert!(
        off > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the quad fills the frame: {off} texels differ from the background"
    );
}

/// The world transform of a node in a hierarchy reaches the spawned entity:
/// a translated parent moves the quad out of the frame's centre, so the
/// rendered frame differs from the untranslated one and leaves background on
/// one side.
pub(super) async fn node_hierarchy_translates_the_quad() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    let mut render = async |translation: [f32; 3]| -> Vec<u8> {
        let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
            translation,
            textured: true,
            blend: false,
            mask: None,
            double_sided: false,
        }))
        .expect("the embedded document parses");
        let resources =
            gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
        let target_handle = gpu.bind_offscreen_target(&world, "test::gltf_translated_quad");
        let entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
        gpu.spawn(&mut world, bundle![camera()]);
        gpu.render(&world);
        let px =
            unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target_handle).await;
        // Drop what the document added and despawn its entities, so the next
        // closure starts from the same empty world.
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

    let centred = render([0.0, 0.0, 0.0]).await;
    let shifted = render([-0.9, 0.0, 0.0]).await;
    assert_ne!(centred, shifted, "a parent translation must move the quad");
    // The frame clears to black by default, so the shifted quad's exposed
    // background is what separates the two counts.
    assert!(
        count_pixels_off_background(&shifted, [0.0, 0.0, 0.0], 2)
            < count_pixels_off_background(&centred, [0.0, 0.0, 0.0], 2),
        "the shifted quad leaves part of the frame on the background"
    );
}

/// A material with no base-color texture inserts as `None` and the quad
/// spawns without a `GpuMaterial`, drawing the flat factor.
pub(super) async fn untextured_material_draws_flat() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    // Same document, but the material samples no texture: the sway to
    // textured/`TEXCOORD_0` is skipped, yet the buffer still carries them.
    let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: false,
        blend: false,
        mask: None,
        double_sided: false,
    }))
    .expect("the embedded document parses");
    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    assert!(
        resources.materials.iter().all(Option::is_none),
        "a textureless material binds nothing"
    );

    let _entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);
    let target = gpu.bind_offscreen_target(&world, "test::gltf_flat_quad");
    gpu.render(&world);
    let px = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    // baseColorFactor [1,1,1,1] on the default white material: everything the
    // quad covers is white, and the frame has no other colour in it.
    let white = count_colour(&px, [255, 255, 255], 1);
    assert!(
        white > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the flat quad draws white: {white} texels"
    );
    assert_eq!(
        count_pixels_off_background(&px, CLEAR, 2) - white,
        0,
        "no texel outside the quad carries a non-background colour"
    );
}

/// Unloading every resource empties the frame again.
pub(super) async fn unload_empties_the_frame() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: true,
        blend: false,
        mask: None,
        double_sided: false,
    }))
    .expect("the embedded document parses");
    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    let entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);

    let target = gpu.bind_offscreen_target(&world, "test::gltf_unload_before");
    gpu.render(&world);
    let before = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;
    assert!(
        count_pixels_off_background(&before, CLEAR, 2) > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the quad draws before unloading"
    );

    for node in entities {
        for entity in node.entities {
            gpu.despawn(&mut world, entity);
        }
    }
    // Giving up the handles is the unload; the next maintain collects.
    drop(resources);
    gpu.maintain(&world);

    let target = gpu.bind_offscreen_target(&world, "test::gltf_unload_after");
    gpu.render(&world);
    let after = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;
    // The frame clears to black with no `RenderLoadOps` in the world, so an
    // empty frame reads back black everywhere.
    let breakdown = [
        ([255, 0, 0], "red"),
        ([0, 255, 0], "green"),
        ([0, 0, 255], "blue"),
        ([255, 255, 255], "white"),
        ([0, 0, 0], "black"),
    ]
    .map(|(colour, name)| format!("{name}={}", count_colour(&after, colour, 2)))
    .join(", ");
    assert_eq!(
        count_pixels_off_background(&after, [0.0, 0.0, 0.0], 2),
        0,
        "nothing draws after the resources are unloaded ({breakdown})",
    );
}
