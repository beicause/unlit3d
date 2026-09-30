//! GPU coverage for the `gltf` feature: uploading a document's textures,
//! materials and meshes into an existing mesh source's graph, spawning the
//! default scene's nodes, rendering what those entities draw, and unloading
//! it all again.
//!
//! The documents are built in-memory as glTF JSON with a single data-URI
//! buffer holding the vertices, indices and a 4x4 PNG (rows red, green, blue,
//! white) as a buffer view, so `UnlitGltf::from_buffer` needs no base path.

pub mod common;

use common::*;
use unlit_wgpu_test_util::{gpu_test_main, gpu_tests};
use unlit3d::gltf::UnlitGltf;
use unlit3d::prelude::*;

/// The buffer: 4 positions (f32), 4 uvs (f32), 6 indices (u16), the PNG.
/// `quad_document` puts it in the doc as a data URI.
const BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAAAAAAAgD8AAIA/AACAPwAAgD8AAAAAAAAAAAAAAAAAAAEAAgAAAAIAAwCJUE5HDQoaCgAAAA1JSERSAAAABAAAAAQIBgAAAKnxnn4AAAAXSURBVHicY/jPwPAfGYMQKkRTABRCAwB0CCfZGxzjzQAAAABJRU5ErkJggg==";
const POS_OFFSET: u32 = 0; // 4 × Vec3 f32
const UV_OFFSET: u32 = 48; // 4 × Vec2 f32
const IDX_OFFSET: u32 = 80; // 6 × u16
const IMG_OFFSET: u32 = 92; // the 80-byte PNG
const BIN_LENGTH: u32 = 172;

/// How a document is shaped: where the mesh node sits, whether the
/// material samples a texture, and whether it blends.
struct QuadDoc {
    /// The mesh node's local translation.
    translation: [f32; 3],
    /// `true`: the material samples the 4x4 PNG through `TEXCOORD_0`.
    /// `false`: a flat base-color factor, no texture and no uvs.
    textured: bool,
    /// `true`: the material states `alphaMode: BLEND` and a half-opaque
    /// base-color factor.
    blend: bool,
}

/// The glTF JSON for one quad, as bytes for [`UnlitGltf::from_buffer`].
///
/// Node 0 is a parent of the meshed node 1, so spawning the default scene
/// takes the world transform through a hierarchy. The quad is a unit square
/// in the XY plane, CCW when seen from +Z.
fn quad_document(doc: &QuadDoc) -> Vec<u8> {
    let mut attributes = "\"POSITION\":0".to_string();
    let alpha = if doc.blend { 0.5 } else { 1.0 };
    let mut material =
        format!("\"pbrMetallicRoughness\":{{\"baseColorFactor\":[1.0,1.0,1.0,{alpha}]");
    let mut textures = String::new();
    if doc.textured {
        attributes.push_str(",\"TEXCOORD_0\":1");
        material.push_str(",\"baseColorTexture\":{\"index\":0}");
        textures.push_str(
            ",\"textures\":[{\"source\":0,\"sampler\":0}],\
             \"samplers\":[{\"magFilter\":9728,\"minFilter\":9728,\"wrapS\":10497,\"wrapT\":10497}],\
             \"images\":[{\"bufferView\":3,\"mimeType\":\"image/png\"}]",
        );
    }
    material.push('}');
    if doc.blend {
        material.push_str(",\"alphaMode\":\"BLEND\"");
    }
    let json = format!(
        concat!(
            "{{\"asset\":{{\"version\":\"2.0\"}},\"scene\":0,",
            "\"scenes\":[{{\"nodes\":[0]}}],",
            "\"nodes\":[{{\"children\":[1]}},{{\"mesh\":0,\"translation\":[{},{},{}]}}],",
            "\"meshes\":[{{\"primitives\":[{{\"attributes\":{{{}}},",
            "\"indices\":2,\"material\":0}}]}}],",
            "\"materials\":[{{{}}}]{},",
            "\"accessors\":[",
            "{{\"bufferView\":0,\"componentType\":5126,\"count\":4,\"type\":\"VEC3\",",
            "\"min\":[-1.8,-1.35,0.0],\"max\":[1.8,1.35,0.0]}},",
            "{{\"bufferView\":1,\"componentType\":5126,\"count\":4,\"type\":\"VEC2\"}},",
            "{{\"bufferView\":2,\"componentType\":5123,\"count\":6,\"type\":\"SCALAR\"}}],",
            "\"bufferViews\":[",
            "{{\"buffer\":0,\"byteOffset\":{},\"byteLength\":48,\"target\":34962}},",
            "{{\"buffer\":0,\"byteOffset\":{},\"byteLength\":32,\"target\":34962}},",
            "{{\"buffer\":0,\"byteOffset\":{},\"byteLength\":12,\"target\":34963}},",
            "{{\"buffer\":0,\"byteOffset\":{},\"byteLength\":80}}],",
            "\"buffers\":[{{\"byteLength\":{},\"uri\":\"data:application/octet-stream;base64,{}\"}}]}}"
        ),
        doc.translation[0],
        doc.translation[1],
        doc.translation[2],
        attributes,
        material,
        textures,
        POS_OFFSET,
        UV_OFFSET,
        IDX_OFFSET,
        IMG_OFFSET,
        BIN_LENGTH,
        BIN_BASE64,
    );
    json.into_bytes()
}

/// Count texels whose three colour channels are all within `tolerance` of
/// `colour`.
fn count_colour(px: &[u8], colour: [u8; 3], tolerance: u8) -> usize {
    px.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| {
            p[..3]
                .iter()
                .zip(colour)
                .all(|(&channel, expected)| channel.abs_diff(expected) <= tolerance)
        })
        .count()
}

