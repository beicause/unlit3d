use super::*;

/// A caller's own entry shader, composed in place of the built-in `lit.wesl`.
///
/// The source is one whole WESL module, mounted at the package path the built-in
/// entry shader occupies. It imports the modules the built-in stages are built
/// from — [`shader::lit_bindings`](crate::shader), `lit_vertex`, `lit_lighting`,
/// `lit_fragment` — through `package::` paths, so it reaches the same bindings,
/// the same vertex streams and the same helpers the built-in shader does, under
/// the variant's conditional-compilation flags.
///
/// Two things stay the pipeline's rather than the shader's:
///
/// * The entry points must be named [`VS_MAIN`] and [`FS_MAIN`], because that is
///   what the pipeline names when it creates the module. The built-in
///   `lit.wesl` is a module of two entry points over the stage modules, and a
///   caller's module takes its place, so it declares both itself.
/// * The layouts and the vertex streams stay the variant's: a shader changes
///   what a stage computes, not what it reads. A caller module that declares a
///   binding of its own therefore fails pipeline creation rather than drawing
///   something else; one that reads the variant's bindings through
///   [`shader::lit_bindings`](crate::shader) works.
///
/// The label names the shader: it is the shader module's label, and a
/// composition error is reported under it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LitShader {
    label: Arc<str>,
    source: Arc<str>,
}

impl LitShader {
    /// A shader named `label` whose `source` is the whole WESL module composed
    /// in place of the built-in entry shader.
    pub fn new(label: impl Into<Arc<str>>, source: impl Into<Arc<str>>) -> Self {
        Self {
            label: label.into(),
            source: source.into(),
        }
    }

    /// The name this shader is reported under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The WESL module composed in place of the built-in entry shader.
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl core::fmt::Debug for LitShader {
    /// Prints the label and the source's length rather than the source itself,
    /// so an [`LitOptions`] dump stays readable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LitShader")
            .field("label", &self.label)
            .field("source", &format_args!("{} bytes", self.source.len()))
            .finish()
    }
}
