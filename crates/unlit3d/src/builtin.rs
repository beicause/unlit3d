//! The plumbing the crate's built-in frame sources share.
//!
//! Every built-in source compiles a module of the `unlit_wgpu` WESL package for
//! a target it only learns about at draw time. That is the same call whichever
//! source makes it, so it lives here instead of once per source.
//!
//! The uniforms a draw binds are not a source's: the frame keeps them in one
//! registry, so no built-in source creates or rewrites one.

/// Compile one module of the built-in WESL package into a shader module.
///
/// `keep_main` is what leaves the module's own declarations - the entry points
/// among them - in the composed WGSL under their own names. The named
/// conditional-compilation features are set before composing, which is how a
/// built-in source carries a variant of its module.
pub(crate) fn shader_module(
    device: &wgpu::Device,
    module: &str,
    label: &str,
    features: &[(&'static str, bool)],
) -> wgpu::ShaderModule {
    let wgsl = compose(module, features)
        .unwrap_or_else(|error| panic!("the built-in unlit_wgpu shader compiles: {error}"));
    unlit_wgpu::util::shader_module(device, &wgsl, label)
}

/// Compose `module` of the built-in unlit_wgpu package as a main module, with
/// the named conditional-compilation features set.
pub(crate) fn compose(
    module: &str,
    features: &[(&'static str, bool)],
) -> Result<String, wesl::Error> {
    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("unlit_wgpu".to_owned()),
        vec![module.to_owned()],
    );
    let mut options = wesl::CompileOptions {
        keep_main: true,
        ..Default::default()
    };
    for (name, enabled) in features {
        options.features.set(name, *enabled);
    }

    let mut resolver = wesl::resolver::PackageResolver::new();
    resolver.add_package(&unlit_wgpu::shader::PACKAGE);
    wesl::Compiler::new_with_resolver(options, resolver)
        .compile_module(&main_path)
        .map(|result| result.syntax.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stage names survive composition, which is what lets a pipeline
    /// address them.
    #[test]
    fn each_module_keeps_its_entry_point_names() {
        let vertex = compose("fullscreen_vertex", &[]).expect("the fullscreen vertex composes");
        assert!(vertex.contains("fn fullscreen_vertex_shader"));

        let fragment = compose("blit", &[]).expect("the blit fragment composes");
        assert!(fragment.contains("fn fs_main"));

        let line = compose("line", &[]).expect("the line module composes");
        assert!(line.contains("fn vs_main"));
        assert!(line.contains("fn fs_main"));
    }
}
