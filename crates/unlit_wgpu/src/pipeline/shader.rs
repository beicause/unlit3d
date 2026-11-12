use super::*;

/// A caller's own entry shader, composed in place of a built-in one.
///
/// The source is one whole WESL module, mounted at the package path a built-in
/// entry shader occupies — `lit.wesl` for the lit pipeline, `line.wesl` for
/// the line one. It imports the package modules that shader is built from
/// through `package::` paths, so it reaches the same bindings, the same vertex
/// streams and the same helpers the built-in shader does, under the variant's
/// conditional-compilation flags.
///
/// Two things stay the variant's rather than the shader's:
///
/// * The entry points must keep the names the built-in module declares, because
///   that is what the pipeline names when it creates the module. A built-in
///   entry shader is a module of two entry points over the package's stage
///   modules, and a caller's module takes its place, so it declares both itself.
/// * The layouts and the vertex streams stay the variant's: a shader changes
///   what a stage computes, not what it reads. A caller module that declares a
///   binding of its own therefore fails pipeline creation rather than drawing
///   something else; one that reads the variant's bindings through the
///   package's binding module works.
///
/// The label names the shader: it is the shader module's label, and a
/// composition error is reported under it. It takes part in the variant's
/// identity, so two shaders of equal source but different labels are two
/// shaders.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Shader {
    label: Arc<str>,
    source: Arc<str>,
}

impl Shader {
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

impl core::fmt::Debug for Shader {
    /// Prints the label and the source's length rather than the source itself,
    /// so an options dump stays readable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Shader")
            .field("label", &self.label)
            .field("source", &format_args!("{} bytes", self.source.len()))
            .finish()
    }
}
