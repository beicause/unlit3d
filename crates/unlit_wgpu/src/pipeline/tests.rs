use super::*;
use crate::compose::{FRAGMENT_HOOK, MODULE_HOOK, VERTEX_HOOK};
use crate::specialize::Variants;

/// A caller composes their own entry shader against the built-in package
/// with `wesl` directly: [`crate::shader`] is the `StaticPackage` that
/// resolves `import unlit_wgpu::…`.
#[test]
fn a_caller_composes_against_the_built_in_package() {
    let source = "\
import unlit_wgpu::camera::Camera;
import unlit_wgpu::mesh_compression;
import unlit_wgpu::mesh_metadata::MeshMetadata;

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(3) var<storage, read> mesh_meta: array<MeshMetadata>;

@vertex
fn vs_main(@location(0) position: vec4<f32>) -> @builtin(position) vec4<f32> {
    let decoded = mesh_compression::decode_position(position.xyz, mesh_meta[0]);
    return camera.clip_from_world * vec4<f32>(decoded, 1.0);
}
";
    // The caller's own module is virtual here; a real one would come from
    // a `FileResolver`. Anything not under the local namespace falls
    // through to the built-in package, which is how
    // `import unlit_wgpu::mesh_compression;` resolves.
    let namespace =
        wesl::syntax::ModulePath::new(wesl::syntax::PathOrigin::Absolute, vec!["app".to_owned()]);
    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Absolute,
        vec!["app".to_owned(), "main".to_owned()],
    );
    let mut virtual_modules = wesl::resolver::VirtualResolver::new();
    virtual_modules.add_module(
        wesl::syntax::ModulePath::new(wesl::syntax::PathOrigin::Absolute, vec!["main".into()]),
        source.into(),
    );

    let mut resolver = wesl::resolver::Router::new();
    resolver.mount_resolver(namespace, virtual_modules);
    resolver.mount_fallback_resolver({
        let mut packages = wesl::resolver::PackageResolver::new();
        packages.add_package(&crate::shader::PACKAGE);
        packages
    });

    let options = wesl::CompileOptions {
        keep_main: true,
        ..Default::default()
    };
    let wgsl = wesl::Compiler::new_with_resolver(options, resolver)
        .compile_module(&main_path)
        .expect("the caller's module composes")
        .syntax
        .to_string();

    // The imports resolved into the composed module.
    assert!(wgsl.contains("fn vs_main"));
    assert!(wgsl.contains("decode_position"));
}

/// A snippet is spliced into the built-in entry shader rather than
/// replacing it: the built-in stages still run, and the snippet's own
/// statements are what the marks were replaced with.
#[test]
fn a_caller_shader_is_spliced_into_the_built_in_source() {
    let shader = LitShader::new(
        "caller",
        "fn marker_module() -> f32 { return 0.5; }",
        "let marker_vertex = marker_module() * 0.25; world = world + vec3<f32>(0.0, marker_vertex, 0.0);",
        "let marker_fragment = 0.25; color = vec4<f32>(marker_fragment, color.g, color.b, color.a);",
    );
    let mut options = LitOptions::standard();
    options.shader = Some(shader);
    let candidate = variant(
        options,
        channels(
            Some(ChannelEncoding::UncompressedPosition),
            false,
            SecondaryFlags::empty(),
        ),
        false,
        false,
    );

    let wgsl = compose_shader(&candidate, false).expect("the caller's snippets compose");
    // The built-in stages are still there: a snippet takes part in them
    // rather than standing in for them.
    assert!(wgsl.contains("fn vs_main"));
    assert!(wgsl.contains("fn fs_main"));
    // Each snippet landed in the place it belongs to -- module scope before
    // either stage, the vertex stage before the fragment one -- and the
    // marks are gone rather than left in the composed module. A module
    // snippet declares what the stages call, which is the whole reason it
    // exists: here the vertex snippet uses a function it declared.
    let module_at = wgsl
        .find("fn marker_module")
        .expect("the module snippet spliced");
    let vertex_at = wgsl
        .find("marker_vertex")
        .expect("the vertex snippet spliced");
    let fragment_at = wgsl
        .find("marker_fragment")
        .expect("the fragment snippet spliced");
    assert!(module_at < vertex_at);
    assert!(vertex_at < fragment_at);
    assert!(!wgsl.contains(MODULE_HOOK));
    assert!(!wgsl.contains(VERTEX_HOOK));
    assert!(!wgsl.contains(FRAGMENT_HOOK));
}

