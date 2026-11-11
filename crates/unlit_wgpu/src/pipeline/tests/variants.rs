//! The tests that flip one policy flag and check the composed source follows.

use super::*;

/// `srgb_to_linear_output` decides what the fragment writes: plain values
/// when the inputs are already linear, a linear encoding when they are
/// sRGB-encoded. Both variants carry the same single entry point.
#[test]
fn srgb_output_flag_switches_the_conversion() {
    let composed = |srgb_to_linear_output: bool| {
        let mut options = LitOptions::standard();
        options.srgb_to_linear_output = srgb_to_linear_output;
        compose_shader(
            &variant(options, LitVertexChannels::empty(), false, false),
            false,
        )
        .expect("compose")
    };

    let plain = composed(false);
    assert!(plain.contains("fn fs_main"), "one entry point either way");
    assert!(
        !plain.contains("srgb_to_linear(color.r)"),
        "a plain target stores what it is given"
    );

    let converted = composed(true);
    // Only RGB converts: alpha is coverage, not color.
    assert!(converted.contains("srgb_to_linear(color.r)"));
    assert!(converted.contains("srgb_to_linear(color.g)"));
    assert!(converted.contains("srgb_to_linear(color.b)"));
    assert!(converted.contains("color.a"), "alpha passes through");
    assert!(!converted.contains("srgb_to_linear(color.a)"));
}

/// The two luminance layouts say how a sampled texel expands, so they reach
/// the fragment as the expansion itself — and only the one texel the variant
/// samples, never the color the target writes.
#[test]
fn luminance_layouts_expand_a_sampled_texel() {
    let composed = |base_color: BaseColorChannels| {
        let mut options = LitOptions::standard();
        options.base_color = base_color;
        compose_shader(
            &variant(
                options,
                channels(
                    Some(ChannelEncoding::CompressedPosition),
                    false,
                    SecondaryFlags::UV,
                ),
                true,
                false,
            ),
            false,
        )
        .expect("compose")
    };

    let full = composed(BaseColorChannels::Rgba);
    assert!(
        !full.contains("srgb_to_linear(texel.r)"),
        "a four-channel texture is uploaded in an sRGB format, so the \
             sampler decodes it"
    );

    let luma = composed(BaseColorChannels::Luminance);
    assert!(
        luma.contains("vec3<f32>(srgb_to_linear(texel.r))"),
        "a luminance texel feeds every color channel, decoded"
    );
    assert!(
        luma.contains("texel.a"),
        "a luminance texture has no alpha, so the channel reads as one"
    );

    let luma_alpha = composed(BaseColorChannels::LuminanceAlpha);
    assert!(luma_alpha.contains("vec3<f32>(srgb_to_linear(texel.r))"));
    assert!(
        luma_alpha.contains("texel.g") && !luma_alpha.contains("texel.a"),
        "the second channel of a luminance-alpha texture is its alpha, and \
             coverage never converts"
    );
}

/// The cutoff rides the instance stream, so the fragment that discards reads
/// the instance attribute. Every built-in variant declares the instance
/// stream, so the attribute is present whether or not the variant cuts.
#[test]
fn alpha_cutoff_flag_discards_a_fragment_below_the_cutoff() {
    let composed = |alpha_cutoff: bool| {
        let mut options = LitOptions::standard();
        options.alpha_cutoff = alpha_cutoff;
        compose_shader(
            &variant(
                options,
                channels(
                    Some(ChannelEncoding::CompressedPosition),
                    false,
                    SecondaryFlags::empty(),
                ),
                false,
                false,
            ),
            false,
        )
        .expect("compose")
    };

    let plain = composed(false);
    assert!(
        !plain.contains("discard"),
        "an opaque material keeps every fragment it is given"
    );
    assert!(
        !plain.contains("cutoff"),
        "a variant that never cuts declares no cutoff input"
    );

    let cut = composed(true);
    let layouts = variant(
        {
            let mut options = LitOptions::standard();
            options.alpha_cutoff = true;
            options
        },
        channels(
            Some(ChannelEncoding::CompressedPosition),
            false,
            SecondaryFlags::empty(),
        ),
        false,
        false,
    )
    .vertex_buffer_layouts();
    let instance = layouts[INSTANCE_SLOT as usize]
        .as_ref()
        .expect("a built-in variant always reads the instance stream");
    assert!(
        instance
            .attributes
            .iter()
            .any(|attribute| attribute.shader_location == location::CUTOFF
                && attribute.format == wgpu::VertexFormat::Float32),
        "the cutoff is one instance's own value, so it rides the instance stream"
    );
    assert!(
        cut.contains("@location(11)\n    cutoff: f32"),
        "the shader reads the cutoff from its instance attribute"
    );
    assert!(
        cut.contains("if color.a < in.cutoff"),
        "a fragment below the cutoff is dropped rather than blended"
    );
    assert!(cut.contains("discard"), "dropped fragments write nothing");
}

