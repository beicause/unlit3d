//! A frame source that copies a texture over the target.
//!
//! [`BlitSource`] draws one screen-covering triangle, sampling the
//! [`BlitTexture`] component it finds in the world. It is the source a
//! two-world setup presents with: a world renders into an offscreen texture,
//! and a second world blits that texture into its own target.
//!
//! The texture is an ordinary component, so the world that blits it decides
//! what to hand over and may replace it between frames; the source builds its
//! pipeline and bind group from whatever the world carries. The pipeline is
//! specialized on the frame's target like any other source's, so the same
//! source presents into a swap chain or into another offscreen target.

use unlit_ecs::World;
use unlit_wgpu::resources::TextureView;
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use unlit_wgpu::specialize::{
    FragmentStateDesc, PipelineDescriptor, RenderPipelineDesc, SurfaceKey, SurfaceTarget,
    VertexStateDesc,
};

use crate::source::{FrameOrder, FrameSource, RenderContext, frame_target};

/// The WESL module whose fullscreen vertex the blit draws.
const VERTEX_MODULE: &str = "fullscreen_vertex";
/// The entry point of the vertex module.
const VERTEX_ENTRY: &str = "fullscreen_vertex_shader";
/// The WESL module whose fragment samples the blitted texture.
const FRAGMENT_MODULE: &str = "blit";
/// The entry point of the fragment module.
const FRAGMENT_ENTRY: &str = "fs_main";

/// The texture a [`BlitSource`] copies to the frame's target.
///
/// Spawn it as a component on any entity of a world holding a [`BlitSource`];
/// the source finds the first one each frame and draws it over the whole
/// target. Replacing the component - or the view it holds - changes what the
/// next frame blits.
#[derive(Clone, Debug, PartialEq)]
pub struct BlitTexture {
    view: TextureView,
    sampler: wgpu::Sampler,
}

impl BlitTexture {
    /// A texture sampled with a linear, clamp-to-edge sampler.
    ///
    /// This is the sampler a blit of a same-sized target wants: one texel of
    /// the source lands on one fragment of the target, and the clamp keeps a
    /// fractional edge sample inside the texture.
    pub fn new(device: &wgpu::Device, view: TextureView) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("unlit3d::blit::sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self::with_sampler(view, sampler)
    }

    /// A texture read with the caller's own sampler.
    pub fn with_sampler(view: TextureView, sampler: wgpu::Sampler) -> Self {
        Self { view, sampler }
    }

    /// The view the blit samples.
    pub fn texture_view(&self) -> &TextureView {
        &self.view
    }

    /// The sampler the blit reads the view with.
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// The format of the texture behind the view.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.view.format()
    }
}

/// A frame source that blits one world texture over the target.
///
/// The source owns its [`Scene`] and the pipeline it draws with; the texture
/// comes from the world on every frame, so a world may swap it without
/// touching the source. The pipeline is built on the first frame that knows
/// its target, and rebuilt only when that target changes, the way
/// [`UiSource`](crate::ui::UiSource) specializes its own.
///
/// The blit covers the whole target, so it records at
/// [`FrameOrder::MESH`] - before any overlay a world draws on top of it.
#[derive(Default)]
pub struct BlitSource {
    /// The draws assembled for the current frame.
    scene: Scene,
    /// The GPU state, built on the first frame that knows its target.
    gpu: Option<Gpu>,
    /// The target the current pipeline is specialized for.
    surface: Option<SurfaceKey>,
}

/// The built pipeline and the state its bind group was built from.
struct Gpu {
    /// The pipeline the blit records.
    pipeline: wgpu::RenderPipeline,
    /// The layout its bind group is built against.
    layout: wgpu::BindGroupLayout,
    /// The texture the current bind group was built from.
    texture: BlitTexture,
    /// The bind group binding the texture and its sampler.
    bind_group: wgpu::BindGroup,
}

impl BlitSource {
    /// Create a blit source.
    ///
    /// No GPU state is built here: the pipeline is specialized on the frame's
    /// target, which is not known until the first frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the pipeline and bind group for `surface`, replacing any earlier
    /// ones.
    fn build_gpu(&mut self, device: &wgpu::Device, texture: &BlitTexture, surface: SurfaceKey) {
        let layout = bind_group_layout(device);
        let pipeline = pipeline(device, &layout, surface);
        let bind_group = bind_group(device, &layout, texture);
        self.gpu = Some(Gpu {
            pipeline,
            layout,
            texture: texture.clone(),
            bind_group,
        });
        self.surface = Some(surface);
    }
}

