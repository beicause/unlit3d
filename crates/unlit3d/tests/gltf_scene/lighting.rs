//! What the frame's lights do to a document's materials: a material carrying
//! `KHR_materials_unlit` draws its base colour whatever the frame's lights
//! are, and a plain material shades by them.

use super::documents::{camera, count_colour, reversed_document, spawn_scene};
use super::{Ctx, HEIGHT, LitGltf, TestGpu, WIDTH, World, bundle};
use unlit3d::prelude::{AmbientLight, DirectionalLight, LightSource, Transform};

/// The buffer of one quad: positions (48 bytes), red vertex colours (16) and
/// indices (12).
const LIT_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAA/wAA//8AAP//AAD//wAA/wAAAQACAAAAAgADAA==";

/// The glTF JSON of a double-sided quad whose material carries no extension:
/// plain `pbrMetallicRoughness`, so it draws lit and shades by the frame's
/// lights.
fn lit_document() -> Vec<u8> {
    let json = r#"{"asset":{"version":"2.0"},
 "scene":0,
 "scenes":[{"nodes":[0]}],
 "nodes":[{"mesh":0}],
 "meshes":[{"primitives":[{"attributes":
 {"POSITION":0,"COLOR_0":1},"indices":2,"material":0}]}],
 "materials":[{"doubleSided":true,
 "pbrMetallicRoughness":{"baseColorFactor":[1.0,1.0,1.0,1.0]}}],
 "accessors":[
 {"bufferView":0,"componentType":5126,"count":4,"type":"VEC3",
 "min":[-1.8,-1.35,0.0],"max":[1.8,1.35,0.0]},
 {"bufferView":1,"componentType":5121,"count":4,"type":"VEC4",
 "normalized":true},
 {"bufferView":2,"componentType":5123,"count":6,"type":"SCALAR"}],
 "bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":48,"target":34962},
 {"buffer":0,"byteOffset":48,"byteLength":16,"target":34962},
 {"buffer":0,"byteOffset":64,"byteLength":12,"target":34963}],
 "buffers":[{"byteLength":76,
 "uri":"data:application/octet-stream;base64,"#;

    let mut document = String::from(json);
    document.push_str(LIT_BIN_BASE64);
    document.push_str(r#""}]}"#);
    document.into_bytes()
}

/// An unlit material ignores the frame's lights entirely: the same document
/// renders to the same pixels with no lights and with a frame-full of them.
pub(super) async fn an_unlit_material_ignores_the_lights() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&reversed_document(true)).expect("the embedded document parses");
    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    let _entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);

    let target = gpu.bind_offscreen_target(&world, "test::gltf_unlit_material");
    gpu.render(&world);
    let dark = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    // A frame-full of light: an ambient term at full white and a directional
    // light far brighter than any surface could take without blowing out. An
    // unlit draw reads neither.
    {
        let mut scope = world.scope_mut(gpu.mesh_source());
        scope.spawn(bundle![AmbientLight::new(glam::Vec4::ONE)]);
        scope.spawn(bundle![
            Transform::default(),
            LightSource::new(DirectionalLight {
                color: glam::Vec4::ONE,
                intensity: 10.0,
            }),
        ]);
    }

    let target = gpu.bind_offscreen_target(&world, "test::gltf_unlit_material");
    gpu.render(&world);
    let lit = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;

    assert_eq!(dark, lit, "the lights must not reach an unlit material");
    assert!(
        count_colour(&lit, [255, 0, 0], 4) > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the unlit quad covers the frame in the red its vertices carry"
    );
}

/// A plain material — no extension, so lit — shades by the frame's lights: it
/// draws black under no light and its base colour under a full ambient term.
pub(super) async fn a_plain_material_shades_by_the_lights() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = LitGltf::from_bytes(&lit_document()).expect("the embedded document parses");
    let resources =
        gpu.with_mesh_source(&world, |source, world| gltf.insert_resources(source, world));
    let _entities = spawn_scene(&gpu, &mut world, &gltf, &resources);
    gpu.spawn(&mut world, bundle![camera()]);

    let target = gpu.bind_offscreen_target(&world, "test::gltf_lit_material");
    gpu.render(&world);
    let dark = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;
    assert!(
        count_colour(&dark, [0, 0, 0], 2) > (WIDTH * HEIGHT * 85 / 100) as usize,
        "without lights the quad draws black over the background"
    );
    assert!(
        count_colour(&dark, [255, 0, 0], 4) < 64,
        "without lights no texel reaches the base colour"
    );

    {
        let mut scope = world.scope_mut(gpu.mesh_source());
        scope.spawn(bundle![AmbientLight::new(glam::Vec4::ONE)]);
    }

    let target = gpu.bind_offscreen_target(&world, "test::gltf_lit_material");
    gpu.render(&world);
    let lit = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;
    assert!(
        count_colour(&lit, [255, 0, 0], 8) > (WIDTH * HEIGHT * 85 / 100) as usize,
        "a full ambient term brings the quad to its base colour"
    );
}