/// An empty snippet leaves its stage exactly as the built-in shader has it.
#[test]
fn an_empty_snippet_leaves_the_built_in_stage_alone() {
    let built_in = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::UncompressedPosition),
            false,
            SecondaryFlags::empty(),
        ),
        false,
        false,
    );
    let mut options = LitOptions::standard();
    options.shader = Some(LitShader::new("empty", "", "", ""));
    let with_snippets = variant(
        options,
        channels(
            Some(ChannelEncoding::UncompressedPosition),
            false,
            SecondaryFlags::empty(),
        ),
        false,
        false,
    );

    assert_eq!(
        code_lines(&compose_shader(&built_in, false).expect("the built-in shader composes")),
        code_lines(&compose_shader(&with_snippets, false).expect("empty snippets compose")),
        "an empty snippet adds nothing to either stage"
    );
}

/// The non-blank lines of a composed module, in a fixed order.
///
/// The order is not the module's: the composed declarations come out of a
/// hash map, so two compilations of the same source can list them
/// differently. Line sets compare what a snippet adds or takes away, which
/// is the question here.
fn code_lines(wgsl: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = wgsl
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.sort_unstable();
    lines
}

/// The render target the test variants are built for.
fn surface() -> SurfaceKey {
    SurfaceKey {
        color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
        depth_stencil_format: Some(wgpu::TextureFormat::Depth24PlusStencil8),
        sample_count: 4,
    }
}

/// A variant from its caller policy and the facts a draw carries.
fn variant(
    mut options: LitOptions,
    channels: LitVertexChannels,
    base_color_texture: bool,
    morph: bool,
) -> LitVariant {
    // These variants draw without lights: their global group carries the
    // camera, the frame clock and the mesh metadata, and a lit one would
    // also demand the light arrays and the cluster. The lighting dimension
    // is enumerated on its own where a test composes both.
    options.unlit = true;
    LitVariant {
        options: Hashed::new(options),
        surface: surface(),
        channels,
        base_color_texture,
        morph,
        strip_index_format: None,
    }
}

/// The vertex channels with a compressed position and nothing else.
fn channels(
    position: Option<ChannelEncoding>,
    joints: bool,
    secondary: SecondaryFlags,
) -> LitVertexChannels {
    LitVertexChannels {
        position: PositionStreamChannels { position, joints },
        secondary,
    }
}

