//! The mesh description.
//!
//! A [`MeshDescriptor`] says what a mesh is in wgpu's own terms: vertex buffers
//! tagged with the slot a pipeline's vertex state expects them in, the way the
//! draws of the mesh address its geometry, and the mesh's local-space [`Aabb`].
//! The renderer assumes no particular vertex layout, so a mesh can carry any
//! combination of attributes and one pipeline, several, or none may specialize
//! on it.
//!
//! A mesh's *pose* is not part of its description. Joint matrices and morph
//! weights are per-frame CPU state, so they live in components of their own —
//! [`SkinPose`](crate::components::SkinPose) and
//! [`MorphWeights`](crate::components::MorphWeights) — that the mesh's entity
//! references through a [`SkinBinding`](crate::components::SkinBinding) and a
//! [`MorphBinding`](crate::components::MorphBinding).

use arrayvec::ArrayVec;

use crate::bounds::Aabb;
pub use unlit_wgpu::mesh::JointMatrix;
use unlit_wgpu::scene::MAX_VERTEX_BUFFERS;
use unlit_wgpu::specialize::VertexAttributes;

/// A vertex buffer bound at `slot` for every draw of a mesh, with the
/// layout the pipeline's vertex state must match.
#[derive(Clone, Debug)]
pub struct VertexBufferDesc {
    /// The vertex-buffer slot the pipeline's vertex state declares.
    pub slot: u32,
    /// The buffer itself.
    pub buffer: wgpu::Buffer,
    /// Stride between elements.
    pub array_stride: u64,
    /// How the buffer advances: per vertex or per instance.
    pub step_mode: wgpu::VertexStepMode,
    /// The attributes this buffer provides.
    pub attributes: VertexAttributes,
}

/// How the draws of a mesh address its geometry.
///
/// The two ways of drawing a mesh take different counts — a vertex count or an
/// index count — and only an indexed draw names an index buffer at all. Naming
/// the geometry with one value rather than with a count, a flag and an optional
/// buffer keeps a mesh from describing a draw it cannot make.
#[derive(Clone, Debug)]
pub enum MeshIndexing {
    /// Draw the first `vertex_count` vertices, with no index buffer.
    NonIndexed {
        /// How many vertices the mesh draws.
        vertex_count: u32,
    },
    /// Draw `count` indices from `index_buffer`.
    Indexed {
        /// The buffer the indices live in.
        index_buffer: wgpu::Buffer,
        /// How wide one index is.
        format: wgpu::IndexFormat,
        /// How many indices the mesh draws.
        count: u32,
    },
}

impl MeshIndexing {
    /// How many indices or vertices a draw of the mesh reads.
    #[must_use]
    pub const fn count(&self) -> u32 {
        match self {
            Self::NonIndexed { vertex_count } => *vertex_count,
            Self::Indexed { count, .. } => *count,
        }
    }
}

/// Geometry to upload, described the way wgpu describes it.
///
/// [`crate::mesh_source::MeshSource::allocate_mesh`] takes one of these and
/// returns a [`crate::components::GpuMesh`] handle, or the reason the
/// description does not describe a mesh that can be drawn. The buffers are
/// moved into the source's resource graph, so the caller hands over ownership.
#[derive(Clone, Debug)]
pub struct MeshDescriptor {
    /// Vertex buffers, each tagged with its slot.
    ///
    /// At most [`MAX_VERTEX_BUFFERS`], the number of vertex-buffer slots a
    /// pass can bind: a mesh needing more cannot be drawn.
    pub vertex_buffers: ArrayVec<VertexBufferDesc, MAX_VERTEX_BUFFERS>,
    /// How draws of this mesh address its geometry.
    pub indexing: MeshIndexing,
    /// The mesh's local-space bounding box, recorded in the mesh-metadata
    /// array and used for CPU frustum culling.
    ///
    /// There is no default: a box the caller guessed at culls better than a
    /// zero box at the origin, which culls every mesh away.
    pub aabb: Aabb,
    /// A bind group bound at
    /// [`MESH_GROUP`](unlit_wgpu::pipeline::MESH_GROUP), for a
    /// pipeline that reads per-mesh data.
    ///
    /// `None` for a pipeline that binds nothing at that index.
    pub bind_group: Option<wgpu::BindGroup>,

    /// How many joint matrices the mesh's vertices are deformed by.
    ///
    /// A mesh whose position stream carries joint indices declares the highest
    /// index it uses, so the pose it is drawn with can be held to have a matrix
    /// for each of them. A mesh with no joints leaves this `None`, exactly as a
    /// mesh with no morph targets leaves `morph_deltas` alone.
    pub skin: Option<SkinShape>,

