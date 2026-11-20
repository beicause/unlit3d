//! The GLB documents the loader tests load, built byte by byte.

use super::documents::{PNG, TRIANGLE_INDICES, TRIANGLE_POSITIONS, add_view, bytes, glb, pad};

/// A GLB holding one skinned triangle: node 0 draws mesh 0 under skin 0,
/// whose single joint is node 1 at `translation`.
///
/// The buffer carries the positions, the joint indices and weights that
/// name the joint, the skin's inverse bind matrix (the identity, so the
/// joint's own transform is what moves the vertex) and the index buffer.
/// The JSON is written out by hand rather than through a serializer,
/// because the crate carries no JSON dependency beyond the loader's own.
pub(super) fn skinned_document(translation: [f32; 3]) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();

    let position_view = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    let joints = bytes(&[[0u16, 0, 0, 0]; 3]);
    let joint_view = add_view(&mut bin, &mut views, &joints, Some(34962));
    let weights = bytes(&[[1.0f32, 0.0, 0.0, 0.0]; 3]);
    let weight_view = add_view(&mut bin, &mut views, &weights, Some(34962));
    let identity = bytes(&[glam::Mat4::IDENTITY.to_cols_array()]);
    let bind_view = add_view(&mut bin, &mut views, &identity, None);
    let index_view = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    pad(&mut bin);

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
    glb(json, &bin)
}

/// A GLB holding one triangle whose mesh declares `targets` positions
/// morph targets and states `mesh_weights` on the mesh, optionally
/// overridden by `node_weights` on the node that draws it.
///
/// Each target displaces a single vertex by `[0, 1, 0]`-ish amounts so a
/// wrong target stride reads the wrong displacement. A target passed as
/// `None` declares no positions at all, which is how a normals-only target
/// is written here.
pub(super) fn morphed_document(
    mesh_weights: Option<&[f32]>,
    node_weights: Option<&[f32]>,
    targets: &[Option<[f32; 3]>],
) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();

    let position_view = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    let mut accessors = vec![format!(
        r#"{{ "bufferView": {position_view}, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    )];
    // One `targets` entry per declared target and one accessor per target
    // that displaces, both built in the same pass so that a target which
    // displaces nothing still occupies its slot.
    let mut target_json = Vec::new();
    for target in targets {
        let Some(delta) = target else {
            target_json.push("{}".to_string());
            continue;
        };
        let mut deltas = [0.0f32; 9];
        deltas[3] = delta[0];
        deltas[4] = delta[1];
        deltas[5] = delta[2];
        let view = add_view(&mut bin, &mut views, &bytes(&[deltas]), None);
        target_json.push(format!(r#"{{"POSITION":{}}}"#, accessors.len()));
        accessors.push(format!(
            r#"{{ "bufferView": {view}, "componentType": 5126, "count": 3, "type": "VEC3" }}"#
        ));
    }
    let index_view = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    let index_accessor = accessors.len();
    accessors.push(format!(
        r#"{{ "bufferView": {index_view}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#
    ));
    pad(&mut bin);
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
    glb(json, &bin)
}

/// A GLB holding one triangle whose material states `doubleSided` or not.
pub(super) fn sided_document(double_sided: bool) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();

    let position_view = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    let index_view = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    pad(&mut bin);
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
    glb(json, &bin)
}

/// A GLB holding one primitive that draws `positions` in the `mode` it
/// states, indexing them through `indices` when the document carries an index
/// stream.
///
/// The mode reaches the loader exactly as written, so one set of vertices can
/// be drawn as a list, a strip, a loop or a fan, indexed or not.
pub(super) fn mode_document(
    mode: gltf::mesh::Mode,
    positions: &[[f32; 3]],
    indices: Option<&[u16]>,
) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();

    let (min, max) = positions.iter().fold(
        ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]),
        |(mut min, mut max), position| {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
            (min, max)
        },
    );
    let bounds = |values: [f32; 3]| format!("[{},{},{}]", values[0], values[1], values[2]);
    let position_view = add_view(&mut bin, &mut views, &bytes(positions), Some(34962));
    let mut accessors = vec![format!(
        r#"{{ "bufferView": {position_view}, "componentType": 5126, "count": {}, "type": "VEC3", "min": {}, "max": {} }}"#,
        positions.len(),
        bounds(min),
        bounds(max),
    )];
    let indices = match indices {
        Some(indices) => {
            let view = add_view(&mut bin, &mut views, &bytes(indices), Some(34963));
            accessors.push(format!(
                r#"{{ "bufferView": {view}, "componentType": 5123, "count": {}, "type": "SCALAR" }}"#,
                indices.len()
            ));
            format!(r#","indices":{}"#, accessors.len() - 1)
        }
        None => String::new(),
    };
    pad(&mut bin);
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
            "scene":0,
            "scenes":[{{"nodes":[0]}}],
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}{indices},
                                        "mode":{mode}}}]}}],
            "accessors":[{}],
            "bufferViews":[{}],
            "buffers":[{{"byteLength":{}}}]}}"#,
        accessors.join(", "),
        views.join(", "),
        bin.len(),
        mode = mode.as_gl_enum(),
    );
    glb(json, &bin)
}

