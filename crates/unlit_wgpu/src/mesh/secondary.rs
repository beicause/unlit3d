//! The secondary vertex stream: the UV, color and normal channels a mesh
//! carries beside its position, packed by the stream writer.

use wgpu::WriteOnly;

use super::compress::{compress_normals, compress_uvs};
use super::vertex_count_of;
use super::{
    ChannelEncoding, CompressedColor, CompressedNormal, CompressedUv, MeshMetadata, StreamChannels,
};

/// Bytes per vertex of the widest secondary stream, used to size the
/// per-vertex scratch the writer assembles on the stack.
///
/// The widest encoding the stream can write is an uncompressed UV followed by
/// the color and the normal, so the scratch covers those three attribute
/// formats.
const MAX_SECONDARY_STRIDE: usize = wgpu::VertexFormat::Float32x2.size() as usize
    + wgpu::VertexFormat::Unorm8x4.size() as usize
    + wgpu::VertexFormat::Snorm16x2.size() as usize;

bitflags::bitflags! {
    /// The optional per-vertex channels of a secondary vertex stream: the UV,
    /// color and normal a mesh carries beside its position.
    ///
    /// A stream is described by what it carries rather than by which pipeline
    /// consumes it, so packing a stream needs nothing the built-in pipeline
    /// defines: the built-in pipeline derives one of these from its own
    /// variant, and a custom pipeline declares one directly.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct SecondaryFlags: u16 {
        /// The stream carries a per-vertex UV.
        const UV = 1 << 0;
        /// The UV is written full precision -- `Float32x2` -- rather than
        /// compressed to `Snorm16x2`.
        ///
        /// An uncompressed UV needs no decode parameters, so writing it
        /// leaves [`crate::mesh::MeshMetadata`] alone.
        const UNCOMPRESSED_UV = 1 << 1;
        /// The stream carries a per-vertex color.
        const COLOR = 1 << 2;
        /// The stream carries a per-vertex normal, octahedral-encoded into
        /// `Snorm16x2`.
        const NORMAL = 1 << 3;
    }
}

/// The optional per-vertex channels of a secondary vertex stream: the UV,
/// color and normal that a mesh carries beside its position.
///
/// Each channel adds one attribute to [`crate::pipeline::SECONDARY_SLOT`], in
/// the order UV, color then normal, so the packed stream matches the shader's
/// declared locations for any combination. [`Self::write`] compresses the raw
/// attributes and interleaves them straight into the target buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SecondaryStreamWriter {
    /// The channels this stream writes.
    pub flags: SecondaryFlags,
}

impl SecondaryStreamWriter {
    /// Whether the UV attribute is written.
    pub fn uv(&self) -> bool {
        self.flags.contains(SecondaryFlags::UV)
    }

    /// Whether the color attribute is written.
    pub fn color(&self) -> bool {
        self.flags.contains(SecondaryFlags::COLOR)
    }

    /// Whether the normal attribute is written.
    pub fn normal(&self) -> bool {
        self.flags.contains(SecondaryFlags::NORMAL)
    }

    /// Whether the UV is written full precision rather than compressed to
    /// `Snorm16x2`.
    ///
    /// An uncompressed UV needs no decode parameters, so [`Self::write`]
    /// leaves the metadata alone.
    pub fn uncompressed_uv(&self) -> bool {
        self.flags.contains(SecondaryFlags::UNCOMPRESSED_UV)
    }

    /// Whether the variant declares no attributes at all, in which case the
    /// slot must be omitted from the vertex-buffer list.
    pub fn is_empty(&self) -> bool {
        !self.uv() && !self.color() && !self.normal()
    }

    /// The channels this stream carries, in attribute order.
    ///
    /// The single description the stride, the attribute offsets and the packed
    /// bytes are all derived from.
    pub fn channels(&self) -> StreamChannels {
        let mut channels = StreamChannels::default();
        if self.uv() {
            channels.push((
                crate::pipeline::location::UV,
                if self.uncompressed_uv() {
                    ChannelEncoding::UncompressedUv
                } else {
                    ChannelEncoding::CompressedUv
                },
            ));
        }
        if self.color() {
            channels.push((crate::pipeline::location::COLOR, ChannelEncoding::Color));
        }
        if self.normal() {
            channels.push((
                crate::pipeline::location::NORMAL,
                ChannelEncoding::CompressedNormal,
            ));
        }
        channels
    }

    /// Bytes per vertex of the packed stream.
    pub fn stride(&self) -> u32 {
        self.channels().stride()
    }

    /// Bytes one `vertex_count`-long stream occupies.
    pub fn byte_len(&self, vertex_count: usize) -> usize {
        self.channels().byte_len(vertex_count)
    }

