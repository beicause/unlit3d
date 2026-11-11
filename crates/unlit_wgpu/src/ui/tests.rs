use super::*;

/// The UI options for a device-free test. Only the policy these tests
/// assert on matters, so the depth-stencil format is the
/// device-independent placeholder.
fn ui_options_for_tests(srgb_to_linear_output: bool) -> LitOptions {
    let mut options = LitOptions::standard();
    apply_ui_settings(&mut options, srgb_to_linear_output);
    options
}

/// A UI variant over the device-free options, for tests that assert on
/// what a variant derives rather than on a compiled pipeline.
fn ui_variant_for_tests(srgb_to_linear_output: bool) -> LitVariant {
    LitVariant {
        options: Hashed::new(ui_options_for_tests(srgb_to_linear_output)),
        surface: SurfaceKey {
            color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            depth_stencil_format: None,
            sample_count: 1,
        },
        channels: ui_channels(),
        base_color_texture: true,
        morph: false,
        strip_index_format: None,
    }
}

#[test]
fn ui_variant_is_screen_space_and_uncompressed() {
    let variant = ui_variant_for_tests(false);
    let channels = variant.channels;
    assert_eq!(
        channels.position.position,
        Some(ChannelEncoding::PlanarPosition),
        "screen-space vertices are written at full precision and in the plane"
    );
    assert!(!channels.position.joints, "a UI mesh is never skinned");
    assert_eq!(
        channels.secondary,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR
    );
    assert!(variant.base_color_texture);
    // Nothing is compressed, so no draw decodes through a metadata index:
    // the instance stream carries no per-mesh state the UI would read.
    assert!(!variant.needs_metadata());
    assert!(!variant.needs_joints());
    assert!(!variant.needs_morphs());
    // The instance stream is declared for every unlit variant, so a UI
    // draw binds the default record.
    assert!(variant.vertex_buffer_layouts()[INSTANCE_SLOT as usize].is_some());
    let options = &variant.options;
    // Overlaid rather than depth-tested: the policy says what a target
    // with an attachment would run, and the surface says whether it has
    // one.
    assert!(!options.depth_stencil.depth_write_enabled);
    assert_eq!(
        options.depth_stencil.depth_compare,
        wgpu::CompareFunction::Always
    );
    // egui's colors premultiply their own alpha.
    let blend = options.blend.expect("the UI variant blends");
    assert_eq!(blend.color.src_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
}

/// The conversion flag follows the parameter, since it depends on the
/// target's format, not the UI.
#[test]
fn srgb_flag_follows_the_parameter() {
    assert!(!ui_options_for_tests(false).srgb_to_linear_output);
    assert!(ui_options_for_tests(true).srgb_to_linear_output);
}

/// The projection maps the viewport's corners onto clip space, with Y
/// flipped: egui's origin is the top left, clip space's the bottom left.
#[test]
fn screen_view_maps_the_viewport_corners() {
    let camera = screen_view([256.0, 192.0]);
    for (point, expected) in [([0.0, 0.0], [-1.0, 1.0]), ([256.0, 192.0], [1.0, -1.0])] {
        let clip = camera.clip_from_world * glam::Vec4::new(point[0], point[1], 0.0, 1.0);
        let got = [clip.x / clip.w, clip.y / clip.w];
        assert!(
            (got[0] - expected[0]).abs() < 1e-5 && (got[1] - expected[1]).abs() < 1e-5,
            "point {point:?} projected to {got:?}, expected {expected:?}"
        );
    }
    // The viewport the same points describe is the view bound beside it.
    assert_eq!(screen_viewport([256.0, 192.0]), View::new(256.0, 192.0));
}

/// A clip rectangle reaching off the target is clamped rather than wrapping
/// into a huge `u32`, and its far edge never falls below its near edge.
#[test]
fn scissor_rect_clamps_to_the_target() {
    let screen = ScreenDescriptor {
        size_in_pixels: [64, 48],
        pixels_per_point: 1.0,
    };
    let rect = scissor_rect(
        egui::Rect::from_two_pos(egui::Pos2::new(-20.0, -8.0), egui::Pos2::new(200.0, 200.0)),
        screen,
    );
    assert_eq!(rect.x, 0);
    assert_eq!(rect.y, 0);
    // Both edges are clamped to the target, so a clip rectangle larger
    // than it covers exactly the target rather than overrunning it.
    assert_eq!(rect.width, 64);
    assert_eq!(rect.height, 48);

    // A rectangle entirely off the target collapses to nothing instead of
    // producing a negative width.
    let off = scissor_rect(
        egui::Rect::from_two_pos(egui::Pos2::new(-40.0, -40.0), egui::Pos2::new(-10.0, -10.0)),
        screen,
    );
    assert_eq!((off.x, off.y, off.width, off.height), (0, 0, 0, 0));
}

/// A clip rectangle is stated in logical points, so at a density above one
/// it must cover that many physical pixels — the conversion egui-wgpu
/// performs. Getting it wrong clips the UI short of its own bounds.
#[test]
fn scissor_rect_scales_points_to_physical_pixels() {
    let screen = ScreenDescriptor {
        size_in_pixels: [256, 192],
        pixels_per_point: 2.0,
    };
    // A 16x8-point clip at (8, 4) is 32x16 physical pixels at (16, 8).
    let clip = egui::Rect::from_min_size(egui::Pos2::new(8.0, 4.0), egui::Vec2::new(16.0, 8.0));
    let rect = scissor_rect(clip, screen);
    assert_eq!(
        rect,
        ScissorRect {
            x: 16,
            y: 8,
            width: 32,
            height: 16
        }
    );

    // Where an unscaled conversion would have ended, in pixels: egui's own
    // point coordinates taken as pixels. The scaled rectangle extends
    // beyond it, so an unscaled clip would have cut that strip away.
    let unscaled_end = clip.max.x as u32;
    assert!(
        unscaled_end < rect.x + rect.width,
        "the scaled rectangle must reach past where an unscaled one would \
             have ended ({unscaled_end} px), but it ends at {} px",
        rect.x + rect.width
    );
}

/// Fractional points round to whole pixels rather than truncating, so a
/// clip edge does not drift by up to a pixel.
#[test]
fn scissor_rect_rounds_fractional_points() {
    let screen = ScreenDescriptor {
        size_in_pixels: [64, 64],
        pixels_per_point: 1.0,
    };
    let rect = scissor_rect(
        egui::Rect::from_min_size(egui::Pos2::new(1.4, 2.6), egui::Vec2::new(4.0, 4.0)),
        screen,
    );
    assert_eq!((rect.x, rect.y), (1, 3));

    let rounded_up = scissor_rect(
        egui::Rect::from_min_size(egui::Pos2::new(1.6, 2.4), egui::Vec2::new(4.0, 4.0)),
        screen,
    );
    assert_eq!((rounded_up.x, rounded_up.y), (2, 2));
}

/// The point size the projection takes is the physical size divided by the
/// density — the opposite conversion from the scissor rectangle's.
#[test]
fn size_in_points_divides_by_the_density() {
    let screen = ScreenDescriptor {
        size_in_pixels: [256, 192],
        pixels_per_point: 2.0,
    };
    assert_eq!(screen.size_in_points(), [128.0, 96.0]);
}

/// A screen descriptor matching the default test target.
fn test_screen() -> ScreenDescriptor {
    ScreenDescriptor {
        size_in_pixels: [256, 192],
        pixels_per_point: 1.0,
    }
}

/// Tag `primitives` with the local source: these tests exercise packing,
/// not the namespaces a frame arrives from.
fn local(primitives: &[egui::ClippedPrimitive]) -> Vec<(Source, &egui::ClippedPrimitive)> {
    primitives
        .iter()
        .map(|primitive| (Source::LOCAL, primitive))
        .collect()
}

/// One tessellated primitive carrying `vertices` points, clipped to the
/// whole screen.
fn primitive(texture: egui::TextureId, vertices: usize) -> egui::ClippedPrimitive {
    let mut mesh = egui::epaint::Mesh::with_texture(texture);
    for index in 0..vertices {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(index as f32, index as f32 * 2.0),
            uv: egui::pos2(0.25, 0.75),
            color: egui::Color32::from_rgba_premultiplied(1, 2, 3, 4),
        });
    }
    // A degenerate triangle per vertex, enough to exercise the index path.
    for index in 0..vertices.saturating_sub(2) {
        mesh.indices
            .extend([index as u32, index as u32 + 1, index as u32 + 2]);
    }
    egui::ClippedPrimitive {
        clip_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(256.0, 192.0)),
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }
}

