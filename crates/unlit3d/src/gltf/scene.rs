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
/// and scale so that an animation can address a single one of them; see
/// [`Transform::from_matrix`] for what that costs.
pub(super) fn node_locals(document: &gltf::Document) -> Vec<Transform> {
    document
        .nodes()
        .map(|node| {
            Transform::from_matrix(glam::Affine3A::from_mat4(glam::Mat4::from_cols_array_2d(
                &node.transform().matrix(),
            )))
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

/// How many morph targets each node's mesh declares, indexed by node index.
///
/// A weights channel states one value per target per keyframe, so the count is
/// what slices a keyframe's values out of the channel. It is the length of the
/// primitive's `targets` array, not the number of targets this loader reads
/// displacements for: a target that displaces only normals still owns a slot,
/// and counting it out would shift every later target's weight. A node with no
/// mesh counts as zero. The specification requires every primitive of one mesh
/// to declare the same number of targets, so the widest primitive's count is
/// the mesh's.
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

/// The node whose global transform decides the winding of each node's skinned
/// primitives, indexed by node index; `None` for a node without a skin.
///
/// The specification defines it as the skin's `skeleton` node when the document
/// states one and as the lowest common ancestor of the skin's joint nodes
/// otherwise. A skin with no joints gets itself, whose transform is the
/// identity, so its primitives wind as the rest of the document does.
pub(super) fn skin_reference_nodes(
    document: &gltf::Document,
    parents: &[Option<usize>],
) -> Vec<Option<usize>> {
    let depths = node_depths(parents);
    document
        .nodes()
        .map(|node| {
            let skin = node.skin()?;
            if let Some(skeleton) = skin.skeleton() {
                return Some(skeleton.index());
            }
            let mut joints = skin.joints();
            let first = joints.next()?.index();
            Some(joints.fold(first, |ancestor, joint| {
                lowest_common_ancestor(ancestor, joint.index(), parents, &depths)
            }))
        })
        .collect()
}

/// How far every node sits below its root, indexed by node index.
fn node_depths(parents: &[Option<usize>]) -> Vec<usize> {
    (0..parents.len())
        .map(|node| {
            let mut depth = 0;
            let mut current = node;
            while let Some(parent) = parents[current] {
                current = parent;
                depth += 1;
            }
            depth
        })
        .collect()
}

/// The deepest node both `a` and `b` descend from.
///
/// The climb is bounded by the two depths, so a children list that states a
/// cycle — which nothing validates against — terminates at the highest node it
/// reaches instead of looping.
fn lowest_common_ancestor(
    mut a: usize,
    mut b: usize,
    parents: &[Option<usize>],
    depths: &[usize],
) -> usize {
    // Everything below the shallower node is out of reach of the other, so the
    // climb never needs more steps than that depth.
    let bound = depths[a].min(depths[b]);
    let mut steps = depths[a].saturating_sub(bound);
    while steps > 0 {
        a = parents[a].unwrap_or(a);
        steps -= 1;
    }
    steps = depths[b].saturating_sub(bound);
    while steps > 0 {
        b = parents[b].unwrap_or(b);
        steps -= 1;
    }
    let mut steps = bound;
    while a != b && steps > 0 {
        a = parents[a].unwrap_or(a);
        b = parents[b].unwrap_or(b);
        steps -= 1;
    }
    a
}
/// Where each mesh's first primitive sits in a document-mesh-major primitive
/// list, indexed by mesh index; one past the last primitive for the last mesh.
///
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