/// A luminance layout describes a sampled texel, so it is meaningless without
/// one. The two layouts can no longer be selected together, because the
/// channel count is an enum rather than two independent flags.
#[test]
#[should_panic(
    expected = "`BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` require \
                    a sampled base-color texture"
)]
fn a_luminance_layout_without_a_texture_is_rejected() {
    let mut options = LitOptions::standard();
    options.base_color = BaseColorChannels::Luminance;
    let _ = compose_shader(
        &variant(
            options,
            channels(
                Some(ChannelEncoding::CompressedPosition),
                false,
                SecondaryFlags::UV,
            ),
            false,
            false,
        ),
        false,
    );
}

/// A target with no depth attachment yields a pipeline with no depth state,
/// so `wgpu`'s compatibility check against the pass cannot fail: a pass with
/// no depth attachment rejects any pipeline that declares one, regardless of
/// what that state compares or writes. The policy survives untouched: a
/// target that has an attachment runs it against that attachment's format.
#[test]
fn a_depth_less_surface_drops_the_state_and_a_deep_one_restores_it() {
    let surface = |depth: Option<wgpu::TextureFormat>| SurfaceKey {
        color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
        depth_stencil_format: depth,
        sample_count: 1,
    };
    let policy = LitOptions::standard().depth_stencil;

    let with_depth = surface(Some(wgpu::TextureFormat::Depth32Float));
    let state = policy.state(with_depth.depth_stencil_format.expect("the test's premise"));
    assert_eq!(state.format, wgpu::TextureFormat::Depth32Float);
    assert_eq!(state.depth_write_enabled, Some(true));
    assert_eq!(state.depth_compare, Some(wgpu::CompareFunction::Greater));
}

/// The variant is a cache key like any other: one pipeline per target,
/// reused when the same target comes back.
#[test]
fn a_surface_variant_caches_one_pipeline_per_target() {
    let (device, _queue) = crate::util::test::noop_device();
    let surface = SurfaceKey {
        color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
        depth_stencil_format: Some(wgpu::TextureFormat::Depth24Plus),
        sample_count: 4,
    };
    let make = |surface: SurfaceKey| {
        let mut candidate = variant(
            LitOptions::standard(),
            LitVertexChannels::empty(),
            false,
            false,
        );
        candidate.surface = surface;
        candidate
    };

    let mut variants = Variants::<wgpu::RenderPipeline, LitVariant>::new(&device);
    let first = variants.specialize(make(surface));
    assert_eq!(
        variants.specialize(make(surface)),
        first,
        "the same target reuses its pipeline"
    );

    // A different color format is a different target, so it compiles its own
    // variant rather than reusing the first one.
    let other_format = SurfaceKey {
        color_format: wgpu::TextureFormat::Bgra8Unorm,
        ..surface
    };
    assert_ne!(variants.specialize(make(other_format)), first);
    assert_eq!(
        variants.get(first).descriptor().surface.color_format,
        surface.color_format
    );
}