/// The packing lands each primitive's bytes where its draw says, and the
/// two vertex streams hold the values egui put in.
#[test]
fn packing_places_each_primitive_where_its_draw_says() {
    let primitives = [primitive(egui::TextureId::default(), 4)];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut draws = Vec::new();
    let tagged = local(&primitives);
    let counts = measure(tagged.iter().copied());
    pack_geometry(
        tagged.iter().copied(),
        test_screen(),
        &HashMap::new(),
        counts,
        &mut vertices,
        &mut indices,
        &mut draws,
    );

    assert_eq!(draws.len(), 1, "one mesh is one draw");
    let draw = &draws[0];
    assert_eq!(draw.indices, 0..6, "four vertices make two triangles");
    // This primitive starts at the pool's first vertex, so baking its
    // offset in leaves the indices as egui wrote them.
    assert_eq!(
        packed_indices(&indices),
        vec![0, 1, 2, 1, 2, 3],
        "the indices address the pool without an offset"
    );

    // The vertex buffer holds the primary stream then the interleaved
    // secondary stream, each entry one stride wide.
    let secondary_start = 4 * POSITION_STRIDE;
    assert_eq!(vertices.len(), secondary_start + 4 * SECONDARY_STRIDE);
    let positions: Vec<f32> = vertices[..secondary_start]
        .as_chunks::<{ size_of::<f32>() }>()
        .0
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .collect();
    assert_eq!(
        positions,
        vec![0.0, 0.0, 1.0, 2.0, 2.0, 4.0, 3.0, 6.0],
        "each position is x and y; the shader supplies the zero z"
    );

    // The colors are egui's premultiplied bytes, unchanged.
    let first_color = secondary_start + size_of::<egui::Vec2>();
    assert_eq!(
        &vertices[first_color..first_color + size_of::<egui::Color32>()],
        &[1, 2, 3, 4]
    );

    // The indices are this primitive's, starting at zero for the first.
    let indices: Vec<u32> = indices
        .as_chunks::<{ size_of::<u32>() }>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    assert_eq!(indices, vec![0, 1, 2, 1, 2, 3]);
}

