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

/// A second buffer: one quad skinned to a single joint, four vertices with
/// positions (f32), joint indices (u16), weights (f32), an inverse bind
/// matrix (f32), six indices (u16) and one red vertex colour per vertex.
///
/// The inverse bind matrix is the identity, so the joint is *bound* at the
/// origin and any transform on the joint node moves the quad by exactly that
/// transform — which is what makes a rest pose and a moved joint tell apart.
const SKINNED_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAAQACAAAAAgADAP8AAP//AAD//wAA//8AAP8=";

/// How a document is shaped: where the mesh node sits, whether the
/// material samples a texture, and how it handles alpha.
struct QuadDoc {
    /// The mesh node's local translation.
    translation: [f32; 3],
    /// `true`: the material samples the 4x4 PNG through `TEXCOORD_0`.
    /// `false`: a flat base-color factor, no texture and no uvs.
    textured: bool,
    /// `true`: the material states `alphaMode: BLEND` and a half-opaque
    /// base-color factor.
    blend: bool,
    /// `Some(cutoff)`: the material states `alphaMode: MASK` with that
    /// `alphaCutoff`, and a half-opaque base-color factor to compare against.
    mask: Option<f32>,
}

/// The glTF JSON for one quad, as bytes for [`UnlitGltf::from_buffer`].
///
/// Node 0 is a parent of the meshed node 1, so spawning the default scene
/// takes the world transform through a hierarchy. The quad is a unit square
/// in the XY plane, CCW when seen from +Z.
fn quad_document(doc: &QuadDoc) -> Vec<u8> {
    let mut attributes = "\"POSITION\":0".to_string();
    // A half-opaque factor halves every fragment's alpha, which is what both
    // a blend and a cutoff have something to say about.
    let alpha = if doc.blend || doc.mask.is_some() {
        0.5
    } else {
        1.0
    };
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
    if let Some(cutoff) = doc.mask {
        material.push_str(&format!(",\"alphaMode\":\"MASK\",\"alphaCutoff\":{cutoff}"));
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

/// The glTF JSON of one quad skinned to a single joint, whose node sits at
/// `joint_translation`.
///
/// The mesh node sits at the origin, so the joint's world transform is
/// exactly `joint_translation` and the pose is the matrix that moves the quad
/// by it. The mesh carries a red vertex colour, so the quad it draws is one
/// flat colour and a test can count it.
fn skinned_document(joint_translation: [f32; 3]) -> Vec<u8> {
    format!(
        concat!(
            "{{\"asset\":{{\"version\":\"2.0\"}},\"scene\":0,",
            "\"scenes\":[{{\"nodes\":[0]}}],",
            "\"nodes\":[{{\"mesh\":0,\"skin\":0}},",
            "{{\"name\":\"joint\",\"translation\":[{},{},{}]}}],",
            "\"skins\":[{{\"joints\":[1],\"inverseBindMatrices\":3}}],",
            "\"meshes\":[{{\"primitives\":[{{\"attributes\":",
            "{{\"POSITION\":0,\"JOINTS_0\":1,\"WEIGHTS_0\":2,\"COLOR_0\":5}},",
            "\"indices\":4,\"material\":0}}]}}],",
            "\"materials\":[{{\"pbrMetallicRoughness\":{{\"baseColorFactor\":[1.0,1.0,1.0,1.0]}}}}],",
            "\"accessors\":[",
            "{{\"bufferView\":0,\"componentType\":5126,\"count\":4,\"type\":\"VEC3\",",
            "\"min\":[-1.8,-1.35,0.0],\"max\":[1.8,1.35,0.0]}},",
            "{{\"bufferView\":1,\"componentType\":5123,\"count\":4,\"type\":\"VEC4\"}},",
            "{{\"bufferView\":2,\"componentType\":5126,\"count\":4,\"type\":\"VEC4\"}},",
            "{{\"bufferView\":3,\"componentType\":5126,\"count\":1,\"type\":\"MAT4\"}},",
            "{{\"bufferView\":4,\"componentType\":5123,\"count\":6,\"type\":\"SCALAR\"}},",
            "{{\"bufferView\":5,\"componentType\":5121,\"count\":4,\"type\":\"VEC4\",",
            "\"normalized\":true}}],",
            "\"bufferViews\":[",
            "{{\"buffer\":0,\"byteOffset\":0,\"byteLength\":48,\"target\":34962}},",
            "{{\"buffer\":0,\"byteOffset\":48,\"byteLength\":32,\"target\":34962}},",
            "{{\"buffer\":0,\"byteOffset\":80,\"byteLength\":64,\"target\":34962}},",
            "{{\"buffer\":0,\"byteOffset\":144,\"byteLength\":64}},",
            "{{\"buffer\":0,\"byteOffset\":208,\"byteLength\":12,\"target\":34963}},",
            "{{\"buffer\":0,\"byteOffset\":220,\"byteLength\":16,\"target\":34962}}],",
            "\"buffers\":[{{\"byteLength\":236,",
            "\"uri\":\"data:application/octet-stream;base64,{}\"}}]}}"
        ),
        joint_translation[0],
        joint_translation[1],
        joint_translation[2],
        SKINNED_BIN_BASE64,
    )
    .into_bytes()
}

/// A quad with two positional morph targets, weighted `[0, 0]` on its mesh.
///
/// The buffer holds the positions (f32), the indices (u16), one red vertex
/// colour per vertex, and the two targets' per-vertex displacements: the first
/// pulls every vertex onto the origin, so a full weight collapses the quad to
/// nothing, and the second pushes every vertex well past the viewport, so a
/// full weight takes it off-screen. Either target at weight `1.0` therefore
/// empties the frame, which is what makes a blended weight tell apart from an
/// unblended one.
const MORPHED_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAABAAIAAAACAAMA/wAA//8AAP//AAD//wAA/2Zm5j/NzKw/AAAAAGZm5r/NzKw/AAAAAGZm5r/NzKy/AAAAAGZm5j/NzKy/AAAAAAAAAEEAAABBAAAAAAAAAEEAAABBAAAAAAAAAEEAAABBAAAAAAAAAEEAAABBAAAAAA==";

/// The glTF JSON of a quad whose two morph targets each empty the frame when
/// fully weighted.
fn morphed_document() -> Vec<u8> {
    let json = concat!(
        "{\"asset\":{\"version\":\"2.0\"},\"scene\":0,",
        "\"scenes\":[{\"nodes\":[0]}],",
        "\"nodes\":[{\"mesh\":0}],",
        "\"meshes\":[{\"weights\":[0.0,0.0],\"primitives\":[{\"attributes\":",
        "{\"POSITION\":0,\"COLOR_0\":2},\"indices\":1,\"material\":0,",
        "\"targets\":[{\"POSITION\":3},{\"POSITION\":4}]}]}],",
        "\"materials\":[{\"pbrMetallicRoughness\":{\"baseColorFactor\":[1.0,1.0,1.0,1.0]}}],",
        "\"accessors\":[",
        "{\"bufferView\":0,\"componentType\":5126,\"count\":4,\"type\":\"VEC3\",",
        "\"min\":[-1.8,-1.35,0.0],\"max\":[1.8,1.35,0.0]},",
        "{\"bufferView\":1,\"componentType\":5123,\"count\":6,\"type\":\"SCALAR\"},",
        "{\"bufferView\":2,\"componentType\":5121,\"count\":4,\"type\":\"VEC4\",",
        "\"normalized\":true},",
        "{\"bufferView\":3,\"componentType\":5126,\"count\":4,\"type\":\"VEC3\"},",
        "{\"bufferView\":4,\"componentType\":5126,\"count\":4,\"type\":\"VEC3\"}],",
        "\"bufferViews\":[",
        "{\"buffer\":0,\"byteOffset\":0,\"byteLength\":48,\"target\":34962},",
        "{\"buffer\":0,\"byteOffset\":48,\"byteLength\":12,\"target\":34963},",
        "{\"buffer\":0,\"byteOffset\":60,\"byteLength\":16,\"target\":34962},",
        "{\"buffer\":0,\"byteOffset\":76,\"byteLength\":48},",
        "{\"buffer\":0,\"byteOffset\":124,\"byteLength\":48}],",
        "\"buffers\":[{\"byteLength\":172,",
        "\"uri\":\"data:application/octet-stream;base64,"
    );

    // The base64 blob is a constant, not a literal a `concat!` can splice, so
    // the document is closed around it here.
    let mut document = String::from(json);
    document.push_str(MORPHED_BIN_BASE64);
    document.push_str("\"}]}");
    document.into_bytes()
}

/// A morphed quad follows its weights: spawning binds the mesh to a weight
/// entity holding the document's starting weights, and writing that entity
/// blends the targets into the geometry.
///
/// The weights are a component of an entity of their own — the one the spawn
/// created — so writing it is how a caller animates a morph.
async fn a_morphed_quad_follows_its_weights() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let gltf = UnlitGltf::from_buffer(&morphed_document()).expect("the embedded document parses");

    assert_eq!(
        gltf.morph_weights(0),
        vec![0.0, 0.0],
        "the mesh's own weights are the starting pose"
    );

    let images = gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
    let materials = gpu.with_mesh_source(&world, |source, world| {
        gltf.insert_materials(source, world, &images)
    });
    let meshes = gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
    assert!(
        meshes[0]
            .key
            .options
            .flags
            .contains(unlit_wgpu::pipeline::UnlitFlags::MORPH_POSITIONS),
        "the primitive declares targets, so the key reads displacements"
    );

    let entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
    assert_eq!(entities.len(), 1, "one mesh node draws one entity");

    let weights_entity = {
        let (_, binding) = world
            .query::<&MorphBinding>()
            .next()
            .expect("a morphed mesh binds to a weight entity");
        binding.weights
    };
    assert_eq!(
        world
            .get::<MorphWeights>(weights_entity)
            .expect("the weight entity carries a `MorphWeights`")
            .weights,
        vec![0.0, 0.0],
        "the spawn starts at the document's weights"
    );

    world.spawn((camera(),));
    let target = gpu.bind_offscreen_target(&world, "test::gltf_morphed");

    let read = |gpu: &TestGpu| {
        gpu.render(&world);
        read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target))
    };
    let undeformed = read(&gpu);
    let red = count_colour(&undeformed, [255, 0, 0], 4);
    assert!(
        red > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the unweighted quad covers the frame (red = {red})"
    );

    // The first target collapses the quad onto a point, so weighting it fully
    // must empty the frame.
    world
        .get_mut::<MorphWeights>(weights_entity)
        .expect("the weight entity carries a `MorphWeights`")
        .weights = vec![1.0, 0.0];
    let collapsed = read(&gpu);
    assert_eq!(
        count_colour(&collapsed, [255, 0, 0], 4),
        0,
        "a fully weighted collapsing target leaves nothing drawn"
    );

    // The second target pushes the quad past the viewport, so it empties the
    // frame too — by different geometry than the first, which is what shows
    // the two weights address their own target's displacements.
    world
        .get_mut::<MorphWeights>(weights_entity)
        .expect("the weight entity carries a `MorphWeights`")
        .weights = vec![0.0, 1.0];
    let displaced = read(&gpu);
    assert_eq!(
        count_colour(&displaced, [255, 0, 0], 4),
        0,
        "a fully weighted displacement target takes the quad off-screen"
    );
}