/// Every vertex-channel and policy combination that composes.
///
/// A sampled base-color texture implies a UV, and the joint and morph
/// channels imply the position they deform, so the invalid combinations are
/// skipped. Each surviving combination is then enumerated once per luminance
/// layout, because those describe the sampled texel rather than the vertex
/// layout and so are orthogonal to everything above — but only where a texel
/// is sampled at all.
///
/// The instance stream is not a dimension: every built-in variant binds one,
/// so a per-instance field never has to be declared separately. The array path
/// is not one either: it is the device's answer and is passed to
/// [`compose_shader`] rather than carried by the variant.
fn all_variants() -> Vec<LitVariant> {
    // A position is absent — leaving the geometry a point at the instance
    // origin — or present in one of the three encodings.
    let positions: &[Option<ChannelEncoding>] = &[
        None,
        Some(ChannelEncoding::CompressedPosition),
        Some(ChannelEncoding::UncompressedPosition),
        Some(ChannelEncoding::PlanarPosition),
    ];
    // The secondary stream carries a UV in either encoding, a color, a
    // normal, or any combination; an uncompressed UV is still a UV.
    let secondaries: &[SecondaryFlags] = &[
        SecondaryFlags::empty(),
        SecondaryFlags::UV,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV,
        SecondaryFlags::COLOR,
        SecondaryFlags::UV | SecondaryFlags::COLOR,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
        SecondaryFlags::NORMAL,
        SecondaryFlags::UV | SecondaryFlags::NORMAL,
        SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
        SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
    ];
    let mut variants = Vec::new();
    for position in positions {
        for joints in [false, true] {
            for secondary in secondaries {
                for base_color_texture in [false, true] {
                    for morph in [false, true] {
                        // The joint stream is part of the position stream,
                        // and a morph target displaces the position.
                        if (joints || morph) && position.is_none() {
                            continue;
                        }
                        // The base-color texture is sampled with the
                        // per-vertex UV.
                        if base_color_texture && !secondary.contains(SecondaryFlags::UV) {
                            continue;
                        }
                        let base = variant(
                            LitOptions::standard(),
                            channels(*position, joints, *secondary),
                            base_color_texture,
                            morph,
                        );
                        // A texel is expanded by one of the two luminance
                        // layouts, or not at all, and only a sampled texel
                        // can be expanded.
                        let luminance_layouts: &[BaseColorChannels] = if base_color_texture {
                            &[
                                BaseColorChannels::Rgba,
                                BaseColorChannels::Luminance,
                                BaseColorChannels::LuminanceAlpha,
                            ]
                        } else {
                            &[BaseColorChannels::Rgba]
                        };
                        for base_color in luminance_layouts {
                            // A cutoff compares its instance's alpha against
                            // the instance's own cutoff, so any variant can
                            // cut — textured or not.
                            for alpha_cutoff in [false, true] {
                                let mut candidate = base.clone();
                                candidate.options.update(|options| {
                                    options.base_color = *base_color;
                                    options.alpha_cutoff = alpha_cutoff;
                                });
                                variants.push(candidate);
                            }
                        }
                    }
                }
            }
        }
    }
    variants
}

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

