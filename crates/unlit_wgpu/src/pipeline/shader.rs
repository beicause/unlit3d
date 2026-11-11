use super::*;

/// A caller's own shader code, spliced into the built-in `lit.wesl`.
///
/// The built-in entry shader keeps three marks, each a comment naming the place
/// it sits: one at module scope, one at the end of the vertex stage and one at
/// the end of the fragment stage. A shader is spliced in at those marks while the
/// module is still WESL, before anything is compiled, so each snippet is ordinary
/// WESL: it sees every binding, flag and helper the place it lands in sees, it
/// takes part in the conditional compilation, and its diagnostics point at the
/// line it was written on rather than at a fragment of generated WGSL.
///
/// The marks sit where the shader still has something left to do, which is what
/// keeps a snippet short:
///
/// * The module mark is after every built-in binding, so a snippet can declare
///   the functions, types and constants the stages call, and read the built-in
///   bindings from them.
/// * The vertex mark is after the instance transform, before the clip position
///   is built. The vertex is a `var world` there, and everything downstream —
///   the clip position, the world position the lights are addressed from, the
///   normal and the cluster cell — follows the vertex the snippet leaves.
/// * The fragment mark is after the base color is lit and cut off, before the
///   target's encoding. The color is a `var color` there, so a snippet can
///   rewrite it in place, and what it leaves is what the target receives.
///
/// The layouts and the vertex streams stay the variant's, exactly as they are
/// for the built-in shader: a snippet changes what a stage computes, not what the
/// variant reads. A snippet that declares a binding of its own therefore fails
/// pipeline creation rather than drawing something else; one that declares
/// functions, types or constants over the bindings every variant of that shape
/// already declares works.
///
/// The label names the shader: it is the shader module's label, and a
/// composition error is reported under it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LitShader {
    label: Arc<str>,
    module: Arc<str>,
    vertex: Arc<str>,
    fragment: Arc<str>,
}

impl LitShader {
    /// A shader named `label` whose `module`, `vertex` and `fragment` snippets
    /// are spliced into the built-in entry shader.
    ///
    /// Any snippet may be empty, which leaves its place exactly as the built-in
    /// shader has it.
    pub fn new(
        label: impl Into<Arc<str>>,
        module: impl Into<Arc<str>>,
        vertex: impl Into<Arc<str>>,
        fragment: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            label: label.into(),
            module: module.into(),
            vertex: vertex.into(),
            fragment: fragment.into(),
        }
    }

    /// The name this shader is reported under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The WESL declarations spliced in at module scope.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// The WESL statements spliced into the vertex stage.
    pub fn vertex(&self) -> &str {
        &self.vertex
    }

    /// The WESL statements spliced into the fragment stage.
    pub fn fragment(&self) -> &str {
        &self.fragment
    }
}

impl core::fmt::Debug for LitShader {
    /// Prints the label and each snippet's length rather than the snippets
    /// themselves, so an [`LitOptions`] dump stays readable.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LitShader")
            .field("label", &self.label)
            .field("module", &format_args!("{} bytes", self.module.len()))
            .field("vertex", &format_args!("{} bytes", self.vertex.len()))
            .field("fragment", &format_args!("{} bytes", self.fragment.len()))
            .finish()
    }
}