impl FrameSource for BlitSource {
    fn build_scene(
        &mut self,
        world: &World,
        ctx: RenderContext,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        // Cleared on every path: a frame that records this source must never
        // replay the previous frame's blit.
        self.scene.clear();

        let Some(target) = frame_target(world) else {
            log::warn!(
                "BlitSource::build_scene: no FrameTarget in the world, so nothing is blitted"
            );
            return;
        };
        let Some((_, texture)) = world.query::<&BlitTexture>().next() else {
            log::warn!(
                "BlitSource::build_scene: no BlitTexture component, so there is no texture to blit"
            );
            return;
        };

        let device = world
            .get::<wgpu::Device>(ctx.device)
            .expect("the context's device resource exists");

        if self.gpu.is_none() || self.surface != Some(target.surface) {
            // The first frame, or a new target: the bind-group layout
            // changes with the pipeline, so the bind group is rebuilt with
            // it.
            self.build_gpu(&device, &texture, target.surface);
        } else if let Some(gpu) = &mut self.gpu
            && gpu.texture != *texture
        {
            // The same target, a different texture: only the bind group has
            // to follow, since the layout did not change.
            gpu.bind_group = bind_group(&device, &gpu.layout, &texture);
            gpu.texture = texture.clone();
        }

        let gpu = self.gpu.as_ref().expect("the pipeline was just built");
        self.scene.push(
            DrawEntry::new(&gpu.pipeline, DrawRange::vertices(0..3))
                .with_bind_group(0, &gpu.bind_group),
        );
    }

    fn scene(&self) -> &Scene {
        &self.scene
    }

    fn order(&self) -> FrameOrder {
        FrameOrder::MESH
    }
}

/// The bind-group layout a blit pipeline declares: one sampled texture and
/// its sampler.
fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit3d::blit::bind_group_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// The bind group binding `texture` and its sampler to `layout`.
fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    texture: &BlitTexture,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("unlit3d::blit::bind_group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(texture.texture_view().view()),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(texture.sampler()),
            },
        ],
    })
}

/// The pipeline a blit draws with, specialized on `surface`.
///
/// The vertex and the fragment come from two shader modules: the fullscreen
/// vertex keeps its own name only while it is the main module of its compile,
/// so the blit pairs that module with the fragment's across two modules, which
/// wgpu allows.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    surface: SurfaceKey,
) -> wgpu::RenderPipeline {
    let vertex = shader_module(device, VERTEX_MODULE, "unlit3d::blit::vertex");
    let fragment = shader_module(device, FRAGMENT_MODULE, "unlit3d::blit::fragment");
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d::blit::pipeline_layout"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });

    let mut descriptor = RenderPipelineDesc {
        label: Some("unlit3d::blit".to_owned()),
        layout: Some(pipeline_layout),
        vertex: VertexStateDesc {
            module: vertex,
            entry_point: Some(VERTEX_ENTRY.to_owned()),
            compilation_options: Default::default(),
            buffers: Vec::new(),
        },
        // The triangle covers the target and the fragment writes every
        // covered sample; the state the pass owns is left to the pass.
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(FragmentStateDesc {
            module: fragment,
            entry_point: Some(FRAGMENT_ENTRY.to_owned()),
            compilation_options: Default::default(),
            targets: vec![Some(wgpu::ColorTargetState {
                // Rewritten by `set_surface`; the key's own format keeps the
                // descriptor honest before it is.
                format: surface.color_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    descriptor.set_surface(surface);
    descriptor.create(device)
}

/// Compile one module of the built-in WESL package into a shader module.
///
/// `keep_main` is what leaves the module's own declarations - the entry points
/// among them - in the composed WGSL under their own names.
fn shader_module(device: &wgpu::Device, module: &str, label: &str) -> wgpu::ShaderModule {
    let wgsl = compose(module)
        .unwrap_or_else(|error| panic!("the built-in unlit_wgpu shader compiles: {error}"));
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    })
}

/// Compose `module` of the built-in unlit_wgpu package as a main module.
fn compose(module: &str) -> Result<String, wesl::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_module_keeps_its_entry_point_name() {
        let vertex = compose(VERTEX_MODULE).expect("the fullscreen vertex composes");
        assert!(vertex.contains("fn fullscreen_vertex_shader"));

        let fragment = compose(FRAGMENT_MODULE).expect("the blit fragment composes");
        assert!(fragment.contains("fn fs_main"));
    }
}