#[test]
fn vertex_channels_for_vertex_buffer_maps_each_slot() {
    use crate::specialize::VertexBufferLayoutDesc;

    let attribute = |format, shader_location| wgpu::VertexAttribute {
        format,
        offset: 0,
        shader_location,
    };
    let layout = |step_mode, attributes: &[wgpu::VertexAttribute]| VertexBufferLayoutDesc {
        array_stride: 8,
        step_mode,
        attributes: attributes.into(),
    };
    let channels =
        |slot, layout: &VertexBufferLayoutDesc| LitVertexChannels::for_vertex_buffer(slot, layout);
    let position_of =
        |slot, layout: &VertexBufferLayoutDesc| channels(slot, layout).position.position;

    // The position slot: presence, and the compressed/uncompressed split.
    let compressed = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION)],
    );
    assert_eq!(
        position_of(PRIMARY_SLOT, &compressed),
        Some(ChannelEncoding::CompressedPosition)
    );
    let uncompressed = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Float32x3, location::POSITION)],
    );
    assert_eq!(
        position_of(PRIMARY_SLOT, &uncompressed),
        Some(ChannelEncoding::UncompressedPosition)
    );

    // The joint pair rides the position stream, so it is this slot's answer
    // rather than a slot of its own.
    let skinned_layout = layout(
        wgpu::VertexStepMode::Vertex,
        &[
            attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION),
            attribute(wgpu::VertexFormat::Uint32, location::JOINTS),
        ],
    );
    let skinned = channels(PRIMARY_SLOT, &skinned_layout);
    assert_eq!(
        skinned.position.position,
        Some(ChannelEncoding::CompressedPosition)
    );
    assert!(
        skinned.position.joints,
        "the joint indices are the evidence"
    );

    // The secondary slot: each channel is independent.
    let secondary = layout(
        wgpu::VertexStepMode::Vertex,
        &[
            attribute(wgpu::VertexFormat::Snorm16x2, location::UV),
            attribute(wgpu::VertexFormat::Unorm8x4, location::COLOR),
            attribute(wgpu::VertexFormat::Snorm16x2, location::NORMAL),
        ],
    );
    assert_eq!(
        channels(SECONDARY_SLOT, &secondary).secondary,
        SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL
    );
    let normal_only = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Snorm16x2, location::NORMAL)],
    );
    assert_eq!(
        channels(SECONDARY_SLOT, &normal_only).secondary,
        SecondaryFlags::NORMAL
    );
    let uncompressed_uv = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Float32x2, location::UV)],
    );
    assert_eq!(
        channels(SECONDARY_SLOT, &uncompressed_uv).secondary,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV
    );

    // The instance slot contributes nothing: the per-instance record is not
    // a vertex channel.
    let instance = layout(
        wgpu::VertexStepMode::Instance,
        &[attribute(
            wgpu::VertexFormat::Uint32,
            location::METADATA_INDEX,
        )],
    );
    assert_eq!(
        channels(INSTANCE_SLOT, &instance),
        LitVertexChannels::empty(),
        "the per-instance record is not a vertex channel"
    );

    // A whole layout is the union of its slots, and a slot that is not one of
    // the pipeline's own contributes nothing either way.
    let foreign = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Float32x3, 9)],
    );
    let whole = LitVertexChannels::for_vertex_layout(&[
        (PRIMARY_SLOT, skinned_layout),
        (SECONDARY_SLOT, uncompressed_uv),
        (INSTANCE_SLOT, instance),
        (7, foreign),
    ]);
    assert_eq!(
        whole.position.position,
        Some(ChannelEncoding::CompressedPosition)
    );
    assert!(whole.position.joints);
    assert_eq!(
        whole.secondary,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV
    );
}

