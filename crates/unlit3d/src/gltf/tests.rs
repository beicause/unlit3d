//! Integration-style unit tests over synthetic glTF documents.

use super::*;
use unlit_ecs::World;

    /// A GLB holding one skinned triangle: node 0 draws mesh 0 under skin 0,
    /// whose single joint is node 1 at `translation`.
    ///
    /// The buffer carries the positions, the joint indices and weights that
    /// name the joint, the skin's inverse bind matrix (the identity, so the
    /// joint's own transform is what moves the vertex) and the index buffer.
    /// The JSON is written out by hand rather than through a serializer,
    /// because the crate carries no JSON dependency beyond the loader's own.
    fn skinned_document(translation: [f32; 3]) -> Vec<u8> {
        let mut bin = Vec::new();
        let mut views: Vec<String> = Vec::new();
        let mut add = |bin: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
            views.push(format!(
                r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
                bin.len(),
                bytes.len()
            ));
            bin.extend_from_slice(bytes);
            views.len() - 1
        };

        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let position_view = add(&mut bin, &positions, Some(34962));
        let joints: Vec<u8> = (0..3)
            .flat_map(|_| [0u16, 0, 0, 0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let joint_view = add(&mut bin, &joints, Some(34962));
        let weights: Vec<u8> = (0..3)
            .flat_map(|_| [1.0f32, 0.0, 0.0, 0.0])
            .flat_map(f32::to_le_bytes)
            .collect();
        let weight_view = add(&mut bin, &weights, Some(34962));
        let identity: Vec<u8> = [
            1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
        let bind_view = add(&mut bin, &identity, None);
        let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_view = add(&mut bin, &indices, Some(34963));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }

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
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    /// A GLB holding one triangle whose mesh declares `targets` positions
    /// morph targets and states `mesh_weights` on the mesh, optionally
    /// overridden by `node_weights` on the node that draws it.
    ///
    /// Each target displaces a single vertex by `[0, 1, 0]`-ish amounts so a
    /// wrong target stride reads the wrong displacement. A target passed as
    /// `None` declares no positions at all, which is how a normals-only target
    /// is written here.
    fn morphed_document(
        mesh_weights: Option<&[f32]>,
        node_weights: Option<&[f32]>,
        targets: &[Option<[f32; 3]>],
    ) -> Vec<u8> {
        let mut bin = Vec::new();
        let mut views: Vec<String> = Vec::new();
        let mut add = |bin: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
            views.push(format!(
                r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
                bin.len(),
                bytes.len()
            ));
            bin.extend_from_slice(bytes);
            views.len() - 1
        };

        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let mut accessors = vec![format!(
            r#"{{ "bufferView": {}, "componentType": 5126, "count": 3, "type": "VEC3",
                 "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#,
            add(&mut bin, &positions, Some(34962))
        )];
        // One accessor per target that displaces, in target order.
        let mut displacement_accessors = Vec::new();
        for target in targets {
            let Some(delta) = target else {
                continue;
            };
            let mut deltas = [0.0f32; 9];
            deltas[3] = delta[0];
            deltas[4] = delta[1];
            deltas[5] = delta[2];
            let bytes: Vec<u8> = deltas.iter().flat_map(|f| f.to_le_bytes()).collect();
            let view = add(&mut bin, &bytes, None);
            displacement_accessors.push(accessors.len());
            accessors.push(format!(
                r#"{{ "bufferView": {view}, "componentType": 5126, "count": 3, "type": "VEC3" }}"#
            ));
        }
        let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_accessor = accessors.len();
        accessors.push(format!(
            r#"{{ "bufferView": {}, "componentType": 5123, "count": 3, "type": "SCALAR" }}"#,
            add(&mut bin, &indices, Some(34963))
        ));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }

        // A target that carries no positions is declared as an empty object,
        // which is what glTF allows and what the loader must skip.
        let target_json: Vec<String> = targets
            .iter()
            .enumerate()
            .map(|(index, target)| match target {
                Some(_) => format!(r#"{{"POSITION":{}}}"#, displacement_accessors[index]),
                None => "{}".to_string(),
            })
            .collect();
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
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    /// A GLB holding one triangle whose material states `doubleSided` or not.
    fn sided_document(double_sided: bool) -> Vec<u8> {
        let mut bin = Vec::new();
        let mut views: Vec<String> = Vec::new();
        let mut add = |bin: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
            views.push(format!(
                r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
                bin.len(),
                bytes.len()
            ));
            bin.extend_from_slice(bytes);
            views.len() - 1
        };
        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let position_view = add(&mut bin, &positions, Some(34962));
        let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_view = add(&mut bin, &indices, Some(34963));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
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
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    #[test]
    fn a_double_sided_material_culls_nothing() {
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());

        let single =
            LitGltf::from_bytes(&sided_document(false)).expect("the single-sided document parses");
        assert_eq!(
            single
                .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0))
                .options
                .primitive
                .cull_mode,
            Some(wgpu::Face::Back),
            "a material that is not double-sided culls back faces"
        );

        let double = LitGltf::from_bytes(&sided_document(true)).expect("the sided document parses");
        assert_eq!(
            double
                .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0))
                .options
                .primitive
                .cull_mode,
            None,
            "a double-sided material culls nothing"
        );
    }

    #[test]
    fn culling_alone_distinguishes_the_two_sided_variants() {
        let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let single = LitGltf::from_bytes(&sided_document(false))
            .expect("the single-sided document parses")
            .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0));
        let double = LitGltf::from_bytes(&sided_document(true))
            .expect("the double-sided document parses")
            .pipeline_key(&device, MeshId::new(0), PrimitiveId::new(0));
        assert_ne!(
            single, double,
            "the two sides must be separate variants so each keeps its own pipeline"
        );
        assert_eq!(
            single.options.primitive.cull_mode,
            Some(wgpu::Face::Back),
            "a single-sided primitive culls its back faces"
        );
        assert_eq!(
            double.options.primitive.cull_mode, None,
            "a double-sided material culls nothing"
        );
        // Nothing but the cull mode sets the two apart. The options are
        // hashed, so the rewrite goes through `update`: the word follows the
        // changed field and the equality check stays sound.
        let mut normalized = single.options.clone();
        normalized
            .update(|options| options.primitive.cull_mode = double.options.primitive.cull_mode);
        assert_eq!(normalized, double.options);
    }

    #[test]
    fn a_morph_target_makes_the_mesh_morph() {
        let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[Some([0.0, 1.0, 0.0])]))
            .expect("the morphed document parses");
        let primitive = gltf.primitive(0, 0);
        assert_eq!(morph_target_count(&primitive), 1);
    }

    #[test]
    fn a_target_without_positions_does_not_make_the_mesh_morph() {
        // A normals-only target displaces nothing this loader reads, so the
        // mesh stays in the rigid path rather than drawing an empty morph.
        let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[None]))
            .expect("the morphed document parses");
        let primitive = gltf.primitive(0, 0);
        assert_eq!(morph_target_count(&primitive), 0);
        assert!(gltf.morph_weights(NodeId::new(0)).is_empty());
    }

    #[test]
    fn a_node_overrides_the_mesh_weights() {
        let gltf = LitGltf::from_bytes(&morphed_document(
            Some(&[0.25]),
            Some(&[0.75]),
            &[Some([0.0, 1.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.75]);

        let gltf = LitGltf::from_bytes(&morphed_document(
            Some(&[0.25]),
            None,
            &[Some([1.0, 0.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.25]);
    }

    #[test]
    fn morph_weights_are_padded_to_the_target_count() {
        // A document that states fewer weights than it has targets would make
        // the renderer reject the mesh, so the shortfall is padded with the
        // undeformed weight rather than passed on.
        let gltf = LitGltf::from_bytes(&morphed_document(
            Some(&[0.5]),
            None,
            &[Some([0.0, 1.0, 0.0]), Some([1.0, 0.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.5, 0.0]);

        // And one that states more is truncated, for the same reason.
        let gltf = LitGltf::from_bytes(&morphed_document(
            Some(&[0.5, 1.0]),
            None,
            &[Some([0.0, 1.0, 0.0])],
        ))
        .expect("the morphed document parses");
        assert_eq!(gltf.morph_weights(NodeId::new(0)), vec![0.5]);
    }

    #[test]
    fn a_mesh_without_targets_starts_undeformed() {
        let gltf = LitGltf::from_bytes(&morphed_document(None, None, &[]))
            .expect("the morphed document parses");
        assert!(gltf.morph_weights(NodeId::new(0)).is_empty());
        assert_eq!(morph_target_count(&gltf.primitive(0, 0)), 0);
    }

    #[test]
    fn a_rest_skin_pose_deforms_nothing() {
        // The joint sits at rest, so its world matrix is the inverse bind
        // matrix's inverse and every joint matrix comes out the identity: the
        // pose a document uploads at rest has to leave the mesh exactly where
        // it is.
        let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
            .expect("the skinned document parses");
        let pose = gltf.skin_pose(NodeId::new(0));
        assert_eq!(pose.matrices.len(), 1, "the skin has one joint");
        assert!(
            pose.matrices[0].abs_diff_eq(glam::Mat4::IDENTITY, 1e-6),
            "{}",
            pose.matrices[0]
        );
    }

    #[test]
    fn a_joint_away_from_its_bind_pose_moves_the_mesh() {
        // An inverse bind matrix records where a joint was *bound*, so a joint
        // node that sits somewhere else pulls the vertices it weights to
        // itself: the pose is the joint's world matrix against its bind
        // matrix, not the identity.
        let gltf = LitGltf::from_bytes(&skinned_document([3.0, -2.0, 1.0]))
            .expect("the skinned document parses");
        let pose = gltf.skin_pose(NodeId::new(0));
        assert!(
            pose.matrices[0].abs_diff_eq(
                glam::Mat4::from_translation(glam::Vec3::new(3.0, -2.0, 1.0)),
                1e-6
            ),
            "{}",
            pose.matrices[0]
        );
    }

    #[test]
    fn a_skinned_primitive_declares_its_joint_stream() {
        // The primitive carries JOINTS_0 and WEIGHTS_0, so the mesh is
        // uploaded with them and the variant a draw resolves reads them.
        let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
            .expect("the skinned document parses");
        let primitive = gltf.primitive(0, 0);
        assert!(primitive.get(&gltf::Semantic::Joints(0)).is_some());
        assert!(primitive.get(&gltf::Semantic::Weights(0)).is_some());
        assert!(!samples_base_color_texture(&primitive));
    }

    #[test]
    fn a_node_without_a_skin_has_no_pose() {
        let gltf = LitGltf::from_bytes(&skinned_document([0.0, 0.0, 0.0]))
            .expect("the skinned document parses");
        assert_eq!(gltf.skin_joint_count(NodeId::new(0)), Some(1));
        assert_eq!(gltf.skin_joint_count(NodeId::new(1)), None);
        assert!(gltf.skin_pose(NodeId::new(1)).matrices.is_empty());
    }

    /// 4-aligns `bin`, records a bufferView for `bytes` and returns its index.
    fn add_view(
    bin: &mut Vec<u8>,
    views: &mut Vec<String>,
    bytes: &[u8],
    target: Option<u32>,
    ) -> usize {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
    views.push(format!(
        r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
        bin.len(),
        bytes.len()
    ));
    bin.extend_from_slice(bytes);
    views.len() - 1
    }

    /// Wraps a JSON document and its binary chunk into a GLB.
    fn glb(json: String, bin: &[u8]) -> Vec<u8> {
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut glb = Vec::new();
    glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    glb.extend_from_slice(bin);
    glb
    }

    /// The `min` and `max` a glTF accessor needs, as a JSON fragment.
    fn bounds(times: &[f32]) -> String {
    let min = times.first().copied().unwrap_or(0.0);
    let max = times.last().copied().unwrap_or(0.0);
    format!(r#", "min": [{min}], "max": [{max}]"#)
    }

    /// Adds the triangle both an animated mesh and a static one draw, and
    /// pushes its position and index accessors, returning the index accessor.
    fn add_triangle(
    bin: &mut Vec<u8>,
    views: &mut Vec<String>,
    accessors: &mut Vec<String>,
    ) -> usize {
    let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let position = add_view(bin, views, &positions, Some(34962));
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let index_accessor = accessors.len();
    let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
    let index = add_view(bin, views, &indices, Some(34963));
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
    fn parented_animated_document(
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
    fn animated_skinned_document(times: &[f32], translations: &[[f32; 3]]) -> Vec<u8> {
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

    let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let position = add_view(&mut bin, &mut views, &positions, Some(34962));
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let joints: Vec<u8> = (0..3)
        .flat_map(|_| [0u16, 0, 0, 0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let joint = add_view(&mut bin, &mut views, &joints, Some(34962));
    accessors.push(format!(
        r#"{{ "bufferView": {joint}, "componentType": 5123, "count": 3, "type": "VEC4" }}"#
    ));
    let weights: Vec<u8> = (0..3)
        .flat_map(|_| [1.0f32, 0.0, 0.0, 0.0])
        .flat_map(f32::to_le_bytes)
        .collect();
    let weight = add_view(&mut bin, &mut views, &weights, Some(34962));
    accessors.push(format!(
        r#"{{ "bufferView": {weight}, "componentType": 5126, "count": 3, "type": "VEC4" }}"#
    ));
    let identity: Vec<u8> = [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
    .iter()
    .flat_map(|f| f.to_le_bytes())
    .collect();
    let bind = add_view(&mut bin, &mut views, &identity, None);
    accessors.push(format!(
        r#"{{ "bufferView": {bind}, "componentType": 5126, "count": 1, "type": "MAT4" }}"#
    ));
    let index_accessor = accessors.len();
    let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
    let index = add_view(&mut bin, &mut views, &indices, Some(34963));
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
    fn animated_morphed_document(times: &[f32], weights: &[f32]) -> Vec<u8> {
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

    let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let position = add_view(&mut bin, &mut views, &positions, Some(34962));
    accessors.push(format!(
        r#"{{ "bufferView": {position}, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }}"#
    ));
    let mut deltas = [0.0f32; 9];
    deltas[4] = 1.0;
    let delta_bytes: Vec<u8> = deltas.iter().flat_map(|f| f.to_le_bytes()).collect();
    let delta = add_view(&mut bin, &mut views, &delta_bytes, None);
    accessors.push(format!(
        r#"{{ "bufferView": {delta}, "componentType": 5126, "count": 3, "type": "VEC3" }}"#
    ));
    let index_accessor = accessors.len();
    let indices: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
    let index = add_view(&mut bin, &mut views, &indices, Some(34963));
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

    #[test]
    fn an_animation_reports_its_name_and_duration() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 1.5, 4.0],
        &[[0.0; 3]; 3],
        &[[0.0, 0.0, 0.0, 1.0]; 3],
    ))
    .expect("the animated document parses");
    assert_eq!(gltf.animation_count(), 1);
    assert_eq!(gltf.animation_name(AnimationId::new(0)), Some("walk"));
    assert!((gltf.animation_duration(AnimationId::new(0)) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn apply_animation_moves_a_node_and_carries_its_child() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "LINEAR",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[
            [0.0, 0.0, 0.0, 1.0],
            [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        ],
    ))
    .expect("the animated document parses");
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let child = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let nodes = [
        GltfNode {
            node: NodeId::new(0),
            entities: vec![parent],
            pose: None,
            weights: None,
        },
        GltfNode {
            node: NodeId::new(1),
            entities: vec![child],
            pose: None,
            weights: None,
        },
    ];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &nodes);
    // The parent's own channel moves it...
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Transform>(parent)
            .expect("the parent carries a transform")
            .get()
            .translation,
        glam::Vec3::new(2.0, 0.0, 0.0)
    );
    let mut transform_row = world
        .root_scope()
        .query_one::<&Transform>(child)
        .expect("the child carries a transform");

    // ...and the child, which no translation channel names, inherits it
    // along with the quarter turn its own channel reaches at the midpoint.
    let child = transform_row.get();
    assert_eq!(child.translation, glam::Vec3::new(2.0, 0.0, 0.0));
    let quarter = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
    assert!(child.rotation.dot(quarter).abs() > 1.0 - 1e-5, "{child:?}");
    }

    #[test]
    fn apply_animation_holds_a_step_keyframe() {
    let gltf = LitGltf::from_bytes(&parented_animated_document(
        "STEP",
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
        &[[0.0, 0.0, 0.0, 1.0]; 2],
    ))
    .expect("the animated document parses");
    let mut world = World::new();
    let parent = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![parent],
        pose: None,
        weights: None,
    }];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.999, &nodes);
    assert_eq!(
        world
            .root_scope()
            .query_one::<&Transform>(parent)
            .expect("the parent carries a transform")
            .get()
            .translation,
        glam::Vec3::ZERO
    );
    }

    #[test]
    fn apply_animation_rebuilds_a_skin_pose() {
    let gltf = LitGltf::from_bytes(&animated_skinned_document(
        &[0.0, 2.0],
        &[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]],
    ))
    .expect("the animated skinned document parses");
    let mut world = World::new();
    let mesh = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let pose = world.root_scope_mut().spawn(bundle![SkinPose::default()]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![mesh],
        pose: Some(pose),
        weights: None,
    }];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &nodes);
    let mut skin_pose_row = world
        .root_scope()
        .query_one::<&SkinPose>(pose)
        .expect("the pose entity carries a pose");

    let pose = skin_pose_row.get();
    assert!(
        pose.matrices[0].abs_diff_eq(
            glam::Mat4::from_translation(glam::Vec3::new(2.0, 0.0, 0.0)),
            1e-6
        ),
        "{}",
        pose.matrices[0]
    );
    }

    #[test]
    fn apply_animation_writes_morph_weights() {
    let gltf = LitGltf::from_bytes(&animated_morphed_document(&[0.0, 2.0], &[0.0, 2.0]))
        .expect("the animated morphed document parses");
    let mut world = World::new();
    let mesh = world.root_scope_mut().spawn(bundle![Transform::default()]);
    let weights = world
        .root_scope_mut()
        .spawn(bundle![MorphWeights { weights: vec![0.0] }]);
    let nodes = [GltfNode {
        node: NodeId::new(0),
        entities: vec![mesh],
        pose: None,
        weights: Some(weights),
    }];
    gltf.apply_animation(&world.root_scope(), AnimationId::new(0), 1.0, &nodes);
    let mut morph_weights_row = world
        .root_scope()
        .query_one::<&MorphWeights>(weights)
        .expect("the weights entity carries weights");

    let weights = morph_weights_row.get();
    assert_eq!(weights.weights, vec![1.0]);
    }
