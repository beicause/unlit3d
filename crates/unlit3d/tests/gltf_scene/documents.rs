//! The glTF documents the tests render, built in memory as JSON plus a data-URI buffer.

use super::*;

/// The buffer: 4 positions (f32), 4 uvs (f32), 6 indices (u16), the PNG.
/// `quad_document` puts it in the doc as a data URI.
pub(super) const BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAAAAAAAgD8AAIA/AACAPwAAgD8AAAAAAAAAAAAAAAAAAAEAAgAAAAIAAwCJUE5HDQoaCgAAAA1JSERSAAAABAAAAAQIBgAAAKnxnn4AAAAXSURBVHicY/jPwPAfGYMQKkRTABRCAwB0CCfZGxzjzQAAAABJRU5ErkJggg==";
pub(super) const POS_OFFSET: u32 = 0; // 4 × Vec3 f32
pub(super) const UV_OFFSET: u32 = 48; // 4 × Vec2 f32
pub(super) const IDX_OFFSET: u32 = 80; // 6 × u16
pub(super) const IMG_OFFSET: u32 = 92; // the 80-byte PNG
pub(super) const BIN_LENGTH: u32 = 172;

/// A second buffer: one quad skinned to a single joint, four vertices with
/// positions (f32), joint indices (u16), weights (f32), an inverse bind
/// matrix (f32), six indices (u16) and one red vertex colour per vertex.
///
/// The inverse bind matrix is the identity, so the joint is *bound* at the
/// origin and any transform on the joint node moves the quad by exactly that
/// transform — which is what makes a rest pose and a moved joint tell apart.
pub(super) const SKINNED_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAAQACAAAAAgADAP8AAP//AAD//wAA//8AAP8=";

/// How a document is shaped: where the mesh node sits, whether the
/// material samples a texture, and how it handles alpha.
pub(super) struct QuadDoc {
    /// The mesh node's local translation.
    pub(super) translation: [f32; 3],
    /// `true`: the material samples the 4x4 PNG through `TEXCOORD_0`.
    /// `false`: a flat base-color factor, no texture and no uvs.
    pub(super) textured: bool,
    /// `true`: the material states `alphaMode: BLEND` and a half-opaque
    /// base-color factor.
    pub(super) blend: bool,
    /// `Some(cutoff)`: the material states `alphaMode: MASK` with that
    /// `alphaCutoff`, and a half-opaque base-color factor to compare against.
    pub(super) mask: Option<f32>,
    /// `true`: the material states `doubleSided`, so its back faces draw too.
    pub(super) double_sided: bool,
}