#[test]
fn composes_each_variant_to_valid_wgsl() {
    for mut candidate in all_variants() {
        // Lighting is the one dimension the variant's channels do not already
        // cover: a lit draw reads the frame's lights and clusters with them,
        // which is why both are compiled here.
        for unlit in [false, true] {
            candidate.options.update(|options| options.unlit = unlit);
            // The array path is the device's, so both are exercised here.
            for texel_arrays in [false, true] {
                let wgsl = compose_shader(&candidate, texel_arrays)
                    .unwrap_or_else(|error| panic!("variant {candidate:?} failed: {error}"));
                assert!(wgsl.contains("fn vs_main"), "variant {candidate:?}");
                assert!(wgsl.contains("fn fs_main"), "variant {candidate:?}");
                // Disabled features must leave no conditional attributes behind.
                assert!(!wgsl.contains("@if"), "variant {candidate:?} kept @if");

                let vertex = candidate.channels;
                // Each channel appears exactly when the geometry carries it.
                assert_eq!(
                    wgsl.contains("base_color_tex"),
                    candidate.base_color_texture,
                    "variant {candidate:?}"
                );
                let compressed_uv = vertex.secondary.contains(SecondaryFlags::UV)
                    && !vertex.secondary.contains(SecondaryFlags::UNCOMPRESSED_UV);
                assert_eq!(
                    wgsl.contains("decode_uv"),
                    compressed_uv,
                    "variant {candidate:?}"
                );
                let compressed_position = matches!(
                    vertex.position.position,
                    Some(ChannelEncoding::CompressedPosition)
                );
                assert_eq!(
                    wgsl.contains("decode_position"),
                    compressed_position,
                    "variant {candidate:?}"
                );
                // A normal is decoded only where it shades, and only a lit
                // fragment shades with one.
                assert_eq!(
                    wgsl.contains("decode_normal"),
                    !candidate.options.unlit && vertex.secondary.contains(SecondaryFlags::NORMAL),
                    "variant {candidate:?}"
                );
                // The light arrays, the cluster lookup and the shading terms are
                // a lit draw's own.
                assert_eq!(
                    wgsl.contains("fragment_cluster_index"),
                    !candidate.options.unlit,
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("load_cell_word") && wgsl.contains("load_light"),
                    !candidate.options.unlit,
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("polygon_form_factor"),
                    !candidate.options.unlit,
                    "variant {candidate:?}"
                );
                // Every array read goes through an accessor, whose name survives
                // assembly unmangled as a suffix: the binding it reads is mangled
                // with its module path, so the accessor is what a variant can be
                // checked against.
                assert_eq!(
                    wgsl.contains("load_mesh_metadata"),
                    candidate.needs_metadata(),
                    "variant {candidate:?}"
                );
                // The pose arrays are the frame's, so they live in the global
                // group alongside the camera and the metadata.
                assert_eq!(
                    wgsl.contains("load_joint_matrix"),
                    candidate.needs_joints(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("load_morph_weight"),
                    candidate.needs_morphs(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("load_morph_delta"),
                    candidate.needs_morphs(),
                    "variant {candidate:?}"
                );
                // The path decides the resource type: storage buffers when the
                // device has them, textures when it does not. Neither path ever
                // leaves the other's declarations behind.
                let reads_an_array = candidate.needs_metadata()
                    || candidate.needs_joints()
                    || !candidate.options.unlit;
                if texel_arrays {
                    assert_eq!(
                        wgsl.matches("var<storage, read>").count(),
                        0,
                        "variant {candidate:?} kept a storage array"
                    );
                }
                assert_eq!(
                    wgsl.contains("texel_of"),
                    texel_arrays && reads_an_array,
                    "variant {candidate:?}"
                );
                // Without a position stream the vertex input declares no position
                // attribute, so the shader cannot read one.
                let vertex_input = wgsl
                    .split_once("struct VertexInput {")
                    .expect("the variant declares vertex inputs")
                    .1;
                let vertex_input = vertex_input
                    .split_once('}')
                    .expect("the vertex input struct ends")
                    .0;
                assert_eq!(
                    vertex_input.contains("position:"),
                    vertex.position.position.is_some(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    vertex_input.contains("joints:") && vertex_input.contains("weights:"),
                    candidate.needs_joints(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    vertex_input.contains("normal:"),
                    vertex.secondary.contains(SecondaryFlags::NORMAL),
                    "variant {candidate:?}"
                );
                // The instance record is unconditional, so its fields are
                // declared as plain attributes with no condition on them.
                assert!(
                    vertex_input.contains("joints_base:"),
                    "variant {candidate:?} declares no instance stream"
                );
                assert!(
                    vertex_input.contains("morph_base:"),
                    "variant {candidate:?} declares no instance stream"
                );
            }
        }
    }
}

/// A variant that contradicts itself must be rejected rather than composed
/// into a shader that cannot work.
///
/// The impossible states are no longer flags disagreeing with each other: an
/// encoding is a property of the channel it encodes, and the instance stream
/// is always bound, so the rejections left are the ones where a resource is
/// read that the variant does not carry — a texture sampled without a UV, a
/// deformation without a position, and a luminance layout without a texel.
#[test]
fn contradictory_variants_are_rejected() {
    let shape = LitOptions::standard;
    let compressed = Some(ChannelEncoding::CompressedPosition);
    let mut luminance_without_a_texture = shape();
    luminance_without_a_texture.base_color = BaseColorChannels::Luminance;
    for candidate in [
        // The base-color texture is sampled with the per-vertex UV.
        variant(
            shape(),
            channels(compressed, false, SecondaryFlags::empty()),
            true,
            false,
        ),
        // A deformation displaces the position.
        variant(
            shape(),
            channels(None, true, SecondaryFlags::empty()),
            false,
            false,
        ),
        variant(
            shape(),
            channels(None, false, SecondaryFlags::empty()),
            false,
            true,
        ),
        // A luminance layout describes a texel that is sampled.
        variant(
            luminance_without_a_texture,
            channels(compressed, false, SecondaryFlags::UV),
            false,
            false,
        ),
    ] {
        let result = std::panic::catch_unwind(|| compose_shader(&candidate, false));
        assert!(result.is_err(), "variant {candidate:?} must be rejected");
    }
}

#[test]
fn vertex_strides_follow_the_attribute_formats() {
    let layouts = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::CompressedPosition),
            false,
            SecondaryFlags::UV,
        ),
        false,
        false,
    )
    .vertex_buffer_layouts();
    let position = layouts[PRIMARY_SLOT as usize]
        .as_ref()
        .expect("position slot");
    assert_eq!(position.array_stride, wgpu::VertexFormat::Snorm16x4.size());

    let secondary = layouts[SECONDARY_SLOT as usize].as_ref().expect("uv slot");
    assert_eq!(secondary.array_stride, wgpu::VertexFormat::Snorm16x2.size());

    let instance = layouts[INSTANCE_SLOT as usize]
        .as_ref()
        .expect("instance slot");
    // Every variant reads the same instance buffer, so the stride is the
    // whole record even when the variant declares only some of its fields.
    assert_eq!(
        instance.array_stride,
        std::mem::size_of::<crate::mesh::MeshInstance>() as u64
    );
    assert_eq!(instance.step_mode, wgpu::VertexStepMode::Instance);
}

/// An uncompressed channel widens its slot to the full-precision format and
/// drops the metadata bindings it no longer needs — but the instance stream
/// is still bound, because every built-in variant binds one.
#[test]
fn uncompressed_channels_widen_their_slots() {
    let candidate = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::UncompressedPosition),
            false,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
        ),
        true,
        false,
    );
    assert!(!candidate.needs_metadata());

    let layouts = candidate.vertex_buffer_layouts();
    let position = layouts[PRIMARY_SLOT as usize]
        .as_ref()
        .expect("position slot");
    assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x3.size());
    assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x3);

    let secondary = layouts[SECONDARY_SLOT as usize].as_ref().expect("uv slot");
    assert_eq!(
        secondary.array_stride,
        wgpu::VertexFormat::Float32x2.size() + wgpu::VertexFormat::Unorm8x4.size()
    );

    // The instance stream is bound for every variant, so the slot exists even
    // though these channels are read entirely from full-precision attributes.
    assert!(layouts[INSTANCE_SLOT as usize].is_some());
}

