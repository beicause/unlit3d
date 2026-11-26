//! Owned vertex streams, however a glTF primitive supplied them.
//!
//! A lit mesh is uploaded from borrowed slices, so the decoded data has to
//! outlive the `allocate_lit_mesh` call. [`from_reader`] fills this one struct
//! from glTF's accessor reader, and [`MeshStreams::desc`] hands its streams to
//! the allocator as a [`LitMeshDesc`].

use super::material::base_color_uv_set;
use super::topology;
use super::{MeshId, PrimitiveId};
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
///
/// `mesh` and `primitive_id` are the primitive's document position and `targets`
/// the number of morph targets its mesh declares — the count
/// [`mesh_morph_targets`](super::scene::mesh_morph_targets) validated, so the
/// slots here and the weights a channel slices always agree.
///
/// A topology `wgpu` cannot assemble is reshaped here, which is also where a
/// primitive that states too few vertices, or indices past its own vertices, is
/// refused: uploading either would draw something the document never stated.
pub(super) fn from_reader(
    buffers: &[gltf::buffer::Data],
    mesh: MeshId,
    primitive_id: PrimitiveId,
    primitive: &gltf::Primitive<'_>,
    targets: usize,
) -> Result<MeshStreams, super::Error> {
    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or(super::Error::PositionsMissing {
            mesh,
            primitive: primitive_id,
        })?
        .collect();
    let vertices = positions.len();
    let mode = primitive.mode();

    // The stream a draw reads: the primitive's own index accessor, or the
    // implicit `0..vertex_count` a non-indexed one states. Both are checked
    // the same way, so a document is refused for the same reasons either way.
    let explicit: Option<Vec<u32>> = reader
        .read_indices()
        .map(|indices| indices.into_u32().collect());
    let stated = explicit.as_ref().map_or(vertices, Vec::len);
    if stated < topology::minimum_vertices(mode) {
        return Err(super::Error::PrimitiveTooShort {
            mesh,
            primitive: primitive_id,
            mode: topology::name(mode),
            vertices: stated,
        });
    }
    if let Some(indices) = &explicit
        && let Some(&index) = indices.iter().find(|&&index| index as usize >= vertices)
    {
        return Err(super::Error::IndexOutOfRange {
            mesh,
            primitive: primitive_id,
            index,
            vertices,
        });
    }
    let indices = if topology::needs_expansion(mode) {
        let implicit;
        let stream: &[u32] = match &explicit {
            Some(indices) => indices,
            None => {
                implicit = (0..vertices as u32).collect::<Vec<u32>>();
                &implicit
            }
        };
        Some(topology::expand(mode, stream))
    } else {
        explicit
    };

    // Read only the streams the primitive declares: an attribute it does
    // not carry is not uploaded, so decoding it would be wasted work.
    // A texture is sampled through the UV set its material names, which is not
    // always the first; a primitive that carries no such set has no UV to
    // interpolate and draws untextured.
    let uvs: Option<Vec<[f32; 2]>> = base_color_uv_set(primitive).map(|set| {
        reader
            .read_tex_coords(set)
            .expect("base_color_uv_set found the set, so the reader yields it")
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
                    .expect("the primitive declares COLOR_0")
                    .into_rgba_u8()
                    .collect()
            });
    // A primitive that carries normals describes its own surface, which is
    // smoother than the one screen-space derivatives infer from the
    // positions; one without them falls back to those derivatives.
    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|normals| normals.collect());
    let skinned = primitive.get(&gltf::Semantic::Joints(0)).is_some()
        && primitive.get(&gltf::Semantic::Weights(0)).is_some();
    let joints: Option<Vec<[u16; 4]>> = skinned.then(|| {
        reader
            .read_joints(0)
            .expect("the primitive declares JOINTS_0")
            .into_u16()
            .collect()
    });
    let weights: Option<Vec<[f32; 4]>> = skinned.then(|| {
        reader
            .read_weights(0)
            .expect("the primitive declares WEIGHTS_0")
            .into_f32()
            .collect()
    });

    // Pack each target's displacements for every vertex, in target order. Every
    // target owns a slot even when it displaces nothing this loader reads: the
    // weights are indexed by the target's slot, so a target without positions
    // keeps its slot's displacements at zero rather than shifting the targets
    // after it one slot down. A conformant document writes the displacements in
    // the decoded vertex order; displacements past the vertex count are ignored
    // rather than panicking on a malformed one.
    let morph_deltas = (targets > 0).then(|| {
        let mut deltas = vec![0.0; vertices * targets * 3];
        for (target, (displacements, _, _)) in reader.read_morph_targets().enumerate() {
            let Some(displacements) = displacements else {
                continue;
            };
            for (vertex, displacement) in displacements.enumerate() {
                let start = (vertex * targets + target) * 3;
                if let Some(slot) = deltas.get_mut(start..start + 3) {
                    slot.copy_from_slice(&displacement);
                }
            }
        }
        MorphDeltas {
            deltas,
            target_count: targets as u32,
        }
    });

    Ok(MeshStreams {
        positions,
        uvs,
        colors,
        normals,
        indices,
        joints,
        weights,
        morph_deltas,
    })
}
