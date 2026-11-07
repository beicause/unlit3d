//! Composing a WESL module of the built-in shader package into WGSL.
//!
//! Every built-in entry shader — `lit`, `line`, `blit`, `fullscreen_vertex` —
//! is a module of the package generated at build time from `shaders/*.wesl`.
//! This module is the one place that turns one of them into a WGSL string: it
//! sets the conditional-compilation flags, keeps the module's own entry points,
//! and, when a caller extends a shader, mounts the spliced source in place of
//! the package's copy. Both the built-in pipelines and a caller's own pipeline
//! reach the same facilities here; nothing in the composition is private to one
//! built-in implementation.

/// The comment the built-in entry shaders mark their module-scope splice with.
pub const MODULE_HOOK: &str = "// unlit_wgpu::hook module";
/// The comment the built-in entry shaders mark their vertex-stage splice with.
pub const VERTEX_HOOK: &str = "// unlit_wgpu::hook vertex";
/// The comment the built-in entry shaders mark their fragment-stage splice with.
pub const FRAGMENT_HOOK: &str = "// unlit_wgpu::hook fragment";

/// `source` with each hook mark replaced by the snippet belonging to it.
///
/// The splice happens on WESL, before anything is parsed or compiled, so a
/// snippet takes part in the same conditional compilation as the code around it
/// and a diagnostic inside one points at the line it was written on.
///
/// # Panics
///
/// If `source` does not carry all three marks. They are checked rather than
/// trusted because a mark is a bare comment: losing one while editing a shader
/// would otherwise silently drop every snippet aimed at it.
pub fn splice(source: &str, module: &str, vertex: &str, fragment: &str) -> String {
    let spliced = splice_at(source, MODULE_HOOK, module);
    let spliced = splice_at(&spliced, VERTEX_HOOK, vertex);
    let spliced = splice_at(&spliced, FRAGMENT_HOOK, fragment);
    assert!(
        !spliced.contains(MODULE_HOOK)
            && !spliced.contains(VERTEX_HOOK)
            && !spliced.contains(FRAGMENT_HOOK),
        "the built-in entry shader carries all three hook marks"
    );
    spliced
}

/// `source` with the line carrying the first `mark` replaced by `snippet`.
///
/// The whole line goes, indentation included, so the snippet stands where the
/// mark stood and an empty snippet leaves a blank line rather than an indented
/// tail. A missing mark leaves the source untouched; [`splice`] is what rejects
/// one.
fn splice_at(source: &str, mark: &str, snippet: &str) -> String {
    let Some(at) = source.find(mark) else {
        return source.to_owned();
    };
    let line_start = source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let line_end = source[at..]
        .find('\n')
        .map_or(source.len(), |newline| at + newline);
    let mut spliced = String::with_capacity(source.len() + snippet.len());
    spliced.push_str(&source[..line_start]);
    spliced.push_str(snippet);
    spliced.push_str(&source[line_end..]);
    spliced
}

/// Compose `module` of the built-in WESL package into one WGSL module.
///
/// `module` names a top-level module of the package (`lit`, `line`, `blit`, …).
/// `features` are the conditional-compilation flags set before composing; the
/// module's own declarations — its entry points among them — stay under their
/// own names, so a pipeline can address them.
///
/// When `replacement` is `Some`, that source replaces the package's own copy of
/// `module`: it is mounted at exactly that one path, so the imports around it
/// (`unlit_wgpu::…` and the shader's own `package::…` alike) still resolve to
/// the package that owns them. It is how a caller's snippets are spliced into
/// an entry shader without that shader gaining a private copy of the compiler.
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