/// A GLB holding one triangle whose material samples the embedded PNG through
/// the `texCoord` its base-color texture names, carrying the `TEXCOORD_n`
/// sets listed in `carried`.
///
/// Every carried set holds one constant UV naming the set it came from, so a
/// stream that was read from the wrong set is a wrong value rather than an
/// ambiguous one.
pub(super) fn uv_document(texture_set: u32, carried: &[u32]) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut accessors: Vec<String> = Vec::new();

    let position_view = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    accessors.push(format!(
        r#"{{ "bufferView": {position_view}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let mut attributes = vec!["\"POSITION\":0".to_string()];
    for &set in carried {
        let uv = [set as f32 + 0.25, 0.5];
        let view = add_view(&mut bin, &mut views, &bytes(&[uv; 3]), Some(34962));
        attributes.push(format!(r#""TEXCOORD_{set}":{}"#, accessors.len()));
        accessors.push(format!(
            r#"{{ "bufferView": {view}, "componentType": 5126, "count": 3, "type": "VEC2" }}"#
        ));
    }
    let index_view = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    let index_accessor = accessors.len();
    accessors.push(format!(
        r#"{{ "bufferView": {index_view}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#
    ));
    let image_view = add_view(&mut bin, &mut views, &PNG, None);
    pad(&mut bin);
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
            "scene":0,
            "scenes":[{{"nodes":[0]}}],
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{{}}},
                                        "indices":{index_accessor},
                                        "material":0}}]}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1.0,1.0,1.0,1.0],
                          "baseColorTexture":{{"index":0,"texCoord":{texture_set}}}}}}}],
            "textures":[{{"source":0}}],
            "images":[{{"bufferView":{image_view},"mimeType":"image/png"}}],
            "accessors":[{}],
            "bufferViews":[{}],
            "buffers":[{{"byteLength":{}}}]}}"#,
        attributes.join(","),
        accessors.join(", "),
        views.join(", "),
        bin.len(),
    );
    glb(json, &bin)
}

/// The `min` and `max` a glTF accessor needs, as a JSON fragment.
pub(super) fn bounds(times: &[f32]) -> String {
    let min = times.first().copied().unwrap_or(0.0);
    let max = times.last().copied().unwrap_or(0.0);
    format!(r#", "min": [{min}], "max": [{max}]"#)
}

/// Adds the triangle both an animated mesh and a static one draw, and
/// pushes its position and index accessors, returning the index accessor.
pub(super) fn add_triangle(
    bin: &mut Vec<u8>,
    views: &mut Vec<String>,
    accessors: &mut Vec<String>,
) -> usize {
    let position = add_view(bin, views, &bytes(&TRIANGLE_POSITIONS), Some(34962));
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let index_accessor = accessors.len();
    let index = add_view(bin, views, &bytes(&TRIANGLE_INDICES), Some(34963));
    accessors.push(format!(
        r#"{{ "bufferView": {index}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#
    ));
    index_accessor
}

/// A GLB whose node 0 carries a translation animation and whose child node
/// 1 — drawing a triangle — carries a rotation animation.
///
/// One document therefore exercises both node paths and the parent-to-child
/// recomposition. `translations` and `rotations` hold one entry per
/// keyframe, except under `CUBICSPLINE`, where each keyframe contributes
/// its in-tangent, value and out-tangent in that order.
pub(super) fn parented_animated_document(
    interpolation: &str,
    times: &[f32],
    translations: &[[f32; 3]],
    rotations: &[[f32; 4]],
) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut accessors: Vec<String> = Vec::new();

    let input_bytes: Vec<u8> = times.iter().flat_map(|t| t.to_le_bytes()).collect();
    let input = add_view(&mut bin, &mut views, &input_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {input}, "componentType": 5126, "count": {}, "type": "SCALAR"{} }}"#,
        times.len(),
        bounds(times),
    ));
    let translation_bytes: Vec<u8> = translations
        .iter()
        .flatten()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let translation = add_view(&mut bin, &mut views, &translation_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {translation}, "componentType": 5126, "count": {}, "type": "VEC3" }}"#,
        translations.len()
    ));
    let rotation_bytes: Vec<u8> = rotations
        .iter()
        .flatten()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let rotation = add_view(&mut bin, &mut views, &rotation_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {rotation}, "componentType": 5126, "count": {}, "type": "VEC4" }}"#,
        rotations.len()
    ));
    let index_accessor = add_triangle(&mut bin, &mut views, &mut accessors);

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"children":[1]}},{{"mesh":0}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":3}},"indices":{index_accessor}}}]}}],
        "accessors":[{}],
        "bufferViews":[{}],
        "buffers":[{{"byteLength":{}}}],
        "animations":[{{"name":"walk",
            "channels":[{{"sampler":0,"target":{{"node":0,"path":"translation"}}}},
                        {{"sampler":1,"target":{{"node":1,"path":"rotation"}}}}],
            "samplers":[{{"input":0,"interpolation":"{interpolation}","output":1}},
                        {{"input":0,"interpolation":"{interpolation}","output":2}}]}}]}}"#,
        accessors.join(", "),
        views.join(", "),
        bin.len(),
    );
    glb(json, &bin)
}

