//! End-to-end GPU tests for the built-in lit pipeline.
//!
//! Every test builds real resources through the library's public API — the
//! vertex compressors, the built-in [`SpecializedLitPipeline`] and the single-pass
//! [`Renderer`] — renders offscreen, and inspects the pixels that come back.
//! The snapshot tests additionally compare frames against stored references
//! with the SSIMULACRA2 perceptual metric.

use crate::common::*;
use unlit_wgpu::globals::{Camera, Globals, View};
use unlit_wgpu::mesh::{
    ChannelEncoding, MeshInstance, MeshMetadata, PositionStreamChannels, SecondaryFlags,
    compress_indices, compress_positions, quantize_colors,
};
use unlit_wgpu::pipeline::ShaderCache;
use unlit_wgpu::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, LitOptions, LitVariant, LitVertexChannels, MATERIAL_GROUP,
    MESH_METADATA_BINDING, PRIMARY_SLOT, SECONDARY_SLOT, Shader, SpecializedLitPipeline,
    VIEW_BINDING, supports_storage_buffers,
};
use unlit_wgpu::render_attachments::{
    RenderAttachments, create_render_target, default_depth_stencil_format, depth_clear,
    stencil_clear,
};
use unlit_wgpu::resources::{TextureExt, TextureView};
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use unlit_wgpu::specialize::{SpecializedPipeline, SurfaceKey};
use unlit_wgpu::texel_array::Array;
use unlit_wgpu::util::Hashed;
use unlit_wgpu_test_util::{Tolerance, gpu_tests, snapshot};
use zerocopy::IntoBytes;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
/// Background the tests clear to, as normalized sRGB components.
const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The tolerance the two cube snapshots are compared within.
///
/// Both frames are cubes seen at an angle: their silhouette and the edges
/// between their faces are hard, and in `lit_cubes_instanced` the cubes
/// overlap, so which of two cubes owns a pixel where they meet is the depth
/// test's to decide. Where two depths are equal, or where a multisampled edge
/// resolves differently, that is the implementation's choice rather than the
/// API's, so a second implementation moves a scatter of pixels along those
/// edges while drawing the same picture.
///
/// The perceptual score is weak on these frames, and alone would be the wrong
/// gate. A differing pixel sits on a hard edge, where the metric is most
/// sensitive, so the scatter costs far more than its area suggests — enough to
/// turn the metric's own ranking inside out: the WebGL2 frame of
/// `lit_cubes_instanced`, which draws the cubes in the right places with the
/// right colors, scores 48.97, *below* the 57.38 that shifting that whole frame
/// one pixel scores. A gate the metric cannot order correctly must not be the
/// one deciding these frames, which is what the outlier allowance is for.
///
/// The score is still worth bounding, just loosely: 45 sits below the lowest
/// score a correct frame reached rather than just under it, so it catches a
/// frame that has collapsed — the picture missing, or every vertex in the wrong
/// place — without re-deciding the edge scatter the outlier allowance already
/// judges. 57.38 passing it is the accepted cost, and the outlier allowance
/// catches that regression instead.
///
/// Measured against the stored snapshots, as score / outliers:
///
/// | frame | `lit_cubes_instanced` | `lit_cube_textured` |
/// |-------|-------------------------|-----------------------|
/// | Vulkan (where they were captured) | 95.88 / 0.000% | 100.00 / 0.000% |
/// | native GL | 73.11 / 0.157% | 100.00 / 0.000% |
/// | Chromium on WebGL2 | 48.97 / 0.385% | 79.28 / 0.118% |
/// | regression: shifted one pixel | 57.38 / 2.979% | 3.20 / 5.015% |
/// | regression: red cube 0.05 nearer | — / 1.750% | — |
/// | regression: drawn 10/255 darker | 28.79 / 54.688% | 27.49 / 50.635% |
/// | regression: the frame blanked | -301.81 / 54.688% | -415.07 / 50.781% |
///
/// 1% sits in the widest gap the outlier measurements leave: every correct frame
/// stays under 0.4%, and the mildest regression measured is over four times past
/// it. The cost is that a change staying within `channel_delta` of nearly every
/// pixel is invisible here — moving one cube 0.02 units nearer reads 0.60%,
/// under this bound — so this tolerance is for a second implementation, not a
/// substitute for the test's own assertions. What covers the property the frames
/// are really about is the structural check beside each one: that the three
/// cubes land in the order their transforms put them, each in its own color.
const CUBE_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(45.0),
    max_outliers: Some(0.01),
    channel_delta: 8,
};

mod fixture;
mod graph;
mod readback;
mod scene;
mod snapshots;

use self::{graph::*, readback::*, scene::*, snapshots::*};

// This file's share of the suite's registry: the suite root concatenates
// it with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
gpu_tests! {
    renders_a_cube_over_the_clear_color,
    base_color_reaches_the_frame,
    depth_ordering_hides_the_far_instance,
    msaa_produces_more_partial_coverage_than_no_msaa,
    lit_cube_matches_snapshot,
    instanced_cubes_match_snapshot,
    position_less_variant_draws_points_at_instance_origins,
    a_caller_module_replaces_the_built_in_stages,
    textured_cube_matches_snapshot,
    a_loaded_color_attachment_keeps_its_contents,
    resource_graph_rebuilds_a_dependent_after_a_resource_change,
    a_color_only_target_draws_a_cube,
    a_readback_handles_a_row_that_is_not_copy_aligned,
    the_readback_module_returns_a_texture_as_tight_rows,
    the_readback_module_returns_a_buffer_slice,
}
