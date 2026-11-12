use super::*;

#[test]
fn default_instance_color_is_white() {
    assert_eq!(InstanceColor::default().color, glam::Vec4::ONE);
}

#[test]
fn a_strip_pipeline_declares_the_mesh_index_width() {
    use wgpu::{IndexFormat, PrimitiveTopology as Topology};
    // A strip's pipeline must declare the width its draw binds, so the
    // mesh's format reaches it whichever width the source picked.
    assert_eq!(
        strip_index_format(Topology::LineStrip, Some(IndexFormat::Uint16)),
        Some(IndexFormat::Uint16)
    );
    assert_eq!(
        strip_index_format(Topology::TriangleStrip, Some(IndexFormat::Uint32)),
        Some(IndexFormat::Uint32)
    );
    // A non-indexed strip binds no index buffer, so it declares none.
    assert_eq!(strip_index_format(Topology::LineStrip, None), None);
    assert_eq!(strip_index_format(Topology::TriangleStrip, None), None);
}

#[test]
fn a_non_strip_pipeline_declares_no_index_width() {
    use wgpu::{IndexFormat, PrimitiveTopology as Topology};
    // `wgpu` rejects a strip index format on a non-strip topology, so an
    // indexed mesh of one must not leak its format into the pipeline.
    for topology in [
        Topology::PointList,
        Topology::LineList,
        Topology::TriangleList,
    ] {
        assert_eq!(
            strip_index_format(topology, Some(IndexFormat::Uint16)),
            None
        );
        assert_eq!(
            strip_index_format(topology, Some(IndexFormat::Uint32)),
            None
        );
    }
}
