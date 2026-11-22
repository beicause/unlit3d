//! Assembling the lit draw one glTF primitive names.
//!
//! A model node draws its mesh through [`LitMeshBundle`], the lit family's
//! spawn shape: the uploaded mesh, the pipeline its key names, the material's
//! bind group and the pose or weights it binds to. This module turns one
//! primitive of a document into that draw, which is what
//! [`LitGltf::to_model`](super::LitGltf::to_model) hands to a model asset.

use super::material::{alpha_cutoff, base_color_uv_set};
use super::{GltfMaterial, GltfMesh, GltfResources, LitGltf, MeshId, PrimitiveId};
use crate::components::{GpuRenderPipeline, ZSortedDrawing};
use crate::lit::{InstanceColor, InstanceCutoff, LitMeshPrimitive};

impl LitGltf {
    pub(super) fn draw_primitive(
        &self,
        node: usize,
        mesh: &gltf::Mesh<'_>,
        primitive: &gltf::Primitive<'_>,
        resources: &GltfResources,
    ) -> Result<LitMeshPrimitive, super::Error> {
        let handle = self
            .uploaded_primitive(
                resources,
                MeshId::new(mesh.index()),
                PrimitiveId::new(primitive.index()),
            )
            .ok_or(super::Error::MeshNotInserted { node })?;
        let material = if base_color_uv_set(primitive).is_some() {
            Some(
                self.primitive_material(mesh, primitive, &resources.materials)?
                    .bind_group
                    .clone(),
            )
        } else {
            None
        };
        let tint = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_factor()
            .into();
        Ok(LitMeshPrimitive {
            mesh: handle.mesh.clone(),
            pipeline: GpuRenderPipeline::new(handle.key.clone()),
            color: InstanceColor::new(tint),
            cutoff: alpha_cutoff(&primitive.material()).map(InstanceCutoff::new),
            hull_width: None,
            material,
            z_sorted: handle.key.options.blend.is_some().then_some(ZSortedDrawing),
        })
    }

    /// The uploaded handle of one primitive of `mesh`, or `None` when the
    /// resources do not hold it.
    ///
    /// The slots a document uploads into are document-mesh-major, so the
    /// handle's own position follows from the mesh and primitive indices.
    /// Resources a caller assembled itself may hold a subset of the document's
    /// meshes and shift those positions, so a slot that names some other
    /// primitive falls back to a scan; the ids are what decides either way.
    fn uploaded_primitive<'r>(
        &self,
        resources: &'r GltfResources,
        mesh: MeshId,
        primitive: PrimitiveId,
    ) -> Option<&'r GltfMesh> {
        let named =
            |handle: &&GltfMesh| handle.mesh_index == mesh && handle.primitive_index == primitive;
        let slot = self
            .first_primitive
            .get(mesh.index())
            .and_then(|start| start.checked_add(primitive.index()));
        match slot.and_then(|slot| resources.meshes.get(slot)) {
            Some(handle) if named(&handle) => Some(handle),
            _ => resources.meshes.iter().find(named),
        }
    }

    /// The inserted material a primitive draws with.
    ///
    /// A primitive without a material index, or whose material is `None` in
    /// `materials` (no base-color texture), draws untextured — but a primitive
    /// that samples its material's base-color texture needs the material
    /// inserted, which [`Self::to_model`] reports as
    /// [`Error::MaterialNotInserted`](super::Error::MaterialNotInserted).
    fn primitive_material<'m>(
        &self,
        mesh: &gltf::Mesh<'_>,
        primitive: &gltf::Primitive<'_>,
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