    /// The morph displacements the mesh's vertices are displaced by.
    ///
    /// A mesh with no morph targets leaves this `None`.
    ///
    /// The weights that blend these displacements are *not* here: they are
    /// per-instance pose state a [`MorphWeights`](crate::components::MorphWeights)
    /// entity holds, so a mesh names the entity rather than owning the weights.
    pub morph_deltas: Option<MorphDeltas>,
}

impl MeshDescriptor {
    /// Check that the description names a mesh every part of which is inside
    /// the buffers it reads.
    ///
    /// A mesh that describes a draw reaching past its own buffers is a
    /// programming error of the caller rather than a state the renderer can
    /// recover from, so it is reported before anything is uploaded. A
    /// non-indexed mesh's vertex buffers are checked against its vertex count;
    /// an indexed mesh's index buffer against its index count, which is as far
    /// as a description goes — how many vertices its indices name is in the
    /// index data, not in the description.
    ///
    /// # Errors
    ///
    /// If the description draws nothing, if a slot is past the last one a pass
    /// binds, if a vertex buffer declares attributes with no stride, if the
    /// draw reaches past a buffer, or if the morph displacements do not hold
    /// three components per vertex and target.
    pub fn validate(&self) -> Result<(), MeshError> {
        let slots = MAX_VERTEX_BUFFERS as u32;
        let drawn = match &self.indexing {
            MeshIndexing::NonIndexed { vertex_count } => {
                if *vertex_count == 0 {
                    return Err(MeshError::NoVertices);
                }
                Some(*vertex_count)
            }
            MeshIndexing::Indexed {
                index_buffer,
                format,
                count,
            } => {
                if *count == 0 {
                    return Err(MeshError::NoIndices);
                }
                let needed = u64::from(*count) * index_size(*format);
                if needed > index_buffer.size() {
                    return Err(MeshError::IndexBufferShort {
                        count: *count,
                        format: *format,
                        needed,
                        size: index_buffer.size(),
                    });
                }
                None
            }
        };

        for buffer in &self.vertex_buffers {
            if buffer.slot >= slots {
                return Err(MeshError::SlotOutOfRange {
                    slot: buffer.slot,
                    slots,
                });
            }
            // A stride of zero is how a slot the pipeline declares and the mesh
            // does not fill stays empty, and such a slot declares no attribute
            // to fetch.
            if buffer.array_stride == 0 && !buffer.attributes.is_empty() {
                return Err(MeshError::ZeroStride { slot: buffer.slot });
            }
            // An instance-step buffer advances per instance, and a mesh
            // description says nothing about how many instances draw it, so
            // only a per-vertex buffer can be checked against the vertex count.
            if buffer.step_mode == wgpu::VertexStepMode::Vertex
                && let Some(vertices) = drawn
            {
                let needed = buffer.array_stride * u64::from(vertices);
                if needed > buffer.buffer.size() {
                    return Err(MeshError::VertexBufferShort {
                        slot: buffer.slot,
                        needed,
                        size: buffer.buffer.size(),
                    });
                }
            }
        }

        if let Some(morph) = &self.morph_deltas {
            if morph.target_count == 0 {
                return Err(MeshError::NoMorphTargets);
            }
            // Without an index buffer the vertex count is the one the draw
            // names; with one, only the index data says what the highest
            // vertex is, so the shortest displacement array that could serve
            // any index is taken as the expected one.
            let expected = self
                .indexing
                .count()
                .checked_mul(morph.target_count)
                .and_then(|targets| targets.checked_mul(3))
                .map_or(usize::MAX, |count| count as usize);
            if morph.deltas.len() < expected {
                return Err(MeshError::MorphDeltasShort {
                    actual: morph.deltas.len(),
                    expected,
                });
            }
        }
        Ok(())
    }
}

/// The joint matrices a skinned mesh is drawn with, as the mesh declares them.
///
/// A skinned mesh's vertex stream names joints by index, so the pose it is
/// drawn with has to hold a matrix for the highest index it uses. The mesh is
/// the one that knows that index, which is why it states the count here rather
/// than leaving the check to the draw: reading past the end of an uploaded pose
/// is a GPU out-of-bounds access, and the draw path has no way to report one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SkinShape {
    /// How many joint matrices the mesh needs, one past the highest joint index
    /// its vertex stream uses.
    pub joints: u32,
}

impl SkinShape {
    /// The shape of a mesh whose vertex stream uses joints up to `highest`.
    #[must_use]
    pub const fn through(highest: u16) -> Self {
        Self {
            joints: highest as u32 + 1,
        }
    }

    /// Check that a pose holds a matrix for every joint the mesh names.
    ///
    /// # Errors
    ///
    /// If the pose is shorter than the mesh's highest joint index needs.
    pub fn validate(&self, pose: &[JointMatrix]) -> Result<(), MeshError> {
        if pose.len() < self.joints as usize {
            return Err(MeshError::PoseShort {
                joints: self.joints,
                held: pose.len(),
            });
        }
        Ok(())
    }
}

