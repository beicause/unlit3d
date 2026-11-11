//! The position vertex stream: the compressed position and, for a skinned
//! mesh, the joint indices and weights that deform it.

use wgpu::WriteOnly;

use super::compress::compress_positions;
use super::{ChannelEncoding, CompressedJoints, CompressedWeights, MeshMetadata, StreamChannels};

/// The channels of the position vertex stream: a position and, for a skinned
/// mesh, the joint indices and weights that deform it.
///
/// Joints and weights share this stream rather than getting one of their own,
/// so a skinned mesh's per-vertex data stays one buffer and one stride. The
/// joint pair is addressed by vertex index rather than by attribute, so
/// nothing else about a draw changes when the stream carries it.
///
/// The order is the attribute order: position, joint indices, joint weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PositionStreamChannels {
    /// How the position itself is encoded, or `None` for a variant that reads
    /// no position at all — its geometry is a single point at the instance
    /// origin, which is what a point or impostor draw wants.
    pub position: Option<ChannelEncoding>,
    /// Whether the stream carries joint indices and weights.
    pub joints: bool,
}

impl PositionStreamChannels {
    /// The channels of the stream, in attribute order.
    pub fn channels(&self) -> StreamChannels {
        let mut channels = StreamChannels::default();
        if let Some(position) = self.position {
            channels.push((crate::pipeline::location::POSITION, position));
        }
        if self.joints {
            channels.push((crate::pipeline::location::JOINTS, ChannelEncoding::Joints));
            channels.push((
                crate::pipeline::location::JOINTS_WEIGHTS,
                ChannelEncoding::Weights,
            ));
        }
        channels
    }
}

/// Packs the position vertex stream: the compressed position and, when the
/// stream declares them, the joint indices and weights that deform it.
///
/// Joints and weights share the position's stream, so a skinned mesh's draw
/// names one buffer for all three channels. Positions are compressed against
/// the mesh's bounding box exactly as [`compress_positions`] does; the joint
/// pair arrives already in its stored widths and is copied through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PositionStreamWriter {
    /// The channels this stream writes.
    pub channels: PositionStreamChannels,
}

impl PositionStreamWriter {
    /// Whether the stream carries joint indices and weights.
    pub fn joints(&self) -> bool {
        self.channels.joints
    }

    /// Whether the position is written full precision rather than compressed
    /// to `Snorm16x4`. A planar position is full precision too, only two
    /// components wide.
    pub fn uncompressed_position(&self) -> bool {
        matches!(
            self.channels.position,
            Some(ChannelEncoding::UncompressedPosition | ChannelEncoding::PlanarPosition)
        )
    }

    /// Whether the position is written as two components rather than three,
    /// with the third read as zero.
    pub fn planar_position(&self) -> bool {
        matches!(
            self.channels.position,
            Some(ChannelEncoding::PlanarPosition)
        )
    }

    /// Bytes per vertex of the packed stream.
    pub fn stride(&self) -> u32 {
        self.channels.channels().stride()
    }

    /// Bytes one `vertex_count`-long stream occupies.
    pub fn byte_len(&self, vertex_count: usize) -> usize {
        self.channels.channels().byte_len(vertex_count)
    }

    /// Compress the raw `positions` and write the interleaved result, together
    /// with the already compressed joint pair, into `out` — which must be
    /// exactly [`Self::byte_len`]`(vertex_count)` bytes.
    ///
    /// `metadata` receives the bounding box the position compression derives,
    /// exactly as [`compress_positions`] would.
    ///
    /// `joints` and `weights` are taken as their stored widths, because that
    /// is what the stream stores; float weights can be quantized with
    /// [`compress_weights`](super::compress_weights) first. They arrive as iterators so a caller can
    /// compress its weights lazily as the stream is packed, without collecting
    /// them first. A stream that declares no joints ignores both.
    ///
    /// # Panics
    ///
    /// If a declared channel's iterator is shorter than the positions, if the
    /// two lengths disagree, or if `out` is not exactly one stream long.
    pub fn write(
        &self,
        positions: &[[f32; 3]],
        joints: impl ExactSizeIterator<Item = CompressedJoints>,
        weights: impl ExactSizeIterator<Item = CompressedWeights>,
        metadata: &mut MeshMetadata,
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = positions.len();
        let mut joints = joints;
        let mut weights = weights;
        if self.joints() {
            assert_eq!(
                joints.len(),
                vertex_count,
                "the joint stream must describe the same vertices as the positions"
            );
            assert_eq!(
                weights.len(),
                vertex_count,
                "the weight stream must describe the same vertices as the positions"
            );
        }
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );
        if vertex_count == 0 {
            return;
        }

        // Deriving the bounding box is the only pass over the input; the
        // encoding itself streams straight into `out`. An uncompressed
        // position is copied as it is, so it never derives metadata.
        let write_position = self.channels.position;
        let uncompressed = self.uncompressed_position();
        let write_joints = self.joints();
        let stride = self.stride() as usize;
        let mut packed_positions = (!uncompressed && write_position.is_some())
            .then(|| compress_positions(positions, metadata))
            .into_iter()
            .flatten();

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_POSITION_STRIDE];
            let mut len = 0;
            match write_position {
                // A planar position stores the two components the source has;
                // the zero third is the array padding `take` drops below.
                Some(ChannelEncoding::PlanarPosition) => {
                    let flat = [positions[index][0], positions[index][1]];
                    let bytes = flat.as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
                Some(_) if uncompressed => {
                    let bytes = positions[index].as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
                Some(_) => {
                    let position = packed_positions.next().expect("one position per vertex");
                    let bytes = position.as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
                // No position channel: the stream carries whatever follows,
                // which is nothing unless the variant also reads joints.
                None => {}
            }
            if write_joints {
                let joint = joints.next().expect("one joint set per vertex");
                let bytes = joint.as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
                let weight = weights.next().expect("one weight set per vertex");
                let bytes = weight.as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, stride);
            vertex.into_iter().take(len)
        }));
    }
}

/// The joint indices and weights of one vertex, as the position stream stores
/// them.
///
/// The pair shares the position's stream, so this is the layout the shader's
/// `VertexInput` declares for them: `Uint16x4` indices followed by `Unorm16x4`
/// weights. The two are written together and never separately, which is why
/// the stream carries them as one unit.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
)]
pub struct JointPair {
    /// Joint indices into the mesh's joint matrices, `Uint16x4`.
    pub joints: CompressedJoints,
    /// Joint weights, `Unorm16x4`, which should sum to 1.
    pub weights: CompressedWeights,
}

impl JointPair {
    /// Bytes one pair occupies, the sum of the two channels.
    pub const SIZE: u32 = ChannelEncoding::Joints.size() + ChannelEncoding::Weights.size();
}

/// Bytes per vertex of the widest position stream, used to size the per-vertex
/// scratch the writer assembles on the stack.
///
/// The widest encoding is an uncompressed position followed by the joint pair.
const MAX_POSITION_STRIDE: usize =
    wgpu::VertexFormat::Float32x3.size() as usize + JointPair::SIZE as usize;