/// The camera that frames the quad exactly: the quad spans ±1.8 in X and
/// ±1.35 in Y at z = 0, and this eye puts those extents on the viewport edge.
fn camera() -> Camera {
    camera_looking_at(
        glam::Vec3::new(0.0, 0.0, 2.34),
        glam::Vec3::ZERO,
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// A textured quad covers the frame in four colour quarters: the rows of
/// the 4x4 PNG (red, green, blue, white) map one each.
async fn textured_quad_renders_four_colours() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = UnlitGltf::from_buffer(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: true,
        blend: false,
    }))
    .expect("the embedded document parses");

    let images = gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
    let materials = gpu.with_mesh_source(&world, |source, world| {
        gltf.insert_materials(source, world, &images)
    });
    let meshes = gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
    let _entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
    world.spawn((camera(),));

    let target = gpu.bind_offscreen_target(&world, "test::gltf_textured_quad");
    gpu.render(&world);
    let px = read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target));

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
async fn node_hierarchy_translates_the_quad() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    let mut render = |translation: [f32; 3]| -> Vec<u8> {
        let gltf = UnlitGltf::from_buffer(&quad_document(&QuadDoc {
            translation,
            textured: true,
            blend: false,
        }))
        .expect("the embedded document parses");
        let images =
            gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
        let materials = gpu.with_mesh_source(&world, |source, world| {
            gltf.insert_materials(source, world, &images)
        });
        let meshes =
            gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
        let target_handle = gpu.bind_offscreen_target(&world, "test::gltf_translated_quad");
        let entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
        world.spawn((camera(),));
        gpu.render(&world);
        let px = read_texture_bytes(
            &ctx,
            &target_handle,
            WIDTH,
            HEIGHT,
            texel_bytes(&target_handle),
        );
        // Drop what the document added and despawn its entities, so the next
        // closure starts from the same empty world.
        for entity in entities {
            world.despawn(entity);
        }
        gpu.with_mesh_source(&world, |source, world| {
            gltf.unload_materials(source, world, &materials);
            gltf.unload_meshes(source, world, &meshes);
            gltf.unload_images(source, world, &images);
        });
        gpu.maintain(&world);
        px
    };

    let centred = render([0.0, 0.0, 0.0]);
    let shifted = render([-0.9, 0.0, 0.0]);
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
async fn untextured_material_draws_flat() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    // Same document, but the material samples no texture: the sway to
    // textured/`TEXCOORD_0` is skipped, yet the buffer still carries them.
    let gltf = UnlitGltf::from_buffer(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: false,
        blend: false,
    }))
    .expect("the embedded document parses");
    let images = gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
    let materials = gpu.with_mesh_source(&world, |source, world| {
        gltf.insert_materials(source, world, &images)
    });
    let meshes = gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
    assert!(
        materials.iter().all(Option::is_none),
        "a textureless material binds nothing"
    );

    let _entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
    world.spawn((camera(),));
    let target = gpu.bind_offscreen_target(&world, "test::gltf_flat_quad");
    gpu.render(&world);
    let px = read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target));

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

/// A material with `alphaMode: BLEND` turns on blending and z-sorting rather
/// than writing opaque geometry: the quad composites over the background
/// instead of covering it with white.
async fn a_blended_material_composites_over_the_frame() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = UnlitGltf::from_buffer(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: false,
        blend: true,
    }))
    .expect("the embedded document parses");

    // The primitive's key carries the blend state...
    let key = gpu.with_mesh_source(&world, |source, world| {
        gltf.pipeline_key(&source.device(world), 0, 0)
    });
    assert!(
        key.options.color_target.blend.is_some(),
        "an alphaMode BLEND material blends"
    );

    let images = gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
    let materials = gpu.with_mesh_source(&world, |source, world| {
        gltf.insert_materials(source, world, &images)
    });
    let meshes = gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
    let entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);

    // ...and the spawned entity is marked for back-to-front compositing.
    let z_sorted: Vec<Entity> = world
        .query::<&ZSortedDrawing>()
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(
        z_sorted, entities,
        "a blended primitive spawns with ZSortedDrawing"
    );

    world.spawn((camera(),));
    let target = gpu.bind_offscreen_target(&world, "test::gltf_blended_quad");
    gpu.render(&world);
    let px = read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target));

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

/// Unloading every resource empties the frame again.
async fn unload_empties_the_frame() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = UnlitGltf::from_buffer(&quad_document(&QuadDoc {
        translation: [0.0, 0.0, 0.0],
        textured: true,
        blend: false,
    }))
    .expect("the embedded document parses");
    let images = gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
    let materials = gpu.with_mesh_source(&world, |source, world| {
        gltf.insert_materials(source, world, &images)
    });
    let meshes = gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
    let entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
    world.spawn((camera(),));

    let target = gpu.bind_offscreen_target(&world, "test::gltf_unload_before");
    gpu.render(&world);
    let before = read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target));
    assert!(
        count_pixels_off_background(&before, CLEAR, 2) > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the quad draws before unloading"
    );

    for entity in entities {
        world.despawn(entity);
    }
    gpu.with_mesh_source(&world, |source, world| {
        gltf.unload_materials(source, world, &materials);
        gltf.unload_meshes(source, world, &meshes);
        gltf.unload_images(source, world, &images);
    });
    gpu.maintain(&world);

    let target = gpu.bind_offscreen_target(&world, "test::gltf_unload_after");
    gpu.render(&world);
    let after = read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target));
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

gpu_tests! {
    textured_quad_renders_four_colours,
    node_hierarchy_translates_the_quad,
    untextured_material_draws_flat,
    a_blended_material_composites_over_the_frame,
    unload_empties_the_frame,
}

gpu_test_main!(all_tests());