/// Why a [`MeshDescriptor`] does not describe a mesh that can be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MeshError {
    /// The description draws no vertices.
    #[error("a non-indexed mesh needs at least one vertex")]
    NoVertices,
    /// The description draws no indices.
    #[error("an indexed mesh needs at least one index")]
    NoIndices,
    /// A vertex buffer names a slot no pass can bind.
    #[error("vertex buffer slot {slot} is past the {slots} slots a pass binds")]
    SlotOutOfRange {
        /// The slot the buffer names.
        slot: u32,
        /// How many slots a pass binds.
        slots: u32,
    },
    /// A vertex buffer declares attributes but advances by no stride.
    #[error("vertex buffer {slot} declares attributes with a stride of zero")]
    ZeroStride {
        /// The slot the buffer names.
        slot: u32,
    },
    /// A draw reads more vertices than the buffer holds.
    #[error("vertex buffer {slot} needs {needed} bytes for the draw and holds {size}")]
    VertexBufferShort {
        /// The slot the buffer names.
        slot: u32,
        /// How many bytes the draw reads.
        needed: u64,
        /// How many bytes the buffer holds.
        size: u64,
    },
    /// A draw reads more indices than the buffer holds.
    #[error("the {count} indices of {format:?} need {needed} bytes and the buffer holds {size}")]
    IndexBufferShort {
        /// How many indices the draw reads.
        count: u32,
        /// How wide one index is.
        format: wgpu::IndexFormat,
        /// How many bytes the draw reads.
        needed: u64,
        /// How many bytes the buffer holds.
        size: u64,
    },
    /// A mesh declares morph targets.
    #[error("a morphing mesh needs at least one target")]
    NoMorphTargets,
    /// A pose holds fewer matrices than the mesh's joints need.
    #[error("the mesh names {joints} joints and the pose holds {held} matrices")]
    PoseShort {
        /// How many matrices the mesh needs.
        joints: u32,
        /// How many the pose holds.
        held: usize,
    },
    /// The morph displacements do not cover every vertex and target.
    #[error("the morph displacements hold {actual} values where {expected} are needed")]
    MorphDeltasShort {
        /// How many values they hold.
        actual: usize,
        /// How many the mesh needs.
        expected: usize,
    },
}

/// How many bytes one index of `format` occupies.
#[must_use]
pub const fn index_size(format: wgpu::IndexFormat) -> u64 {
    match format {
        wgpu::IndexFormat::Uint16 => 2,
        wgpu::IndexFormat::Uint32 => 4,
    }
}

/// Where a draw starts reading a mesh and how much it reads.
///
/// A mesh owns a slice of the buffer it draws from — its own buffer, or its own
/// range of the pool a source shares between meshes — and this is that slice in
/// the form a draw takes it. Resolving it once, when the mesh is uploaded, is
/// what lets the draw path key on it without reading the mesh's parts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MeshDraw {
    /// Draw a range of vertices, with no index buffer.
    Vertices {
        /// The vertex range to draw.
        vertices: core::ops::Range<u32>,
    },
    /// Draw a range of indices, offset by `base_vertex`.
    Indexed {
        /// The index range to draw.
        indices: core::ops::Range<u32>,
        /// Value added to every index before fetching a vertex.
        base_vertex: i32,
    },
}

impl MeshDraw {
    /// Where the draw starts in the buffer it reads.
    #[must_use]
    pub fn first(&self) -> u32 {
        match self {
            Self::Vertices { vertices } => vertices.start,
            Self::Indexed { indices, .. } => indices.start,
        }
    }

    /// How many indices or vertices the draw reads.
    #[must_use]
    pub fn count(&self) -> u32 {
        match self {
            Self::Vertices { vertices } => vertices.end - vertices.start,
            Self::Indexed { indices, .. } => indices.end - indices.start,
        }
    }

    /// The offset added to every index, which a non-indexed draw has none of.
    #[must_use]
    pub const fn base_vertex(&self) -> i32 {
        match self {
            Self::Vertices { .. } => 0,
            Self::Indexed { base_vertex, .. } => *base_vertex,
        }
    }

    /// Whether the draw reads an index buffer.
    #[must_use]
    pub const fn is_indexed(&self) -> bool {
        matches!(self, Self::Indexed { .. })
    }
}

/// A mesh's morph displacements and how many targets follow each vertex.
///
/// The displacements are CPU data: the source pools them into one frame-wide
/// array alongside every other morphed mesh's, and the mesh's metadata entry
/// names the slice it took. Handing the source the geometry rather than a
/// ready-made GPU buffer is what lets it lay every mesh's displacements out in
/// one array.
#[derive(Clone, Debug)]
pub struct MorphDeltas {
    /// Every target's per-vertex position displacement, flat and tightly
    /// packed: for each vertex, `target_count` targets in order, three
    /// components each.
    pub deltas: Vec<f32>,
    /// How many targets follow each vertex.
    pub target_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> wgpu::Device {
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0
    }

