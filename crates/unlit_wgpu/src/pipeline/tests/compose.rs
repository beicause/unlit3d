//! The composition tests: what a composed module contains, and that it is valid WGSL.

use super::*;

/// Every built-in entry shader composes into a module naga accepts, under each
/// state its flags can take.
///
/// The lit variants are covered by the slot suite, which composes every one of
/// them; this covers the others — the strokes under both states of their one
/// flag, and the stages that take no flags at all — because a slot enumerates
/// lit variants only.
#[test]
fn every_built_in_module_composes_to_valid_wgsl() {
    let cases: [(&str, &[(&'static str, bool)]); 4] = [
        ("line", &[("HARD_EDGE", false)]),
        ("line", &[("HARD_EDGE", true)]),
        ("blit", &[]),
        ("fullscreen_vertex", &[]),
    ];
    for (module, features) in cases {
        let wgsl = crate::compose::compose(module, features, None)
            .unwrap_or_else(|error| panic!("{module} composes: {error}"));
        assert_valid_wgsl(&wgsl, module);
    }
}

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

/// A caller's module stands in place of the built-in entry shader rather than
/// being added to it: what it does not call is not composed, so the built-in
/// shading is absent from the result.
#[test]
fn a_caller_module_replaces_the_built_in_entry_shader() {
    let shader = LitShader::new(
        "caller",
        "\
import package::lit_bindings;
import package::lit_vertex;

@vertex
public fn vs_main(in: lit_vertex::VertexInput) -> @builtin(position) vec4<f32> {
    return lit_bindings::camera.clip_from_world * vec4<f32>(in.model_2.xyz, 1.0);
}

@fragment
public fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 1.0, 1.0);
}
",
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

    let wgsl = compose_shader(&candidate, false).expect("the caller's module composes");
    // The caller's own entry points are the module's, under the names the
    // pipeline addresses.
    assert!(wgsl.contains("fn vs_main"));
    assert!(wgsl.contains("fn fs_main"));
    // The stages the caller did not call are not composed with it: the lighting
    // helper and the built-in fragment stage are absent, which is what makes the
    // module a replacement instead of an addition.
    assert!(!wgsl.contains("lit_color"), "{wgsl}");
    assert!(!wgsl.contains("fragment_normal"), "{wgsl}");
    assert!(!wgsl.contains("fs_stage"), "{wgsl}");
}

/// A caller's module composes the built-in stages out of the package modules:
/// the same two calls the built-in entry shader makes, with a displacement
/// between them, which is what a caller that only reshapes the geometry writes.
#[test]
fn a_caller_module_composes_the_built_in_stage_modules() {
    let shader = LitShader::new(
        "caller",
        "\
import package::lit_fragment;
import package::lit_vertex;

@vertex
public fn vs_main(in: lit_vertex::VertexInput, @builtin(vertex_index) vertex_index: u32) -> lit_vertex::VertexOut {
    var stage = lit_vertex::vertex_world(in, vertex_index);
    stage.world = stage.world + vec3<f32>(0.0, 0.25, 0.0);
    return lit_vertex::vertex_out(in, stage);
}

@fragment
public fn fs_main(in: lit_vertex::VertexOut) -> @location(0) vec4<f32> {
    return lit_fragment::fs_stage(in);
}
",
    );
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

    let built_in = compose_shader(&built_in, false).expect("the built-in shader composes");
    let composed = compose_shader(&candidate, false).expect("the caller's module composes");
    // The stage modules are the same ones the built-in entry shader imports, so
    // the composed declarations are too: a caller that calls both halves in the
    // built-in order gets the built-in shader.
    // The entry points are the composed module's own and keep their names; the
    // stage modules' items are imported, so wesl prefixes them with the path
    // they came from and only the tail is what a test can look for.
    for declaration in ["fn vs_main", "fn fs_main"] {
        assert!(composed.contains(declaration), "{declaration} missing");
        assert!(
            built_in.contains(declaration),
            "{declaration} missing from the built-in shader"
        );
    }
    for declaration in [
        "lit_vertex__1vertex_world(",
        "lit_vertex__1vertex_out(",
        "lit_fragment__1fs_stage(",
        "lit_fragment__1fragment_color(",
    ] {
        assert!(composed.contains(declaration), "{declaration} missing");
        assert!(
            built_in.contains(declaration),
            "{declaration} missing from the built-in shader"
        );
    }
    // The displacement is the caller's alone, so it is the one statement the
    // composed module carries that the built-in one does not.
    assert!(composed.contains("0.25"));
    assert!(!built_in.contains("0.25"));
}
