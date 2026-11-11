//! The composition tests: what a composed module contains, and that it is valid WGSL.

use super::*;

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