/// A skinned quad follows its joint: at rest it covers the frame, and moving
/// the joint entity slides the quad across it.
///
/// The pose is a component of its own entity — the one `spawn_default_scene`
/// created and the drawn mesh binds to — so writing it is how a caller
/// animates the skin.
async fn a_skinned_quad_follows_its_joint() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    // The joint at rest: the pose's own matrix is the identity, so the quad
    // is exactly where the document put it.
    let gltf = UnlitGltf::from_buffer(&skinned_document([0.0, 0.0, 0.0]))
        .expect("the embedded document parses");
    assert_eq!(gltf.skin_joint_count(0), Some(1), "the skin has one joint");

    let images = gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
    let materials = gpu.with_mesh_source(&world, |source, world| {
        gltf.insert_materials(source, world, &images)
    });
    let meshes = gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
    assert!(
        meshes[0]
            .key
            .options
            .flags
            .contains(unlit_wgpu::pipeline::UnlitFlags::VERTEX_JOINTS),
        "the primitive declares JOINTS_0, so the key reads joints"
    );

    let entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
    assert_eq!(entities.len(), 1, "one mesh node draws one entity");

    // The drawn entity names the pose entity the spawn created, and that
    // entity holds the rest pose: the identity, one matrix per joint.
    let pose = {
        let (_, binding) = world
            .query::<&SkinBinding>()
            .next()
            .expect("a skinned mesh binds to a pose");
        binding.pose
    };
    let rest = world
        .get::<SkinPose>(pose)
        .expect("the pose entity carries a `SkinPose`")
        .matrices
        .clone();
    assert_eq!(rest.len(), 1, "one joint, one matrix");
    assert!(
        rest[0].abs_diff_eq(glam::Mat4::IDENTITY, 1e-6),
        "the rest pose deforms nothing: {}",
        rest[0]
    );

    world.spawn((camera(),));
    let target = gpu.bind_offscreen_target(&world, "test::gltf_skinned");

    let read = |gpu: &TestGpu| {
        gpu.render(&world);
        read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target))
    };
    let before = read(&gpu);
    let red = count_colour(&before, [255, 0, 0], 4);
    assert!(
        red > (WIDTH * HEIGHT * 85 / 100) as usize,
        "the skinned quad covers the frame at rest (red = {red})"
    );

    // Move the joint a whole screen to the left: the quad follows it, and the
    // right side of the frame is left empty.
    *world
        .get_mut::<SkinPose>(pose)
        .expect("the pose entity carries a `SkinPose`") =
        SkinPose::new(vec![glam::Mat4::from_translation(glam::Vec3::new(
            -5.0, 0.0, 0.0,
        ))]);
    let after = read(&gpu);
    let red_after = count_colour(&after, [255, 0, 0], 4);
    assert!(
        red_after < red,
        "moving the joint must move the geometry (before = {red}, after = {red_after})"
    );
    assert_eq!(
        red_after, 0,
        "a joint moved past the viewport takes the quad with it"
    );
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
        mask: None,
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
            mask: None,
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
        mask: None,
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
        mask: None,
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