/// A second primitive's indices are offset by the first's vertex count, so
/// one base vertex addresses both streams.
#[test]
fn a_later_primitive_is_offset_by_the_earlier_one() {
    let primitives = [
        primitive(egui::TextureId::default(), 3),
        primitive(egui::TextureId::default(), 3),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut draws = Vec::new();
    let tagged = local(&primitives);
    let counts = measure(tagged.iter().copied());
    pack_geometry(
        tagged.iter().copied(),
        test_screen(),
        &HashMap::new(),
        counts,
        &mut vertices,
        &mut indices,
        &mut draws,
    );

    assert_eq!(draws.len(), 2);
    assert_eq!(draws[0].indices, 0..3);
    assert_eq!(draws[1].indices, 3..6);
    // The second primitive's vertices start at ordinal 3, so its local
    // indices are shifted by 3 — the offset a device without `base_vertex`
    // could not take from the draw.
    assert_eq!(
        packed_indices(&indices),
        vec![0, 1, 2, 3, 4, 5],
        "the second primitive's indices are offset to the pool"
    );
}

/// The indices `pack_geometry` wrote, as `u32`s.
fn packed_indices(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<{ size_of::<u32>() }>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect()
}

/// The byte buffers are cleared and refilled, not reallocated, so a caller
/// that keeps them uploads without allocating.
#[test]
fn packing_reuses_the_vertex_and_index_buffers() {
    let big = [primitive(egui::TextureId::default(), 8)];
    let small = [primitive(egui::TextureId::default(), 3)];

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut draws = Vec::new();
    let big_tagged = local(&big);
    let big_counts = measure(big_tagged.iter().copied());
    pack_geometry(
        big_tagged.iter().copied(),
        test_screen(),
        &HashMap::new(),
        big_counts,
        &mut vertices,
        &mut indices,
        &mut draws,
    );
    let vertex_capacity = vertices.capacity();
    let index_capacity = indices.capacity();

    // A later, smaller frame keeps both allocations rather than taking new
    // ones; a fresh `Vec` per frame would have exactly its own length.
    let small_tagged = local(&small);
    let small_counts = measure(small_tagged.iter().copied());
    pack_geometry(
        small_tagged.iter().copied(),
        test_screen(),
        &HashMap::new(),
        small_counts,
        &mut vertices,
        &mut indices,
        &mut draws,
    );
    assert_eq!(
        vertices.capacity(),
        vertex_capacity,
        "the vertex scratch buffer is reused"
    );
    assert_eq!(
        indices.capacity(),
        index_capacity,
        "the index scratch buffer is reused"
    );
    assert_eq!(draws.len(), 1, "and the stale second draw is gone");
}
