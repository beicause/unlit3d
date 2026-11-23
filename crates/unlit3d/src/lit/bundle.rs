//! The bundle one draw of the lit family is spawned with.

use crate::components::{
    GpuMaterial, GpuMesh, GpuRenderPipeline, MorphBinding, SkinBinding, Transform, ZSortedDrawing,
};
use crate::lit::{InstanceColor, InstanceCutoff, InstanceHullWidth};
use unlit_ecs::{Bundle, DynamicBundle};

/// The components one draw of the lit family carries, as one spawnable bundle.
///
/// A draw of the lit family names a [`GpuMesh`], the [`GpuRenderPipeline`]
/// its key specializes, and whatever per-instance and binding state that draw
/// resolves: a tint, an alpha cutoff, a hull width, a material's bind group, a
/// skin pose or morph weights to deform by, and a [`ZSortedDrawing`] mark for
/// blended draws. Which of those a draw needs varies with the mesh and the
/// material, so the bundle carries them as options and leaves the absent ones
/// out of the spawn rather than handing the renderer `None`s to look through
/// — `LitInstance` reads the per-instance state with world queries that fall
/// back to white, zero and no binding of their own.
///
/// The built-in glTF module spawns this same bundle for every primitive it
/// draws, and a scene of the caller's own spawns it for hand-built meshes, so
/// both paths assemble one shape of draw.
///
/// # Examples
///
/// ```
/// # use unlit3d::prelude::*;
/// # fn spawn(world: &mut unlit_ecs::World, mesh: GpuMesh, pipeline: GpuRenderPipeline) {
/// world.root_scope_mut().spawn(LitMeshBundle {
///     transform: Transform::default(),
///     primitive: LitMeshPrimitive::new(mesh, pipeline),
///     skin: None,
///     morph: None,
/// });
/// # }
/// ```
pub struct LitMeshBundle {
    /// Where the mesh sits when it draws.
    pub transform: Transform,
    /// The geometry, pipeline and material to draw.
    pub primitive: LitMeshPrimitive,
    /// The skin pose a skinned draw deforms by; `None` draws undeformed.
    pub skin: Option<SkinBinding>,
    /// The morph weights a morphed draw deforms by; `None` draws the base
    /// mesh.
    pub morph: Option<MorphBinding>,
}

/// Reusable draw data, independent of placement and deformation entity handles.
#[derive(Clone, Debug)]
pub struct LitMeshPrimitive {
    /// The mesh this entity draws.
    pub mesh: GpuMesh,
    /// The pipeline the mesh's key specializes.
    pub pipeline: GpuRenderPipeline,
    /// The per-instance tint the fragment multiplies its base colour by.
    pub color: InstanceColor,
    /// The alpha cutoff a masked pipeline compares against; `None` discards
    /// nothing.
    pub cutoff: Option<InstanceCutoff>,
    /// The distance an expanding pipeline pushes vertices out along their
    /// normal, which is what an inverted-hull outline draws behind the mesh;
    /// `None` draws the mesh in place.
    pub hull_width: Option<InstanceHullWidth>,
    /// The material bind group a textured draw samples; `None` draws the
    /// instance tint alone.
    pub material: Option<GpuMaterial>,
    /// The mark that draws this entity after the opaque geometry,
    /// back-to-front among its own kind; `None` draws it in the opaque pass.
    pub z_sorted: Option<ZSortedDrawing>,
}

impl LitMeshPrimitive {
    /// A primitive that draws `mesh` through `pipeline` with the default
    /// white tint and no cutoff, hull, material or z-sorting.
    pub fn new(mesh: GpuMesh, pipeline: GpuRenderPipeline) -> Self {
        Self {
            mesh,
            pipeline,
            color: InstanceColor::default(),
            cutoff: None,
            hull_width: None,
            material: None,
            z_sorted: None,
        }
    }
}

impl Bundle for LitMeshBundle {
    fn into_bundle(self) -> DynamicBundle {
        let mut bundle = DynamicBundle::new();
        bundle.push_component(self.transform);
        bundle.push_component(self.primitive.mesh);
        bundle.push_component(self.primitive.pipeline);
        bundle.push_component(self.primitive.color);
        if let Some(cutoff) = self.primitive.cutoff {
            bundle.push_component(cutoff);
        }
        if let Some(hull_width) = self.primitive.hull_width {
            bundle.push_component(hull_width);
        }
        if let Some(material) = self.primitive.material {
            bundle.push_component(material);
        }
        if let Some(skin) = self.skin {
            bundle.push_component(skin);
        }
        if let Some(morph) = self.morph {
            bundle.push_component(morph);
        }
        if let Some(z_sorted) = self.primitive.z_sorted {
            bundle.push_component(z_sorted);
        }
        bundle
    }
}
