//! Assembling the lit draw one glTF primitive names.
//!
//! A model node draws its mesh through
//! [`LitMeshBundle`](crate::lit::LitMeshBundle), the lit family's spawn shape:
//! the uploaded mesh, the pipeline its key names, the material's bind group and
//! the pose or weights it binds to. This module turns one primitive of a
//! document into that draw, which is what
//! [`LitGltf::to_model`](super::LitGltf::to_model) hands to a model asset.

use super::material::{alpha_cutoff, base_color_uv_set};
use super::{GltfMaterial, GltfResources, LitGltf, MaterialId, MeshId, PrimitiveId};
use crate::components::{GpuRenderPipeline, ZSortedDrawing};
use crate::lit::{InstanceColor, InstanceCutoff, LitMeshPrimitive};

impl LitGltf {
    pub(super) fn draw_primitive(
        &self,
        node: super::NodeId,
        mesh: &gltf::Mesh<'_>,
        primitive: &gltf::Primitive<'_>,
        resources: &GltfResources,
    ) -> Result<LitMeshPrimitive, super::Error> {
        let mesh_id = MeshId::new(mesh.index());
        let primitive_id = PrimitiveId::new(primitive.index());
        let handle = resources
            .mesh(mesh_id, primitive_id)
            .ok_or(super::Error::MeshNotInserted { node })?;
        let material = if base_color_uv_set(primitive).is_some() {
            Some(
                self.primitive_material(mesh_id, primitive_id, primitive, resources)?
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

    /// The inserted material a primitive draws with.
    ///
    /// A primitive that names no material draws untextured. One that samples
    /// its material's base-color texture needs that material inserted — the
    /// resources holding no entry for it is
    /// [`Error::MaterialNotInserted`](super::Error::MaterialNotInserted), and an
    /// entry uploaded without a base-color texture is
    /// [`Error::MaterialWithoutTexture`](super::Error::MaterialWithoutTexture).
    /// Both are reported rather than drawn untextured, because a primitive the
    /// document describes as textured would otherwise lose its texture
    /// silently.
    fn primitive_material<'r>(
        &self,
        mesh: MeshId,
        primitive: PrimitiveId,
        data: &gltf::Primitive<'_>,
        resources: &'r GltfResources,
    ) -> Result<&'r GltfMaterial, super::Error> {
        let Some(index) = data.material().index() else {
            return Err(super::Error::MaterialNotInserted { mesh, primitive });
        };
        let material = MaterialId::new(index);
        resources
            .material(material)
            .ok_or(if resources.holds_material(material) {
                super::Error::MaterialWithoutTexture { mesh, primitive }
            } else {
                super::Error::MaterialNotInserted { mesh, primitive }
            })
    }
}
