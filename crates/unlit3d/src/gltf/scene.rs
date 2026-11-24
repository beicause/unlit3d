//! Node-hierarchy helpers: world matrices built from a document
//! `Transform` trees.

use crate::components::Transform;

/// The parent of every node, indexed by node index; `None` at a root.
///
/// glTF nodes have no parent handle, so the table is built from each node's
/// children list, which is the document's own statement of the hierarchy.
pub(super) fn node_parents(document: &gltf::Document) -> Vec<Option<usize>> {
    let mut parents = vec![None; document.nodes().len()];
    for node in document.nodes() {
        for child in node.children() {
            parents[child.index()] = Some(node.index());
        }
    }
    parents
}

/// The local-space transform of every node, indexed by node index.
///
/// Preserve authored TRS so a channel can replace one property independently.
/// Matrix-form nodes have no animated TRS properties; for their conversion
/// to the model representation, see [`Transform::from_matrix`].
pub(super) fn node_locals(document: &gltf::Document) -> Vec<Transform> {
    document
        .nodes()
        .map(|node| match node.transform() {
            // Preserve the axes and signs of the authored scale: replacing
            // only one TRS property must not reinterpret the other two.
            gltf::scene::Transform::Decomposed {
                translation,
                rotation,
                scale,
            } => Transform {
                translation: translation.into(),
                rotation: glam::Quat::from_array(rotation),
                scale: scale.into(),
            },
            gltf::scene::Transform::Matrix { matrix } => Transform::from_matrix(
                glam::Affine3A::from_mat4(glam::Mat4::from_cols_array_2d(&matrix)),
            ),
        })
        .collect()
}

/// The world-space matrix of every node, indexed by node index, walked in the
/// parent-first order [`node_order`] returns.
///
/// World matrices are accumulated root-first so a child always sees its
/// parent's matrix before its own is computed.
pub(super) fn node_world_matrices(
    document: &gltf::Document,
    parents: &[Option<usize>],
    order: &[usize],
) -> Vec<glam::Mat4> {
    let mut world = vec![glam::Mat4::IDENTITY; document.nodes().len()];
    for &root in order {
        let node_data = document.nodes().nth(root).expect("node index in bounds");
        let local = glam::Mat4::from_cols_array_2d(&node_data.transform().matrix());
        world[root] = match parents[root] {
            Some(parent) => world[parent] * local,
            None => local,
        };
    }
    world
}

/// The children of every node, indexed by node index.
///
/// glTF states a hierarchy as children lists, and a node's descendants are what
/// an animation has to revisit when it moves the node: a document's shape does
/// not change, so the table is built once and a frame walks it.
pub(super) fn node_children(document: &gltf::Document) -> Vec<Vec<usize>> {
    document
        .nodes()
        .map(|node| node.children().map(|child| child.index()).collect())
        .collect()
}

/// The node indices in an order where every node follows its parent, read out
/// of `children`.
///
/// The order is the same whether a node's own local matrix is the document's
/// or one an animation replaced it with, so recomposing from it can reuse the
/// cached `parents` table. It is also the order the world matrices have to be
/// accumulated in, so a document keeps one copy rather than walking the tree
/// per frame.
pub(super) fn node_order(children: &[Vec<usize>], parents: &[Option<usize>]) -> Vec<usize> {
    let mut order = Vec::with_capacity(children.len());
    let mut stack: Vec<usize> = (0..children.len())
        .filter(|&index| parents[index].is_none())
        .collect();
    while let Some(node) = stack.pop() {
        order.push(node);
        stack.extend(children[node].iter().copied());
    }
    order
}

/// How many morph targets each mesh declares, indexed by mesh index.
///
/// The count is the length of a primitive's `targets` array, not the number of
/// targets this loader reads displacements for: a target that displaces only
/// normals still owns a slot, and counting it out would shift every later
/// target's weight.
///
/// The specification requires every primitive of one mesh to declare the same
/// number of targets, so a document that disagrees with itself is refused here
/// rather than read differently by the vertex streams and the weights channel.
pub(super) fn mesh_morph_targets(document: &gltf::Document) -> Result<Vec<usize>, super::Error> {
    document
        .meshes()
        .map(|mesh| {
            let mut primitives = mesh.primitives();
            let Some(first) = primitives.next() else {
                return Ok(0);
            };
            let expected = super::material::morph_target_count(&first);
            for primitive in primitives {
                let targets = super::material::morph_target_count(&primitive);
                if targets != expected {
                    return Err(super::Error::MorphTargetCountMismatch {
                        mesh: mesh.index(),
                        primitive: primitive.index(),
                        targets,
                        expected,
                    });
                }
            }
            Ok(expected)
        })
        .collect()
}

/// How many morph targets each node's mesh declares, indexed by node index.
///
/// A weights channel states one value per target per keyframe, so the count is
/// what slices a keyframe's values out of the channel, and a frame needs it to
/// read a node's weights without building the node's weight vector. A node with
/// no mesh counts as zero. [`mesh_morph_targets`] is the only place the count
/// is read from the document.
pub(super) fn nodes_morph_targets(document: &gltf::Document, meshes: &[usize]) -> Vec<usize> {
    document
        .nodes()
        .map(|node| node.mesh().map_or(0, |mesh| meshes[mesh.index()]))
        .collect()
}

/// A mesh's primitives are contiguous in that list, so a primitive's slot is
/// its mesh's entry plus its own index. The table has one entry per mesh plus
/// the total, which is what makes the last mesh's length readable too.
pub(super) fn first_primitives(document: &gltf::Document) -> Vec<usize> {
    let mut starts = Vec::with_capacity(document.meshes().len() + 1);
    let mut count = 0;
    for mesh in document.meshes() {
        starts.push(count);
        count += mesh.primitives().len();
    }
    starts.push(count);
    starts
}