/// A GLB holding one skinned triangle whose single joint node carries a
/// translation animation.
pub(super) fn animated_skinned_document(times: &[f32], translations: &[[f32; 3]]) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut accessors: Vec<String> = Vec::new();

    let input_bytes: Vec<u8> = times.iter().flat_map(|t| t.to_le_bytes()).collect();
    let input = add_view(&mut bin, &mut views, &input_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {input}, "componentType": 5126, "count": {}, "type": "SCALAR"{} }}"#,
        times.len(),
        bounds(times),
    ));
    let translation_bytes: Vec<u8> = translations
        .iter()
        .flatten()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let translation = add_view(&mut bin, &mut views, &translation_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {translation}, "componentType": 5126, "count": {}, "type": "VEC3" }}"#,
        translations.len()
    ));

    let position = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let joint = add_view(
        &mut bin,
        &mut views,
        &bytes(&[[0u16, 0, 0, 0]; 3]),
        Some(34962),
    );
    accessors.push(format!(
        r#"{{ "bufferView": {joint}, "componentType": 5123, "count": 3, "type": "VEC4" }}"#
    ));
    let weight = add_view(
        &mut bin,
        &mut views,
        &bytes(&[[1.0f32, 0.0, 0.0, 0.0]; 3]),
        Some(34962),
    );
    accessors.push(format!(
        r#"{{ "bufferView": {weight}, "componentType": 5126, "count": 3, "type": "VEC4" }}"#
    ));
    let identity = bytes(&[glam::Mat4::IDENTITY.to_cols_array()]);
    let bind = add_view(&mut bin, &mut views, &identity, None);
    accessors.push(format!(
        r#"{{ "bufferView": {bind}, "componentType": 5126, "count": 1, "type": "MAT4" }}"#
    ));
    let index_accessor = accessors.len();
    let index = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    accessors.push(format!(
        r#"{{ "bufferView": {index}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#
    ));

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0,"skin":0}},{{"name":"joint"}}],
        "skins":[{{"joints":[1],"inverseBindMatrices":5}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":2,"JOINTS_0":3,"WEIGHTS_0":4}},
                                    "indices":{index_accessor}}}]}}],
        "accessors":[{}],
        "bufferViews":[{}],
        "buffers":[{{"byteLength":{}}}],
        "animations":[{{"name":"walk",
            "channels":[{{"sampler":0,"target":{{"node":1,"path":"translation"}}}}],
            "samplers":[{{"input":0,"interpolation":"LINEAR","output":1}}]}}]}}"#,
        accessors.join(", "),
        views.join(", "),
        bin.len(),
    );
    glb(json, &bin)
}