/// A planar channel narrows its slot to the two-component format and needs
/// no metadata either, exactly as any other full-precision position.
#[test]
fn planar_channels_narrow_their_slots() {
    let candidate = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::PlanarPosition),
            false,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
        ),
        true,
        false,
    );
    assert!(!candidate.needs_metadata());

    let layouts = candidate.vertex_buffer_layouts();
    let position = layouts[PRIMARY_SLOT as usize]
        .as_ref()
        .expect("position slot");
    assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x2.size());
    assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x2);
}

#[test]
fn secondary_slot_follows_its_channels() {
    let stride = |secondary: SecondaryFlags| {
        variant(
            LitOptions::standard(),
            channels(None, false, secondary),
            false,
            false,
        )
        .vertex_buffer_layouts()[SECONDARY_SLOT as usize]
            .as_ref()
            .map(|layout| layout.array_stride)
    };

    let uv = wgpu::VertexFormat::Snorm16x2.size();
    let uv_uncompressed = wgpu::VertexFormat::Float32x2.size();
    let color = wgpu::VertexFormat::Unorm8x4.size();
    let normal = wgpu::VertexFormat::Snorm16x2.size();

    // No channel: the slot disappears entirely, so a variant never binds a
    // buffer the shader does not declare.
    assert_eq!(stride(SecondaryFlags::empty()), None);
    assert_eq!(stride(SecondaryFlags::UV), Some(uv));
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV),
        Some(uv_uncompressed)
    );
    assert_eq!(stride(SecondaryFlags::COLOR), Some(color));
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::COLOR),
        Some(uv + color)
    );
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR),
        Some(uv_uncompressed + color)
    );
    // A normal is always the same two lanes, whatever else the stream
    // carries, and it sits in attribute order last.
    assert_eq!(stride(SecondaryFlags::NORMAL), Some(normal));
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL),
        Some(uv + color + normal)
    );
}