/// The glTF JSON for one quad, as bytes for [`LitGltf::from_bytes`].
///
/// Node 0 is a parent of the meshed node 1, so spawning the default scene
/// takes the world transform through a hierarchy. The quad is a unit square
/// in the XY plane, CCW when seen from +Z.
pub(super) fn quad_document(doc: &QuadDoc) -> Vec<u8> {
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
    if doc.double_sided {
        material.push_str(",\"doubleSided\":true");
    }
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"children":[1]}},{{"mesh":0,"translation":[{},{},{}]}}],
        "meshes":[{{"primitives":[{{"attributes":{{{}}},
        "indices":2,"material":0}}]}}],
        "materials":[{{{}}}]{},
        "accessors":[
        {{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3",
        "min":[-1.8,-1.35,0.0],"max":[1.8,1.35,0.0]}},
        {{"bufferView":1,"componentType":5126,"count":4,"type":"VEC2"}},
        {{"bufferView":2,"componentType":5123,"count":6,"type":"SCALAR"}}],
        "bufferViews":[
        {{"buffer":0,"byteOffset":{},"byteLength":48,"target":34962}},
        {{"buffer":0,"byteOffset":{},"byteLength":32,"target":34962}},
        {{"buffer":0,"byteOffset":{},"byteLength":12,"target":34963}},
        {{"buffer":0,"byteOffset":{},"byteLength":80}}],
        "buffers":[{{"byteLength":{},"uri":"data:application/octet-stream;base64,{}"}}]}}"#,
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
pub(super) fn count_colour(px: &[u8], colour: [u8; 3], tolerance: u8) -> usize {
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
pub(super) fn camera() -> Camera {
    camera_looking_at(
        glam::Vec3::new(0.0, 0.0, 2.34),
        glam::Vec3::ZERO,
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// Spawn the document's default scene into the harness's mesh source.
///
/// A frame draws a source's own subtree, so the nodes have to land under the
/// source rather than beside it.
pub(super) fn spawn_scene(
    gpu: &TestGpu,
    world: &mut World,
    gltf: &LitGltf,
    resources: &GltfResources,
) -> Vec<GltfNode> {
    let source = gpu.mesh_source();
    let mut scope = world
        .scope_mut(source)
        .expect("the source entity is its own scope");
    gltf.spawn_default_scene(&mut scope, resources)
}

/// The glTF JSON of one quad skinned to a single joint, whose node sits at
/// `joint_translation`.
///
/// The mesh node sits at the origin, so the joint's world transform is
/// exactly `joint_translation` and the pose is the matrix that moves the quad
/// by it. The mesh carries a red vertex colour, so the quad it draws is one
/// flat colour and a test can count it.
pub(super) fn skinned_document(joint_translation: [f32; 3]) -> Vec<u8> {
    format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0,"skin":0}},
        {{"name":"joint","translation":[{},{},{}]}}],
        "skins":[{{"joints":[1],"inverseBindMatrices":3}}],
        "meshes":[{{"primitives":[{{"attributes":
        {{"POSITION":0,"JOINTS_0":1,"WEIGHTS_0":2,"COLOR_0":5}},
        "indices":4,"material":0}}]}}],
        "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1.0,1.0,1.0,1.0]}}}}],
        "accessors":[
        {{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3",
        "min":[-1.8,-1.35,0.0],"max":[1.8,1.35,0.0]}},
        {{"bufferView":1,"componentType":5123,"count":4,"type":"VEC4"}},
        {{"bufferView":2,"componentType":5126,"count":4,"type":"VEC4"}},
        {{"bufferView":3,"componentType":5126,"count":1,"type":"MAT4"}},
        {{"bufferView":4,"componentType":5123,"count":6,"type":"SCALAR"}},
        {{"bufferView":5,"componentType":5121,"count":4,"type":"VEC4",
        "normalized":true}}],
        "bufferViews":[
        {{"buffer":0,"byteOffset":0,"byteLength":48,"target":34962}},
        {{"buffer":0,"byteOffset":48,"byteLength":32,"target":34962}},
        {{"buffer":0,"byteOffset":80,"byteLength":64,"target":34962}},
        {{"buffer":0,"byteOffset":144,"byteLength":64}},
        {{"buffer":0,"byteOffset":208,"byteLength":12,"target":34963}},
        {{"buffer":0,"byteOffset":220,"byteLength":16,"target":34962}}],
        "buffers":[{{"byteLength":236,
        "uri":"data:application/octet-stream;base64,{}"}}]}}"#,
        joint_translation[0], joint_translation[1], joint_translation[2], SKINNED_BIN_BASE64,
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
pub(super) const MORPHED_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAABAAIAAAACAAMA/wAA//8AAP//AAD//wAA/2Zm5j/NzKw/AAAAAGZm5r/NzKw/AAAAAGZm5r/NzKy/AAAAAGZm5j/NzKy/AAAAAAAAAEEAAABBAAAAAAAAAEEAAABBAAAAAAAAAEEAAABBAAAAAAAAAEEAAABBAAAAAA==";