/// A GLB holding one triangle with a single morph target whose weight the
/// document animates.
pub(super) fn animated_morphed_document(times: &[f32], weights: &[f32]) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut accessors: Vec<String> = Vec::new();

    let input_bytes: Vec<u8> = times.iter().flat_map(|t| t.to_le_bytes()).collect();
    let input = add_view(&mut bin, &mut views, &input_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {input}, "componentType": 5126, "count": {}, "type": "SCALAR"{} }}"#,
        times.len(),
        bounds(times),
    ));
    let weight_bytes: Vec<u8> = weights.iter().flat_map(|w| w.to_le_bytes()).collect();
    let weight = add_view(&mut bin, &mut views, &weight_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {weight}, "componentType": 5126, "count": {}, "type": "SCALAR" }}"#,
        weights.len()
    ));

    let position = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let mut deltas = [0.0f32; 9];
    deltas[4] = 1.0;
    let delta = add_view(&mut bin, &mut views, &bytes(&[deltas]), None);
    accessors.push(format!(
        r#"{{ "bufferView": {delta}, "componentType": 5126, "count": 3, "type": "VEC3" }}"#
    ));
    let index_accessor = accessors.len();
    let index = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    accessors.push(format!(
        r#"{{ "bufferView": {index}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#
    ));

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":2}},"indices":{index_accessor},
                                    "targets":[{{"POSITION":3}}]}}]}}],
        "accessors":[{}],
        "bufferViews":[{}],
        "buffers":[{{"byteLength":{}}}],
        "animations":[{{"name":"walk",
            "channels":[{{"sampler":0,"target":{{"node":0,"path":"weights"}}}}],
            "samplers":[{{"input":0,"interpolation":"LINEAR","output":1}}]}}]}}"#,
        accessors.join(", "),
        views.join(", "),
        bin.len(),
    );
    glb(json, &bin)
}

/// A GLB whose only node states its transform as a `matrix` and carries both a
/// translation animation and a weights animation on its one morph target.
///
/// The specification gives a matrix node no animatable properties, so the
/// translation channel has to be dropped; the morph weights belong to the mesh
/// rather than to the transform and stay animatable.
pub(super) fn matrix_animated_document(
    matrix: [f32; 16],
    times: &[f32],
    translations: &[[f32; 3]],
    weights: &[f32],
) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut accessors: Vec<String> = Vec::new();

    let input_bytes: Vec<u8> = times.iter().flat_map(|t| t.to_le_bytes()).collect();
    let input = add_view(&mut bin, &mut views, &input_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {input}, "componentType": 5126, "count": {}, "type": "SCALAR"{} }}"#,
        times.len(),
        bounds(times),
    ));
    let translation_bytes: Vec<u8> = translations
        .iter()
        .flatten()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let translation = add_view(&mut bin, &mut views, &translation_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {translation}, "componentType": 5126, "count": {}, "type": "VEC3" }}"#,
        translations.len()
    ));
    let weight_bytes: Vec<u8> = weights.iter().flat_map(|w| w.to_le_bytes()).collect();
    let weight = add_view(&mut bin, &mut views, &weight_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {weight}, "componentType": 5126, "count": {}, "type": "SCALAR" }}"#,
        weights.len()
    ));

    let position = add_view(
        &mut bin,
        &mut views,
        &bytes(&TRIANGLE_POSITIONS),
        Some(34962),
    );
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let mut deltas = [0.0f32; 9];
    deltas[4] = 1.0;
    let delta = add_view(&mut bin, &mut views, &bytes(&[deltas]), None);
    accessors.push(format!(
        r#"{{ "bufferView": {delta}, "componentType": 5126, "count": 3, "type": "VEC3" }}"#
    ));
    let index_accessor = accessors.len();
    let index = add_view(&mut bin, &mut views, &bytes(&TRIANGLE_INDICES), Some(34963));
    accessors.push(format!(
        r#"{{ "bufferView": {index}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#
    ));
    let matrix: Vec<String> = matrix.iter().map(|f| f.to_string()).collect();

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"matrix":[{}],"mesh":0}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":3}},"indices":{index_accessor},
                                    "targets":[{{"POSITION":4}}]}}]}}],
        "accessors":[{}],
        "bufferViews":[{}],
        "buffers":[{{"byteLength":{}}}],
        "animations":[{{"name":"walk",
            "channels":[{{"sampler":0,"target":{{"node":0,"path":"translation"}}}},
                        {{"sampler":1,"target":{{"node":0,"path":"weights"}}}}],
            "samplers":[{{"input":0,"interpolation":"LINEAR","output":1}},
                        {{"input":0,"interpolation":"LINEAR","output":2}}]}}]}}"#,
        matrix.join(", "),
        accessors.join(", "),
        views.join(", "),
        bin.len(),
    );
    glb(json, &bin)
}