/// The position slot follows its channels, so a position-less variant binds
/// no position buffer at all.
#[test]
fn position_slot_follows_its_channels() {
    let position = |position: Option<ChannelEncoding>, joints: bool| {
        variant(
            LitOptions::standard(),
            channels(position, joints, SecondaryFlags::empty()),
            false,
            false,
        )
        .vertex_buffer_layouts()[PRIMARY_SLOT as usize]
            .as_ref()
            .map(|layout| (layout.array_stride, layout.step_mode))
    };

    assert_eq!(
        position(None, false),
        None,
        "no position stream means no position slot"
    );
    assert_eq!(
        position(Some(ChannelEncoding::CompressedPosition), false),
        Some((
            wgpu::VertexFormat::Snorm16x4.size(),
            wgpu::VertexStepMode::Vertex
        ))
    );
    assert_eq!(
        position(Some(ChannelEncoding::UncompressedPosition), false),
        Some((
            wgpu::VertexFormat::Float32x3.size(),
            wgpu::VertexStepMode::Vertex
        ))
    );
}

/// The instance slot's GPU attribute offsets and stride must match the CPU
/// struct. This is the vertex-buffer equivalent of the shader-layout
/// validation that `const_shader_layout` gives the uniform and storage
/// structs: vertex attributes are addressed by the offsets declared here, not
/// by WGSL alignment rules.
#[test]
fn instance_vertex_layout_matches_the_struct() {
    use crate::mesh::MeshInstance;
    use core::mem::{offset_of, size_of};

    let layouts = variant(
        LitOptions::standard(),
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
        .expect("instance slot");

    assert_eq!(
        instance.array_stride,
        size_of::<MeshInstance>() as u64,
        "the instance stride must step exactly one MeshInstance"
    );

    let model = offset_of!(MeshInstance, model) as u64;
    let vec4 = size_of::<[f32; 4]>() as u64;
    // The record has to be tightly packed: `IntoBytes` rejects a type with
    // padding, so the sum of the record's field formats is the whole
    // stride. This variant neither cuts nor expands, so its layout omits
    // both words the shader would not declare; the declared formats plus
    // those two omitted words still cover the record exactly.
    let declared: u64 = instance
        .attributes
        .iter()
        .map(|attribute| attribute.format.size())
        .sum();
    assert_eq!(
        declared + 2 * wgpu::VertexFormat::Float32.size(),
        instance.array_stride,
        "the instance record carries no padding"
    );

    let expected = [
        model,
        model + vec4,
        model + 2 * vec4,
        offset_of!(MeshInstance, base_color) as u64,
        offset_of!(MeshInstance, joints_base) as u64,
        offset_of!(MeshInstance, metadata_index) as u64,
        offset_of!(MeshInstance, morph_base) as u64,
    ];
    let actual: Vec<u64> = instance.attributes.iter().map(|a| a.offset).collect();
    assert_eq!(
        actual, expected,
        "attribute offsets must line up with the MeshInstance fields"
    );

    let locations: Vec<u32> = instance
        .attributes
        .iter()
        .map(|a| a.shader_location)
        .collect();
    assert_eq!(
        locations,
        [
            location::MODEL_0,
            location::MODEL_1,
            location::MODEL_2,
            location::BASE_COLOR,
            location::JOINTS_BASE,
            location::METADATA_INDEX,
            location::MORPH_BASE,
        ]
    );
    // The matrix columns are floats, the color is quantized, and the two
    // bases and the metadata index are integers: each addresses a slot in
    // one of the frame's shared arrays.
    let expected_formats = [
        wgpu::VertexFormat::Float32x4,
        wgpu::VertexFormat::Float32x4,
        wgpu::VertexFormat::Float32x4,
        wgpu::VertexFormat::Unorm8x4,
        wgpu::VertexFormat::Uint32,
        wgpu::VertexFormat::Uint32,
        wgpu::VertexFormat::Uint32,
    ];
    let formats: Vec<wgpu::VertexFormat> = instance.attributes.iter().map(|a| a.format).collect();
    assert_eq!(formats, expected_formats, "{instance:?}");
}

/// A cutting variant's instance layout declares the cutoff and an expanding
/// one declares the hull width; a plain one omits both, matching the
/// shader's own conditional input declarations. The other instance inputs
/// are unconditional in the shader, so the layout declares them for every
/// variant.
#[test]
fn instance_layout_declares_the_cutoff_and_the_hull_width_only_when_read() {
    let locations = |alpha_cutoff: bool, expanded: bool| {
        let mut options = LitOptions::standard();
        options.alpha_cutoff = alpha_cutoff;
        options.expanded = expanded;
        variant(
            options,
            channels(None, false, SecondaryFlags::COLOR | SecondaryFlags::NORMAL),
            false,
            false,
        )
        .vertex_buffer_layouts()[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot")
            .attributes
            .iter()
            .map(|a| a.shader_location)
            .collect::<Vec<u32>>()
    };

    let plain = [
        location::MODEL_0,
        location::MODEL_1,
        location::MODEL_2,
        location::BASE_COLOR,
        location::JOINTS_BASE,
        location::METADATA_INDEX,
        location::MORPH_BASE,
    ];
    let mut cutting = plain.to_vec();
    cutting.insert(6, location::CUTOFF);
    let mut expanding = plain.to_vec();
    expanding.push(location::HULL_WIDTH);
    let mut both = cutting.clone();
    both.push(location::HULL_WIDTH);

    assert_eq!(
        locations(false, false),
        plain,
        "a plain variant declares neither the cutoff nor the hull width"
    );
    assert_eq!(
        locations(true, false),
        cutting,
        "a cutting variant declares the cutoff"
    );
    assert_eq!(
        locations(false, true),
        expanding,
        "an expanding variant declares the hull width"
    );
    assert_eq!(
        locations(true, true),
        both,
        "one variant can do both, and the width stays last"
    );
}

/// The hull width rides the instance stream, so the vertex stage reads the
/// instance attribute and pushes the vertex along the transformed normal.
#[test]
fn expanded_flag_pushes_the_vertex_along_its_normal() {
    let composed = |expanded: bool| {
        let mut options = LitOptions::standard();
        options.expanded = expanded;
        compose_shader(
            &variant(
                options,
                channels(
                    Some(ChannelEncoding::CompressedPosition),
                    false,
                    SecondaryFlags::NORMAL,
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
        !plain.contains("hull_width"),
        "a variant that expands by nothing declares no hull width"
    );

    let expanded = composed(true);
    assert!(
        expanded.contains("@location(14)\n    hull_width: f32"),
        "the shader reads the hull width from its instance attribute"
    );
    assert!(
        expanded.contains("let reach = length(outward);"),
        "a degenerate instance matrix is checked before it can be normalized"
    );
    assert!(
        expanded.contains("world += outward / reach * in.hull_width"),
        "the vertex is pushed along its transformed normal"
    );
}

/// A variant cannot expand without a direction to expand in, so composing
/// one over a mesh with no normal channel is a mistake worth stopping at.
#[test]
#[should_panic(expected = "`EXPANDED` requires a normal channel")]
fn an_expanding_variant_without_normals_is_rejected() {
    let mut options = LitOptions::standard();
    options.expanded = true;
    let _ = compose_shader(
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
    );
}