/// A material with `alphaMode: MASK` draws binary coverage: every fragment
/// whose alpha falls below the cutoff is discarded, so the quad disappears
/// whole when the cutoff clears its alpha and covers the frame when it does
/// not. Nothing is composited either way, so the quad needs no z-sort.
async fn a_masked_material_discards_fragments_below_its_cutoff() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    let mut render_with_cutoff = |cutoff: f32| -> Vec<u8> {
        let gltf = UnlitGltf::from_buffer(&quad_document(&QuadDoc {
            translation: [0.0, 0.0, 0.0],
            textured: true,
            blend: false,
            mask: Some(cutoff),
        }))
        .expect("the embedded document parses");

        // The cutoff is a uniform the material binds, not a variant of its
        // own: the key only declares that fragments are cut off.
        let key = gpu.with_mesh_source(&world, |source, world| {
            gltf.pipeline_key(&source.device(world), 0, 0)
        });
        assert!(
            key.options
                .flags
                .contains(unlit_wgpu::pipeline::UnlitFlags::ALPHA_CUTOFF),
            "an alphaMode MASK material cuts its fragments off"
        );
        assert!(
            key.options.color_target.blend.is_none(),
            "a cut-off material does not blend"
        );

        let images =
            gpu.with_mesh_source(&world, |source, world| gltf.insert_images(source, world));
        let materials = gpu.with_mesh_source(&world, |source, world| {
            gltf.insert_materials(source, world, &images)
        });
        let meshes =
            gpu.with_mesh_source(&world, |source, world| gltf.insert_meshes(source, world));
        let entities = gltf.spawn_default_scene(&mut world, &meshes, &materials);
        assert!(
            world.query::<&ZSortedDrawing>().next().is_none(),
            "a cut-off material draws opaque coverage, so it needs no sort"
        );

        let target_handle = gpu.bind_offscreen_target(&world, "test::gltf_masked_quad");
        world.spawn((camera(),));
        gpu.render(&world);
        let px = read_texture_bytes(
            &ctx,
            &target_handle,
            WIDTH,
            HEIGHT,
            texel_bytes(&target_handle),
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
        px
    };

    // The material's factor is half opaque, so a cutoff above it discards the
    // whole quad and one below it keeps every fragment.
    let drawn = render_with_cutoff(0.25);
    let quarter = (WIDTH * HEIGHT / 4) as usize;
    let red = count_colour(&drawn, [255, 0, 0], 0);
    assert!(
        red > quarter - 1_024 && red < quarter + 1_024,
        "a cutoff below the quad's alpha keeps the red quarter: {red} texels"
    );
    let discarded = render_with_cutoff(0.75);
    // The frame clears to black with no `RenderLoadOps` in the world, so a
    // quad that drew nothing leaves the whole frame on the background.
    assert_eq!(
        count_pixels_off_background(&discarded, [0.0, 0.0, 0.0], 2),
        0,
        "a cutoff above the quad's alpha discards every fragment"
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
        mask: None,
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
    a_masked_material_discards_fragments_below_its_cutoff,
    a_skinned_quad_follows_its_joint,
    a_morphed_quad_follows_its_weights,
    unload_empties_the_frame,
}

gpu_test_main!(all_tests());
