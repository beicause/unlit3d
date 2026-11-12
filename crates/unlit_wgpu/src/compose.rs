//! Composing a WESL module of the built-in shader package into WGSL.
//!
//! Every built-in entry shader — `lit`, `line`, `blit`, `fullscreen_vertex` —
//! is a module of the package generated at build time from `shaders/*.wesl`.
//! This module is the one place that turns one of them into a WGSL string: it
//! sets the conditional-compilation flags, keeps the module's own entry points,
//! and, when a caller has one, mounts the caller's source in place of the
//! package's copy. Both the built-in pipelines and a caller's own pipeline
//! reach the same facilities here; nothing in the composition is private to one
//! built-in implementation.
//!
//! A caller's source is a whole module, never a snippet: `lit.wesl` and
//! `line.wesl` are pairs of entry points over the package's stage modules, so a
//! caller writes a module that imports those and calls the parts it keeps. It is
//! mounted at exactly the path of the copy it replaces, which is why its own
//! imports still resolve to the package.

/// Compose `module` of the built-in WESL package into one WGSL module.
///
/// `module` names a top-level module of the package (`lit`, `line`, `blit`, …).
/// `features` are the conditional-compilation flags set before composing; the
/// module's own declarations — its entry points among them — stay under their
/// own names, so a pipeline can address them.
///
/// When `replacement` is `Some`, that whole module replaces the package's own copy
/// of `module`: it is mounted at exactly that one path, so its own imports
/// (`package::…` and `unlit_wgpu::…` alike) still resolve to the package, and a
/// caller composes the stages out of the same modules the built-in shader uses
/// without gaining a private copy of the compiler.
pub fn compose(
    module: &str,
    features: &[(&'static str, bool)],
    replacement: Option<&str>,
) -> Result<String, ComposeError> {
    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("unlit_wgpu".to_owned()),
        vec![module.to_owned()],
    );
    let mut compile_options = wesl::CompileOptions::default();
    for (name, enabled) in features {
        compile_options.features.set(name, *enabled);
    }
    compile_options.keep_main = true;

    let mut packages = wesl::resolver::PackageResolver::new();
    packages.add_package(&crate::shader::PACKAGE);

    let composed = match replacement {
        None => {
            wesl::Compiler::new_with_resolver(compile_options, packages).compile_module(&main_path)
        }
        Some(source) => {
            let mut modules: wesl::resolver::VirtualResolver<'static> =
                wesl::resolver::VirtualResolver::new();
            modules.add_module(
                wesl::syntax::ModulePath::new(wesl::syntax::PathOrigin::Absolute, Vec::new()),
                source.to_owned().into(),
            );
            let mut resolver = wesl::resolver::Router::new();
            resolver.mount_resolver(main_path.clone(), modules);
            resolver.mount_fallback_resolver(packages);
            wesl::Compiler::new_with_resolver(compile_options, resolver).compile_module(&main_path)
        }
    };
    composed
        .map(|result| result.syntax.to_string())
        .map_err(ComposeError::Compile)
}

/// Failure reasons reported by [`compose`].
#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    /// The WESL compiler rejected the module.
    #[error("WESL compilation failed: {0}")]
    Compile(#[from] wesl::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stage names survive composition, which is what lets a pipeline
    /// address them.
    #[test]
    fn each_module_keeps_its_entry_point_names() {
        let vertex =
            compose("fullscreen_vertex", &[], None).expect("the fullscreen vertex composes");
        assert!(vertex.contains("fn fullscreen_vertex_shader"));

        let fragment = compose("blit", &[], None).expect("the blit fragment composes");
        assert!(fragment.contains("fn fs_main"));

        let line = compose("line", &[], None).expect("the line module composes");
        assert!(line.contains("fn vs_main"));
        assert!(line.contains("fn fs_main"));
    }
}
