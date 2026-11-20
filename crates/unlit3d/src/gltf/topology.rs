//! The primitive topology a glTF mesh primitive draws with, and the index
//! list an unsupported one is expanded into.
//!
//! glTF states seven primitive modes; `wgpu` states five. Four of the seven are
//! one of those five under another name. `TriangleStrip` and `LineStrip` also
//! pass through: the rasterizer assembles them itself, and a strip's winding
//! alternates with every triangle, which no CPU expansion reproduces. `LineLoop`
//! and `TriangleFan` have no `wgpu` name at all, so their index stream is
//! rewritten here into the list of the same topology.

use gltf::mesh::Mode;

/// Whether a mode has to be expanded into a list of the same topology.
pub(super) fn needs_expansion(mode: Mode) -> bool {
    matches!(mode, Mode::LineLoop | Mode::TriangleFan)
}

/// The `wgpu` topology a primitive of `mode` draws with.
///
/// A mode [`needs_expansion`] draws as the list its indices are expanded into,
/// so the answer is the topology of the stream a caller has to reshape.
pub(super) fn topology(mode: Mode) -> wgpu::PrimitiveTopology {
    match mode {
        Mode::Points => wgpu::PrimitiveTopology::PointList,
        Mode::Lines => wgpu::PrimitiveTopology::LineList,
        Mode::LineStrip => wgpu::PrimitiveTopology::LineStrip,
        Mode::Triangles => wgpu::PrimitiveTopology::TriangleList,
        Mode::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        Mode::LineLoop => wgpu::PrimitiveTopology::LineList,
        Mode::TriangleFan => wgpu::PrimitiveTopology::TriangleList,
    }
}

/// Rewrite the index stream of a `LineLoop` or a `TriangleFan` into the list
/// that draws the same primitives.
///
/// glTF states a topology as vertex sets rather than as an index stream, so
/// each primitive of one is spelled out in full:
///
/// - a loop is a strip plus the segment from its last vertex back to its first:
///   `{v_i, v_{i+1}}` for every adjacent pair, then `{v_last, v_0}`;
/// - a fan is `{v_{i+1}, v_{i+2}, v_0}` per triangle, so the shared vertex sits
///   last in every one of them and each keeps the winding the document states.
///
/// A primitive with no index accessor has an implicit stream `0..vertex_count`,
/// which the caller passes in as such: the expansion never has to tell the two
/// apart, and the result is an index buffer either way.
///
/// # Panics
///
/// If `mode` is not a mode [`needs_expansion`], or if `indices` is too short
/// to hold one primitive of `mode` — the specification requires at least two
/// vertices for a loop and at least three for a fan.
pub(super) fn expand(mode: Mode, indices: &[u32]) -> Vec<u32> {
    match mode {
        Mode::TriangleFan => {
            assert!(
                indices.len() >= 3,
                "a triangle fan primitive needs at least three vertices"
            );
            let mut triangles = Vec::with_capacity((indices.len() - 2) * 3);
            for pair in indices[1..].windows(2) {
                triangles.extend_from_slice(&[pair[0], pair[1], indices[0]]);
            }
            triangles
        }
        Mode::LineLoop => {
            assert!(
                indices.len() >= 2,
                "a line loop primitive needs at least two vertices"
            );
            let mut lines = Vec::with_capacity(indices.len() * 2);
            for pair in indices.windows(2) {
                lines.extend_from_slice(pair);
            }
            lines.extend_from_slice(&[indices[indices.len() - 1], indices[0]]);
            lines
        }
        _ => panic!("only a line loop or a triangle fan primitive is expanded"),
    }
}
