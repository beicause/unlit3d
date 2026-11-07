//! Owned vertex streams, however a glTF primitive supplied them.
//!
//! A lit mesh is uploaded from borrowed slices, so the decoded data has to
//! outlive the `allocate_lit_mesh` call. Both the ordinary accessor reader
//! and the Draco decoder fill this one struct, and [`MeshStreams::desc`] hands
//! its streams to the allocator as a [`LitMeshDesc`].

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
    /// Take the streams as the description `allocate_lit_mesh` consumes.
    ///
    /// The description owns its morph displacements, so they are moved out
    /// rather than cloned; a stream is spent on one allocation anyway.
    pub fn desc(&mut self) -> LitMeshDesc<'_> {
        let morph_deltas = self.morph_deltas.take();
        LitMeshDesc {
            positions: &self.positions,
            uvs: self.uvs.as_deref(),
            colors: self.colors.as_deref(),
            normals: self.normals.as_deref(),
            indices: self.indices.as_deref(),
            joints: self.joints.as_deref(),
            weights: self.weights.as_deref(),
            morph_deltas,
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
    let morph_deltas = read_morph_deltas(buffers, primitive, positions.len());

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

/// Pack a primitive's position-displacing morph targets for a mesh of
/// `vertex_count` vertices, as the mesh's displacement stream wants them:
/// for each vertex, every positional target in order, three components each.
///
/// Morph targets are not compressed into a Draco bitstream — their
/// displacements live in ordinary accessors — so both the accessor reader and
/// the Draco decoder share this packing. A Draco decode can change the vertex
/// order and count, so a conformant generator writes these displacements in
/// the decoded order; displacements past `vertex_count` are ignored rather
/// than panicking on a malformed document.
pub(super) fn read_morph_deltas(
    buffers: &[gltf::buffer::Data],
    primitive: &gltf::Primitive<'_>,
    vertex_count: usize,
) -> Option<MorphDeltas> {
    let target_count = morph_target_count(primitive);
    (target_count > 0).then(|| {
        let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
        let mut deltas = vec![0.0; vertex_count * target_count * 3];
        let mut target = 0;
        for (displacements, _, _) in reader.read_morph_targets() {
            let Some(displacements) = displacements else {
                continue;
            };
            for (vertex, displacement) in displacements.enumerate() {
                let start = (vertex * target_count + target) * 3;
                if let Some(slot) = deltas.get_mut(start..start + 3) {
                    slot.copy_from_slice(&displacement);
                }
            }
            target += 1;
        }
        MorphDeltas {
            deltas,
            target_count: target_count as u32,
        }
    })
}
