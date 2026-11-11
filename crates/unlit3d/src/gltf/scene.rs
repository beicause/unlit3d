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
/// A matrix-form node transform is composed back into translation, rotation
/// and scale so that an animation can address a single one of them; the
/// decomposition is lossy for shear, which a `Transform` cannot carry.
pub(super) fn node_locals(document: &gltf::Document) -> Vec<Transform> {
    document
        .nodes()
        .map(|node| decompose(glam::Mat4::from_cols_array_2d(&node.transform().matrix())))
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

/// How many morph targets each node's mesh displaces, indexed by node index.
///
/// A weights channel states one value per target per keyframe, so the count is
/// what slices a keyframe's values out of the channel; a node with no mesh, or
/// a mesh whose targets displace nothing, counts as zero.
pub(super) fn nodes_morph_targets(document: &gltf::Document) -> Vec<usize> {
    document
        .nodes()
        .map(|node| {
            node.mesh().map_or(0, |mesh| {
                mesh.primitives()
                    .map(|primitive| super::material::morph_target_count(&primitive))
                    .max()
                    .unwrap_or(0)
            })
        })
        .collect()
}

/// Split a world-space matrix into the parts an entity's
/// [`Transform`] stores: translation, rotation and scale.
///
/// The decomposition is lossy for sheared matrices, and an axis scaled to
/// zero keeps an arbitrary rotation (there is no rotation to preserve along a
/// collapsed axis). Both are fine for an unlit renderer.
pub(super) fn decompose(matrix: glam::Mat4) -> Transform {
    let translation = matrix.w_axis.truncate();
    let x = matrix.x_axis.truncate();
    let y = matrix.y_axis.truncate();
    let z = matrix.z_axis.truncate();
    let (sx, sy, sz) = (x.length(), y.length(), z.length());
    let rotation = glam::Quat::from_mat3(&glam::Mat3::from_cols(
        axis_or_identity(x, sx),
        axis_or_identity(y, sy),
        axis_or_identity(z, sz),
    ));
    Transform {
        translation,
        rotation,
        scale: glam::Vec3::new(sx, sy, sz),
    }
}

/// A unit-length axis, keeping its direction; identity when collapsed.
fn axis_or_identity(axis: glam::Vec3, length: f32) -> glam::Vec3 {
    if length > f32::EPSILON {
        axis / length
    } else {
        glam::Vec3::X // direction is meaningless along a zero-length axis
    }
}