    fn buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX,
            mapped_at_creation: false,
        })
    }

    fn descriptor(indexing: MeshIndexing) -> MeshDescriptor {
        MeshDescriptor {
            vertex_buffers: ArrayVec::new(),
            indexing,
            aabb: Aabb::new(glam::Vec3::ZERO, glam::Vec3::ONE),
            bind_group: None,
            skin: None,
            morph_deltas: None,
        }
    }

    #[test]
    fn a_mesh_that_draws_nothing_is_refused() {
        let device = device();
        let none = descriptor(MeshIndexing::NonIndexed { vertex_count: 0 });
        assert_eq!(none.validate(), Err(MeshError::NoVertices));

        let no_indices = descriptor(MeshIndexing::Indexed {
            index_buffer: buffer(&device, 6),
            format: wgpu::IndexFormat::Uint16,
            count: 0,
        });
        assert_eq!(no_indices.validate(), Err(MeshError::NoIndices));
    }

    #[test]
    fn an_index_buffer_shorter_than_the_count_is_refused() {
        let device = device();
        let short = descriptor(MeshIndexing::Indexed {
            index_buffer: buffer(&device, 6),
            format: wgpu::IndexFormat::Uint32,
            count: 3,
        });
        assert_eq!(
            short.validate(),
            Err(MeshError::IndexBufferShort {
                count: 3,
                format: wgpu::IndexFormat::Uint32,
                needed: 12,
                size: 6,
            })
        );
    }

    #[test]
    fn a_vertex_buffer_shorter_than_the_draw_is_refused() {
        let device = device();
        let mut short = descriptor(MeshIndexing::NonIndexed { vertex_count: 4 });
        short.vertex_buffers.push(VertexBufferDesc {
            slot: 0,
            buffer: buffer(&device, 12),
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: VertexAttributes::new(),
        });
        assert_eq!(
            short.validate(),
            Err(MeshError::VertexBufferShort {
                slot: 0,
                needed: 48,
                size: 12,
            })
        );
    }

    #[test]
    fn a_slot_no_pass_binds_is_refused() {
        let device = device();
        let mut over = descriptor(MeshIndexing::NonIndexed { vertex_count: 3 });
        over.vertex_buffers.push(VertexBufferDesc {
            slot: MAX_VERTEX_BUFFERS as u32,
            buffer: buffer(&device, 64),
            array_stride: 4,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: VertexAttributes::new(),
        });
        assert_eq!(
            over.validate(),
            Err(MeshError::SlotOutOfRange {
                slot: MAX_VERTEX_BUFFERS as u32,
                slots: MAX_VERTEX_BUFFERS as u32,
            })
        );
    }

    #[test]
    fn an_empty_slot_may_declare_no_attribute() {
        let device = device();
        let mut empty = descriptor(MeshIndexing::NonIndexed { vertex_count: 3 });
        empty.vertex_buffers.push(VertexBufferDesc {
            slot: 0,
            buffer: buffer(&device, 0),
            array_stride: 0,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: VertexAttributes::new(),
        });
        assert_eq!(empty.validate(), Ok(()), "a slot the mesh does not fill");

        let mut declared = empty.clone();
        declared.vertex_buffers[0].attributes =
            VertexAttributes::from_slice(&[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: 0,
                shader_location: 0,
            }]);
        assert_eq!(declared.validate(), Err(MeshError::ZeroStride { slot: 0 }));
    }

    #[test]
    fn morph_displacements_that_do_not_cover_the_draw_are_refused() {
        let mut short = descriptor(MeshIndexing::NonIndexed { vertex_count: 3 });
        short.morph_deltas = Some(MorphDeltas {
            deltas: vec![0.0; 3 * 3],
            target_count: 2,
        });
        assert_eq!(
            short.validate(),
            Err(MeshError::MorphDeltasShort {
                actual: 9,
                expected: 18,
            })
        );

        short.morph_deltas = Some(MorphDeltas {
            deltas: Vec::new(),
            target_count: 0,
        });
        assert_eq!(short.validate(), Err(MeshError::NoMorphTargets));
    }

    #[test]
    fn a_pose_shorter_than_the_meshs_joints_is_refused() {
        let skin = SkinShape::through(3);
        assert_eq!(skin.joints, 4, "the first unused index is the count");
        assert_eq!(
            skin.validate(&[JointMatrix::IDENTITY; 3]),
            Err(MeshError::PoseShort { joints: 4, held: 3 })
        );
        assert_eq!(skin.validate(&[JointMatrix::IDENTITY; 4]), Ok(()));
    }
}
