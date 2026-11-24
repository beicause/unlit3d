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

/// The fewest vertices a primitive of `mode` has to state to draw anything.
///
/// The specification states one minimum per mode: a point is one vertex, a line
/// two, a triangle three, and a strip or a loop needs one primitive's worth of
/// them. A stream shorter than that cannot be drawn at all, so the loader
/// refuses it instead of uploading geometry the device would draw as nothing.
pub(super) fn minimum_vertices(mode: Mode) -> usize {
    match mode {
        Mode::Points => 1,
        Mode::Lines | Mode::LineStrip | Mode::LineLoop => 2,
        Mode::Triangles | Mode::TriangleStrip | Mode::TriangleFan => 3,
    }
}

/// The name glTF gives `mode`, as an error states it.
pub(super) fn name(mode: Mode) -> &'static str {
    match mode {
        Mode::Points => "POINTS",
        Mode::Lines => "LINES",
        Mode::LineStrip => "LINE_STRIP",
        Mode::LineLoop => "LINE_LOOP",
        Mode::Triangles => "TRIANGLES",
        Mode::TriangleStrip => "TRIANGLE_STRIP",
        Mode::TriangleFan => "TRIANGLE_FAN",
    }
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
/// The caller checks [`minimum_vertices`] first, so the stream always holds one
/// primitive of the mode.
///
/// # Panics
///
/// If `mode` is not a mode [`needs_expansion`], or `indices` is shorter than
/// [`minimum_vertices`] of it. Both are the caller's own checks: a primitive
/// that reaches here with a shorter stream is a loader bug, not a document one.
pub(super) fn expand(mode: Mode, indices: &[u32]) -> Vec<u32> {
    match mode {
        Mode::TriangleFan => {
            let mut triangles = Vec::with_capacity((indices.len() - 2) * 3);
            for pair in indices[1..].windows(2) {
                triangles.extend_from_slice(&[pair[0], pair[1], indices[0]]);
            }
            triangles
        }
        Mode::LineLoop => {
            let mut lines = Vec::with_capacity(indices.len() * 2);
            for pair in indices.windows(2) {
                lines.extend_from_slice(pair);
            }
            lines.extend_from_slice(&[indices[indices.len() - 1], indices[0]]);
            lines
        }
        _ => unreachable!("only a line loop or a triangle fan primitive is expanded"),
    }
}
