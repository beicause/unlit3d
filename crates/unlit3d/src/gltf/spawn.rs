//! Spawning entities that draw a document's nodes.
//!
//! Every entity a spawn produces is a bundle assembled from the uploaded
//! handles: the mesh, the pipeline its key names, the material's bind group and
//! the per-node pose or weights it binds to. The returned [`GltfNode`] names
//! them, so an animation can write into exactly the entities a node draws.

use super::material::{alpha_cutoff, samples_base_color_texture};
use super::{GltfMaterial, GltfNode, GltfResources, LitGltf, MeshId, NodeId, PrimitiveId};
use crate::components::{
    GpuRenderPipeline, MorphBinding, MorphWeights, SkinBinding, ZSortedDrawing,
};
use crate::lit::{InstanceColor, InstanceCutoff};
use unlit_ecs::bundle;
use unlit_ecs::{DynamicBundle, ScopeMut, prelude::Entity};

impl LitGltf {
    /// Spawn the entities that draw `node`'s mesh into `world`.
    ///
    /// Returns a [`GltfNode`] naming one entity per primitive of the node's
    /// mesh — each carrying the
    /// node's world-space [`Transform`](crate::components::Transform), the
    /// uploaded [`GpuMesh`](crate::components::GpuMesh), the
    /// pipeline for the mesh's key and an [`InstanceColor`] tinted with the
    /// material's base-color factor. A primitive that samples its material's
    /// base-color texture also carries the matching
    /// [`GpuMaterial`](crate::components::GpuMaterial). A blended
    /// primitive ([`gltf::material::AlphaMode::Blend`]) also carries
    /// [`ZSortedDrawing`], so it is drawn after the opaque geometry and
    /// composited back-to-front, and its pipeline writes no depth: each of its
    /// fragments covers a surface only partly, and the ones behind it are
    /// still to be drawn. A skinned primitive (one whose mesh carries a
    /// joint stream) carries a [`SkinBinding`] to the node's
    /// [`SkinPose`](crate::components::SkinPose), which is
    /// spawned on an entity of its own so several meshes can share it — see
    /// [`Self::skin_pose`]. Children are not spawned; use
    /// [`Self::spawn_default_scene`] for the whole scene.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MeshNotInserted`](super::Error::MeshNotInserted) when
    /// the node's mesh (or a mesh it shares primitives with) was not inserted
    /// into `resources`, and
    /// [`Error::MaterialNotInserted`](super::Error::MaterialNotInserted) when a
    /// primitive's material is `None` there while it samples a base-color
    /// texture.
    pub fn spawn_node(
        &self,
        world: &mut ScopeMut<'_>,
        node: NodeId,
        resources: &GltfResources,
    ) -> Result<GltfNode, super::Error> {
        let index = node.index();
        let transform = self.node_transforms[index].clone();
        let node = self.node(index);
        let mut entities = Vec::new();
        // One pose entity per skinned node, shared by every primitive it
        // draws: the joints are the node's skin, and two primitives of one
        // mesh deform together. The morph weights get an entity of their own
        // for the same reason — they are the node's, not the primitive's.
        let mut pose: Option<Entity> = None;
        let mut morphs: Option<Entity> = None;
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let mesh_handle = resources
                    .meshes
                    .iter()
                    .find(|handle| {
                        handle.mesh_index == MeshId::new(mesh.index())
                            && handle.primitive_index == PrimitiveId::new(primitive.index())
                    })
                    .ok_or(super::Error::MeshNotInserted { node: index })?;
                // The base-color factor tints the instance whether or not the
                // material samples a texture; a primitive naming no material
                // falls back to the glTF default, opaque white.
                let tint: glam::Vec4 = primitive
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_factor()
                    .into();
                // The `alphaMode: MASK` cutoff the shader compares against is
                // read before the primitive is handed to the material lookup,
                // which takes it by value.
                let cutoff = alpha_cutoff(&primitive.material());
                let material = match samples_base_color_texture(&primitive) {
                    true => {
                        Some(self.primitive_material(&mesh, primitive, &resources.materials)?)
                    }
                    false => None,
                };
                let z_sorted = mesh_handle.key.options.blend.is_some();
                let skinned = mesh_handle.mesh.skinned;
                let binding = skinned.then(|| {
                    let pose = *pose.get_or_insert_with(|| {
                        world.spawn_named(
                            "gltf skin pose",
                            bundle![self.skin_pose(NodeId::new(index))],
                        )
                    });
                    SkinBinding { pose }
                });
                let morphed = mesh_handle.mesh.morph_targets > 0;
                let morph_binding = morphed.then(|| {
                    let weights = *morphs.get_or_insert_with(|| {
                        world.spawn_named(
                            "gltf morph weights",
                            bundle![MorphWeights {
                                weights: self.morph_weights(NodeId::new(index))
                            }],
                        )
                    });
                    MorphBinding { weights }
                });
                // The components a primitive carries vary by material,
                // skin and blend mode, so the bundle is assembled rather than
                // written as a tuple: an absent material is a component left
                // out, not a `None` the renderer would have to look through.
                let mut bundle = DynamicBundle::new().named("gltf primitive");
                bundle.push_component(transform.clone());
                bundle.push_component(mesh_handle.mesh.clone());
                bundle.push_component(GpuRenderPipeline::new(mesh_handle.key.clone()));
                bundle.push_component(InstanceColor::new(tint));
                // The cutoff is per-instance state, so the entity carries it
                // and the shader reads it out of the instance stream.
                if let Some(cutoff) = cutoff {
                    bundle.push_component(InstanceCutoff::new(cutoff));
                }
                if let Some(material) = material {
                    bundle.push_component(material.bind_group.clone());
                }
                if let Some(binding) = binding {
                    bundle.push_component(binding);
                }
                if let Some(binding) = morph_binding {
                    bundle.push_component(binding);
                }
                if z_sorted {
                    bundle.push_component(ZSortedDrawing);
                }
                entities.push(world.spawn(bundle));
            }
        }
        Ok(GltfNode {
            node: NodeId::new(index),
            entities,
            pose,
            weights: morphs,
        })
    }

    /// Spawn every node reachable from the document's default scene.
    ///
    /// Returns one [`GltfNode`] per visited node, in the order the walk
    /// reaches them; a node with no mesh has none of its own but is still
    /// returned, so the list maps a document node to its entities. Nodes are
    /// visited root-first and each subtree fully; every node with a mesh
    /// contributes its primitives exactly where its world-space transform sits
    /// in the hierarchy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoDefaultScene`](super::Error::NoDefaultScene) when the
    /// document declares no default scene, or whatever
    /// [`Self::spawn_node`] returns for any visited node.
    pub fn spawn_default_scene(
        &self,
        world: &mut ScopeMut<'_>,
        resources: &GltfResources,
    ) -> Result<Vec<GltfNode>, super::Error> {
        let scene = self
            .document
            .default_scene()
            .ok_or(super::Error::NoDefaultScene)?;
        let mut nodes = Vec::new();
        let mut stack: Vec<NodeId> = scene
            .nodes()
            .map(|node| NodeId::new(node.index()))
            .collect();
        while let Some(node) = stack.pop() {
            nodes.push(self.spawn_node(world, node, resources)?);
            stack.extend(
                self.node(node.index())
                    .children()
                    .map(|child| NodeId::new(child.index())),
            );
        }
        Ok(nodes)
    }

    /// The inserted material a primitive draws with.
    ///
    /// A primitive without a material index, or whose material is `None` in
    /// `materials` (no base-color texture), draws untextured — but a primitive
    /// that samples its material's base-color texture needs the material
    /// inserted, which [`Self::spawn_node`] reports as
    /// [`Error::MaterialNotInserted`](super::Error::MaterialNotInserted).
    fn primitive_material<'m>(
        &self,
        mesh: &gltf::Mesh<'_>,
        primitive: gltf::Primitive<'_>,
        materials: &'m [Option<GltfMaterial>],
    ) -> Result<&'m GltfMaterial, super::Error> {
        let missing = || super::Error::MaterialNotInserted {
            mesh: mesh.index(),
            primitive: primitive.index(),
        };
        match primitive.material().index() {
            Some(index) => materials
                .get(index)
                .and_then(Option::as_ref)
                .ok_or_else(missing),
            None => Err(missing()),
        }
    }
}