/// The glTF JSON of a quad whose two morph targets each empty the frame when
/// fully weighted.
pub(super) fn morphed_document() -> Vec<u8> {
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
 "scenes":[{"nodes":[0]}],
 "nodes":[{"mesh":0}],
 "meshes":[{"weights":[0.0,0.0],"primitives":[{"attributes":
 {"POSITION":0,"COLOR_0":2},"indices":1,"material":0,
 "targets":[{"POSITION":3},{"POSITION":4}]}]}],
 "materials":[{"pbrMetallicRoughness":{"baseColorFactor":[1.0,1.0,1.0,1.0]}}],
 "accessors":[
 {"bufferView":0,"componentType":5126,"count":4,"type":"VEC3",
 "min":[-1.8,-1.35,0.0],"max":[1.8,1.35,0.0]},
 {"bufferView":1,"componentType":5123,"count":6,"type":"SCALAR"},
 {"bufferView":2,"componentType":5121,"count":4,"type":"VEC4",
 "normalized":true},
 {"bufferView":3,"componentType":5126,"count":4,"type":"VEC3"},
 {"bufferView":4,"componentType":5126,"count":4,"type":"VEC3"}],
 "bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":48,"target":34962},
 {"buffer":0,"byteOffset":48,"byteLength":12,"target":34963},
 {"buffer":0,"byteOffset":60,"byteLength":16,"target":34962},
 {"buffer":0,"byteOffset":76,"byteLength":48},
 {"buffer":0,"byteOffset":124,"byteLength":48}],
 "buffers":[{"byteLength":172,
 "uri":"data:application/octet-stream;base64,"#;

    // The base64 blob is a constant, not a literal a raw string can splice, so
    // the document is closed around it here.
    let mut document = String::from(json);
    document.push_str(MORPHED_BIN_BASE64);
    document.push_str(r#""}]}"#);
    document.into_bytes()
}

/// The skinned quad's buffer plus its animation: two keyframe times (f32) and
/// the joint's translation at each, the second taking it off-screen.
pub(super) const ANIMATED_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAAQACAAAAAgADAP8AAP//AAD//wAA//8AAP8AAAAAAAAAQAAAAAAAAAAAAAAAAAAAoMAAAAAAAAAAAA==";

/// The glTF JSON of the skinned quad, animated: the joint node carries a
/// translation channel whose keyframes put it at the origin at `t = 0` and a
/// whole screen to the left at `t = 2`.
///
/// The mesh node itself is never animated, so the quad only leaves the frame
/// because `apply_animation` recomposed the joint and rebuilt the skin pose
/// against it.
pub(super) fn animated_skinned_document() -> Vec<u8> {
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
 "scenes":[{"nodes":[0]}],
 "nodes":[{"mesh":0,"skin":0},{"name":"joint"}],
 "skins":[{"joints":[1],"inverseBindMatrices":3}],
 "meshes":[{"primitives":[{"attributes":
 {"POSITION":0,"JOINTS_0":1,"WEIGHTS_0":2,"COLOR_0":5},
 "indices":4,"material":0}]}],
 "materials":[{"pbrMetallicRoughness":{"baseColorFactor":[1.0,1.0,1.0,1.0]}}],
 "accessors":[
 {"bufferView":0,"componentType":5126,"count":4,"type":"VEC3",
 "min":[-1.8,-1.35,0.0],"max":[1.8,1.35,0.0]},
 {"bufferView":1,"componentType":5123,"count":4,"type":"VEC4"},
 {"bufferView":2,"componentType":5126,"count":4,"type":"VEC4"},
 {"bufferView":3,"componentType":5126,"count":1,"type":"MAT4"},
 {"bufferView":4,"componentType":5123,"count":6,"type":"SCALAR"},
 {"bufferView":5,"componentType":5121,"count":4,"type":"VEC4",
 "normalized":true},
 {"bufferView":6,"componentType":5126,"count":2,"type":"SCALAR",
 "min":[0.0],"max":[2.0]},
 {"bufferView":7,"componentType":5126,"count":2,"type":"VEC3"}],
 "bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":48,"target":34962},
 {"buffer":0,"byteOffset":48,"byteLength":32,"target":34962},
 {"buffer":0,"byteOffset":80,"byteLength":64,"target":34962},
 {"buffer":0,"byteOffset":144,"byteLength":64},
 {"buffer":0,"byteOffset":208,"byteLength":12,"target":34963},
 {"buffer":0,"byteOffset":220,"byteLength":16,"target":34962},
 {"buffer":0,"byteOffset":236,"byteLength":8},
 {"buffer":0,"byteOffset":244,"byteLength":24}],
 "animations":[{"name":"walk",
 "channels":[{"sampler":0,"target":{"node":1,"path":"translation"}}],
 "samplers":[{"input":6,"interpolation":"LINEAR","output":7}]}],
 "buffers":[{"byteLength":268,
 "uri":"data:application/octet-stream;base64,"#;

    // The base64 blob is a constant, not a literal a raw string can splice, so
    // the document is closed around it here.
    let mut document = String::from(json);
    document.push_str(ANIMATED_BIN_BASE64);
    document.push_str(r#""}]}"#);
    document.into_bytes()
}

/// The buffer of one quad whose triangles are wound the other way round, so
/// the face the frame's camera sees is a *back* face.
///
/// Same corners as the other tests; only the index order differs, which is the
/// whole point. The vertices carry a flat red colour rather than a texture, so
/// the document needs no image and the buffer is the geometry alone: positions
/// (48 bytes), colours (16) and indices (12).
pub(super) const REVERSED_BIN_BASE64: &str = "Zmbmv83MrL8AAAAAZmbmP83MrL8AAAAAZmbmP83MrD8AAAAAZmbmv83MrD8AAAAA/wAA//8AAP//AAD//wAA/wAAAgABAAAAAwACAA==";

/// The glTF JSON of the reversed-winding quad, optionally `doubleSided`.
pub(super) fn reversed_document(double_sided: bool) -> Vec<u8> {
    let sided = if double_sided {
        ",\"doubleSided\":true"
    } else {
        ""
    };
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0}}],
        "meshes":[{{"primitives":[{{"attributes":
        {{"POSITION":0,"COLOR_0":1}},"indices":2,"material":0}}]}}],
        "materials":[{{"pbrMetallicRoughness":
        {{"baseColorFactor":[1.0,1.0,1.0,1.0]}}{sided}}}],
        "accessors":[
        {{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3",
        "min":[-1.8,-1.35,0.0],"max":[1.8,1.35,0.0]}},
        {{"bufferView":1,"componentType":5121,"count":4,"type":"VEC4",
        "normalized":true}},
        {{"bufferView":2,"componentType":5123,"count":6,"type":"SCALAR"}}],
        "bufferViews":[
        {{"buffer":0,"byteOffset":0,"byteLength":48,"target":34962}},
        {{"buffer":0,"byteOffset":48,"byteLength":16,"target":34962}},
        {{"buffer":0,"byteOffset":64,"byteLength":12,"target":34963}}],
        "buffers":[{{"byteLength":76,
        "uri":"data:application/octet-stream;base64,"#,
        sided = sided
    );

    // The base64 blob is a constant, not a literal a raw string can splice.
    let mut document = json;
    document.push_str(REVERSED_BIN_BASE64);
    document.push_str(r#""}]}"#);
    document.into_bytes()
}
