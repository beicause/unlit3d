//! Owned vertex streams, however a glTF primitive supplied them.
//!
//! A lit mesh is uploaded from borrowed slices, so the decoded data has to
//! outlive the `allocate_lit_mesh` call. Both the ordinary accessor reader
//! and the Draco decoder fill this one struct, and [`MeshStreams::desc`] lends
//! it to the allocator as a [`LitMeshDesc`].

use super::material::{morph_target_count, samples_base_color_texture};
use crate::lit::LitMeshDesc;
use crate::mesh::MorphDeltas;

/// Every vertex stream a glTF primitive can upload.
///
/// `positions` is the only stream a mesh cannot do without; the rest are
/// optional and are left absent when the primitive does not declare them, so a
/// missing channel never changes the pipeline the draw picks.
#[derive(Default)]
pub(super) struct MeshStreams {
    pub positions: Vec<[f32; 3]>,
    pub uvs: Option<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[u8; 4]>>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub indices: Option<Vec<u32>>,
    pub joints: Option<Vec<[u16; 4]>>,
    pub weights: Option<Vec<[f32; 4]>>,
    pub morph_deltas: Option<MorphDeltas>,
}

impl MeshStreams {
    /// Borrow the streams as the description `allocate_lit_mesh` consumes.
    pub fn desc(&self) -> LitMeshDesc<'_> {
        LitMeshDesc {
            positions: &self.positions,
            uvs: self.uvs.as_deref(),
            colors: self.colors.as_deref(),
            normals: self.normals.as_deref(),
            indices: self.indices.as_deref(),
            joints: self.joints.as_deref(),
            weights: self.weights.as_deref(),
            morph_deltas: self.morph_deltas.clone(),
        }
    }
}

/// Read a primitive's streams through glTF's accessor reader.
pub(super) fn from_reader(
    buffers: &[gltf::buffer::Data],
    primitive: &gltf::Primitive<'_>,
) -> MeshStreams {
    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .expect("a glTF primitive carries its POSITION attribute")
        .collect();
    // Read only the streams the primitive declares: an attribute it does
    // not carry is not uploaded, so decoding it would be wasted work.
    let uvs: Option<Vec<[f32; 2]>> = samples_base_color_texture(primitive).then(|| {
        reader
            .read_tex_coords(0)
            .expect("the primitive samples a texture, so it has TEXCOORD_0")
            .into_f32()
            .collect()
    });
    let colors: Option<Vec<[u8; 4]>> =
        primitive
            .get(&gltf::Semantic::Colors(0))
            .is_some()
            .then(|| {
                reader
                    .read_colors(0)
                    .expect("the primitive has COLOR_0")
                    .into_rgba_u8()
                    .collect()
            });
    // A primitive that carries normals describes its own surface, which is
    // smoother than the one screen-space derivatives infer from the
    // positions; one without them falls back to those derivatives.
    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|normals| normals.collect());
    let indices: Option<Vec<u32>> = reader.read_indices().map(|i| i.into_u32().collect());
    let skinned = primitive.get(&gltf::Semantic::Joints(0)).is_some()
        && primitive.get(&gltf::Semantic::Weights(0)).is_some();
    let joints: Option<Vec<[u16; 4]>> = skinned.then(|| {
        reader
            .read_joints(0)
            .expect("the primitive has JOINTS_0")
            .into_u16()
            .collect()
    });
    let weights: Option<Vec<[f32; 4]>> = skinned.then(|| {
        reader
            .read_weights(0)
            .expect("the primitive has WEIGHTS_0")
            .into_f32()
            .collect()
    });
    // The displacements the mesh's morph stream reads, packed the way a
    // mesh wants them: for each vertex, every position-displacing target in
    // order, three components each. Writing straight into the packed array
    // keeps one allocation for the whole thing, however many targets the
    // primitive declares; a target of normals alone is skipped here.
    let morph_deltas = (morph_target_count(primitive) > 0).then(|| {
        let target_count = morph_target_count(primitive);
        let mut deltas = vec![0.0; positions.len() * target_count * 3];
        let mut target = 0;
        for (displacements, _, _) in reader.read_morph_targets() {
            let Some(displacements) = displacements else {
                continue;
            };
            for (vertex, displacement) in displacements.enumerate() {
                let start = (vertex * target_count + target) * 3;
                deltas[start..start + 3].copy_from_slice(&displacement);
            }
            target += 1;
        }
        MorphDeltas {
            deltas,
            target_count: target_count as u32,
        }
    });

    MeshStreams {
        positions,
        uvs,
        colors,
        normals,
        indices,
        joints,
        weights,
        morph_deltas,
    }
}