    /// Compress the raw `uvs` and `normals` and write the interleaved
    /// result, together with the already compressed `colors`, into `out` —
    /// which must be exactly [`Self::byte_len`]`(vertex_count)` bytes,
    /// normally a mapped-at-creation vertex buffer, so the vertices land in
    /// GPU memory without an intermediate byte buffer.
    ///
    /// Colors are taken as [`CompressedColor`] because that is what the
    /// stream stores; float colors from a [glam::Vec4] can be quantized with
    /// [`quantize_colors`](super::quantize_colors) first. Normals need no such preparation: the
    /// octahedral encoding is absolute, so they are compressed here.
    ///
    /// `metadata` receives the UV decode parameters the compression derives,
    /// exactly as [`compress_uvs`] would.
    ///
    /// Nothing is allocated: the compressed UVs are produced lazily and
    /// written as they are computed. Use [`Self::write_compressed`] to write
    /// channels that are already compressed, for example to share one stream
    /// across several meshes.
    ///
    /// # Panics
    /// If an enabled channel has no matching slice, if the channel lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write(
        &self,
        uvs: &[[f32; 2]],
        colors: &[CompressedColor],
        normals: &[[f32; 3]],
        metadata: &mut MeshMetadata,
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = self.raw_vertex_count(uvs, colors, normals);
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );
        if vertex_count == 0 {
            return;
        }

        // Deriving the UV range is the only pass over the input; the encoding
        // itself streams straight into `out`. An uncompressed channel is
        // copied as it is, so it never derives metadata.
        let (write_uv, write_color, write_normal) = (self.uv(), self.color(), self.normal());
        let compress_uv = write_uv && !self.uncompressed_uv();
        let stride = self.stride() as usize;
        let mut packed_uvs = compress_uv
            .then(|| compress_uvs(uvs, metadata))
            .into_iter()
            .flatten();
        let mut compressed_normals = write_normal.then(|| compress_normals(normals));

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_SECONDARY_STRIDE];
            let mut len = 0;
            if write_uv {
                // A compressed UV is produced lazily as a temporary, so it
                // is copied out while it is still alive; an uncompressed one
                // is the caller's `f32` pair verbatim.
                if compress_uv {
                    let uv = packed_uvs.next().expect("one UV per vertex");
                    let bytes = uv.as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                } else {
                    let bytes = uvs[index].as_bytes();
                    vertex[len..len + bytes.len()].copy_from_slice(bytes);
                    len += bytes.len();
                }
            }
            if write_color {
                let bytes = colors[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            if write_normal {
                // The octahedral encoding needs no per-mesh range, so unlike
                // the UVs it streams straight out with the same laziness.
                let normal = compressed_normals
                    .as_mut()
                    .expect("compressed normals")
                    .next()
                    .expect("one normal per vertex");
                let bytes = normal.as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, stride);
            vertex.into_iter().take(len)
        }));
    }

    /// Write the already compressed `uvs`, `colors` and `normals` into
    /// `out` as the interleaved vertex stream, which must be exactly
    /// [`Self::byte_len`]`(vertex_count)` bytes.
    ///
    /// Takes the packed form produced by [`compress_uvs`],
    /// [`quantize_colors`](super::quantize_colors) and [`compress_normals`], so one compressed
    /// stream can be uploaded for several meshes without recompressing it.
    ///
    /// # Panics
    /// If an enabled channel has no matching slice, if the channel lengths
    /// disagree, or if `out` is not exactly one stream long.
    pub fn write_compressed(
        &self,
        uvs: &[CompressedUv],
        colors: &[CompressedColor],
        normals: &[CompressedNormal],
        out: WriteOnly<'_, [u8]>,
    ) {
        use zerocopy::IntoBytes;

        let vertex_count = self.compressed_vertex_count(uvs, colors, normals);
        assert_eq!(
            out.len(),
            self.byte_len(vertex_count),
            "the target must hold exactly one packed vertex stream"
        );

        // Each vertex is assembled as a fixed-size array on the stack and
        // streamed out, so writing never allocates at all.
        out.write_iter((0..vertex_count).flat_map(move |index| {
            let mut vertex = [0u8; MAX_SECONDARY_STRIDE];
            let mut len = 0;
            if self.uv() {
                let bytes = uvs[index].as_bytes();
                vertex[..bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            if self.color() {
                let bytes = colors[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            if self.normal() {
                let bytes = normals[index].as_bytes();
                vertex[len..len + bytes.len()].copy_from_slice(bytes);
                len += bytes.len();
            }
            debug_assert_eq!(len, self.stride() as usize);
            vertex.into_iter().take(len)
        }));
    }

    /// Number of vertices the raw channel slices describe.
    ///
    /// Every enabled channel describes the same vertices, so the first one
    /// sets the count and the rest must agree; a stream whose channels
    /// disagree has no length that would pack them together.
    fn raw_vertex_count(
        &self,
        uvs: &[[f32; 2]],
        colors: &[CompressedColor],
        normals: &[[f32; 3]],
    ) -> usize {
        let lengths: [(&str, Option<usize>); 3] = [
            ("UV", self.uv().then_some(uvs.len())),
            ("color", self.color().then_some(colors.len())),
            ("normal", self.normal().then_some(normals.len())),
        ];
        vertex_count_of(&lengths)
    }

    /// Number of vertices the compressed channel slices describe.
    fn compressed_vertex_count(
        &self,
        uvs: &[CompressedUv],
        colors: &[CompressedColor],
        normals: &[CompressedNormal],
    ) -> usize {
        let lengths: [(&str, Option<usize>); 3] = [
            ("UV", self.uv().then_some(uvs.len())),
            ("color", self.color().then_some(colors.len())),
            ("normal", self.normal().then_some(normals.len())),
        ];
        vertex_count_of(&lengths)
    }
}
