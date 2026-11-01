//! The plumbing the crate's built-in frame sources share.
//!
//! Every built-in source compiles a module of the `unlit_wgpu` WESL package for
//! a target it only learns about at draw time, and rewrites the camera and
//! frame-globals uniforms through the frame's own encoder. Both are the same
//! calls whichever source makes them, so they live here instead of once per
//! source.

/// Compile one module of the built-in WESL package into a shader module.
///
/// `keep_main` is what leaves the module's own declarations - the entry points
/// among them - in the composed WGSL under their own names.
pub(crate) fn shader_module(
    device: &wgpu::Device,
    module: &str,
    label: &str,
) -> wgpu::ShaderModule {
    let wgsl = compose(module)
        .unwrap_or_else(|error| panic!("the built-in unlit_wgpu shader compiles: {error}"));
    unlit_wgpu::util::shader_module(device, &wgsl, label)
}

/// Compose `module` of the built-in unlit_wgpu package as a main module.
pub(crate) fn compose(module: &str) -> Result<String, wesl::Error> {
    let main_path = wesl::syntax::ModulePath::new(
        wesl::syntax::PathOrigin::Package("unlit_wgpu".to_owned()),
        vec![module.to_owned()],
    );
    let options = wesl::CompileOptions {
        keep_main: true,
        ..Default::default()
    };

    let mut resolver = wesl::resolver::PackageResolver::new();
    resolver.add_package(&unlit_wgpu::shader::PACKAGE);
    wesl::Compiler::new_with_resolver(options, resolver)
        .compile_module(&main_path)
        .map(|result| result.syntax.to_string())
}

/// A uniform buffer of `size` bytes, rewritten through a source's staging
/// buffer every frame.
pub(crate) fn uniform_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stage names survive composition, which is what lets a pipeline
    /// address them.
    #[test]
    fn each_module_keeps_its_entry_point_names() {
        let vertex = compose("fullscreen_vertex").expect("the fullscreen vertex composes");
        assert!(vertex.contains("fn fullscreen_vertex_shader"));

        let fragment = compose("blit").expect("the blit fragment composes");
        assert!(fragment.contains("fn fs_main"));

        let line = compose("line").expect("the line module composes");
        assert!(line.contains("fn vs_main"));
        assert!(line.contains("fn fs_main"));
    }
}
