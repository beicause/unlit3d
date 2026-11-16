//! The fullscreen composite a frame source shows one texture with.
//!
//! A composite draws one triangle over the whole target, sampling a texture
//! view it is handed. What varies between its users is only what the pipeline
//! says about the target and how the fragment combines with it — a blit copies
//! a texture an earlier pass drew, while a document composites its page over
//! the meshes — so those are the variant's fields and everything else is built
//! once per frame source.
//!
//! The sampled texture's own format decides one of those fields. A sampler
//! reading an sRGB-encoded texture decodes it, so the fragment already holds
//! linear light; reading a plain one hands it the stored values, which for a
//! picture are sRGB-encoded. An sRGB target encodes what it is given, so a plain
//! source has to decode first or the picture washes out, and the fragment does so
//! with the same `encoded_output` step the lit pipeline ends with. An sRGB source
//! into a plain target needs no step: the target stores the linear light the
//! sampler produced.

use unlit_ecs::Scope;
use unlit_wgpu::pipeline::{ShaderCache, ShaderRecipe};
use unlit_wgpu::resources::{Rebuild, ResHandle, Resource, ResourceGraph, TextureView};
use unlit_wgpu::scene::{DrawEntry, DrawOrder, DrawRange, Scene};
use unlit_wgpu::specialize::{
    FragmentStateDesc, PipelineDescriptor, PipelineVariant, RenderPipelineDesc, SurfaceKey,
    SurfaceTarget, Variants, VertexStateDesc,
};

use crate::pass::PassInfo;
use crate::source::RenderContext;

/// The WESL module whose fullscreen vertex the composite draws.
const VERTEX_MODULE: &str = "fullscreen_vertex";
/// The entry point of the vertex module.
const VERTEX_ENTRY: &str = "fullscreen_vertex_shader";
/// The WESL module whose fragment samples the composited texture.
const FRAGMENT_MODULE: &str = "blit_fragment";
/// The entry point of the fragment module.
const FRAGMENT_ENTRY: &str = "fs_main";
/// The vertex the fullscreen triangle is drawn from: its three corners are
/// written by the shader, so nothing is bound to feed them.
const FULLSCREEN_VERTICES: u32 = 3;
/// The binding the composited texture takes.
const TEXTURE_BINDING: u32 = 0;
/// The binding the sampler takes.
const SAMPLER_BINDING: u32 = 1;

/// The state a screen composite draws through, kept across the frames of one
/// source.
///
/// The sampler, the layout and the current bind group do not depend on the
/// target, so one of each serves every pass a frame records the source into.
/// The pipelines are specialized per target, because the passes of one frame
/// need not share a sample count or a depth attachment. Pointing the source at
/// another view rebuilds the group and keeps the rest.
pub(crate) struct ScreenBlit {
    /// Where the composite's draw sits among the pass's own.
    ///
    /// The tier is the source's, not the frame's: a source that presents an
    /// earlier pass's picture overwrites the target and goes before every other
    /// draw, while one that layers a document over the meshes goes after them.
    order: DrawOrder,
    /// The state built on the first frame: nothing is built until one knows
    /// which device and which target it draws with.
    core: Option<Core>,
}

/// The state a composite reads its view through.
struct Core {
    /// The layout the bind group is built against.
    layout: wgpu::BindGroupLayout,
    /// The sampler the view is read with, in the frame's graph.
    sampler: ResHandle<wgpu::Sampler>,
    /// The view the current bind group reads.
    view: ResHandle<TextureView>,
    /// The graph node binding the view and its sampler.
    bind_group: ResHandle<wgpu::BindGroup>,
    /// The pipelines, specialized on the target and the fragment policy.
    pipelines: Variants<wgpu::RenderPipeline, ScreenVariant>,
}

/// The pipeline variant a composite resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ScreenVariant {
    /// The target the composite is drawn into.
    surface: SurfaceKey,
    /// How the fragment combines with whatever the pass already holds.
    blend: Option<wgpu::BlendState>,
    /// Whether the sampled value still needs decoding for the target.
    srgb_to_linear_output: bool,
}

impl PipelineVariant<wgpu::RenderPipeline> for ScreenVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        *self
    }
}

impl PipelineDescriptor<wgpu::RenderPipeline> for ScreenVariant {
    fn create(&self, device: &wgpu::Device, cache: &mut ShaderCache) -> wgpu::RenderPipeline {
        pipeline(device, *self, cache)
    }
}

impl ScreenBlit {
    /// A composite that draws at `order`, with nothing built yet.
    pub(crate) fn new(order: DrawOrder) -> Self {
        Self { order, core: None }
    }

    /// Record `view`, combined with the pass by `blend`.
    ///
    /// `view` must be registered in the frame's resource graph — the one
    /// `ctx` addresses. `blend` is `None` for a copy that overwrites what the
    /// pass holds, and a source-over state for one that layers over it.
    pub(crate) fn record(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        view: ResHandle<TextureView>,
        blend: Option<wgpu::BlendState>,
    ) {
        // The graph settles on every path out of this function, so a sampler
        // or bind group this build registers is realised when the scope drops,
        // whatever this frame does.
        let maintain = ctx.maintain_scope(*scope);

        let device = ctx
            .device(scope)
            .expect("the context's device resource exists");
        // The source's format decides the fragment's one dynamic step, so it is
        // read before the pipeline is chosen.
        let srgb_source = {
            let mut graph_holder = ctx.graph(scope);
            graph_holder
                .get()
                .get(&view)
                .expect("the composited view is registered in the graph")
                .format()
                .is_srgb()
        };

        if self.core.is_none() {
            let sampler = ctx.graph(scope).get().insert(sampler(&device), None);
            let layout = bind_group_layout(&device);
            let bind_group =
                allocate_bind_group(scope, ctx, &device, &layout, sampler.clone(), view.clone());
            self.core = Some(Core {
                layout,
                sampler,
                view: view.clone(),
                bind_group,
                pipelines: Variants::new(&device),
            });
        }
        let core = self.core.as_mut().expect("the core is built above");

        let ordinal = core.pipelines.specialize(ScreenVariant {
            surface: info.surface,
            blend,
            srgb_to_linear_output: !srgb_source && info.surface.color_format.is_srgb(),
        });

        // The same target, a different view: the layout and the sampler stay,
        // and the view's new id gets its own group.
        if core.view != view {
            let layout = core.layout.clone();
            let sampler = core.sampler.clone();
            core.bind_group =
                allocate_bind_group(scope, ctx, &device, &layout, sampler, view.clone());
            core.view = view;
        }

        // Drop the scope so the graph maintains before the group is read back
        // out: a replace this build made must be visible to the draw.
        drop(maintain);
        let mut graph_holder = ctx.graph(scope);
        let graph = graph_holder.get();
        let pipeline = &core.pipelines.get(ordinal).pipeline;
        let bind_group = graph
            .get(&core.bind_group)
            .expect("the bind group is registered in the graph");
        scene.draws.push(
            DrawEntry::new(pipeline, DrawRange::vertices(0..FULLSCREEN_VERTICES))
                .with_bind_group(TEXTURE_BINDING, bind_group)
                .with_order(self.order),
        );
    }
}

/// The sampler a composite reads its view with: linear filtering, clamped at
/// the edges.
///
/// This is what a same-sized composite wants: one texel of the source lands on
/// one fragment of the target, and the clamp keeps a fractional edge sample
/// inside the texture.
fn sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("unlit3d::screen::sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

/// Register a bind group reading `view` with `sampler` into the frame's
/// graph, rebuilding it whenever either is replaced.
///
/// The group is a node the graph owns, so the view and the sampler have to
/// stay registered for as long as the group reads them; `add_dependency`
/// records that on the group's behalf.
fn allocate_bind_group(
    scope: &Scope<'_>,
    ctx: RenderContext,
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: ResHandle<wgpu::Sampler>,
    view: ResHandle<TextureView>,
) -> ResHandle<wgpu::BindGroup> {
    let recipe = bind_group_recipe(
        device.clone(),
        layout.clone(),
        view.clone(),
        sampler.clone(),
    );
    let mut graph_holder = ctx.graph(scope);
    let graph = graph_holder.get();
    let bind_group = recipe(graph);
    let rebuild = Rebuild::new(move |graph| Resource::BindGroup(recipe(graph)));
    let bind_group = graph.insert(bind_group, Some(rebuild));
    graph.add_dependency(&bind_group, &view);
    graph.add_dependency(&bind_group, &sampler);
    bind_group
}

/// The bind-group layout a composite pipeline declares: one sampled texture
/// and its sampler.
fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit3d::screen::bind_group_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: TEXTURE_BINDING,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: SAMPLER_BINDING,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// The recipe that binds `view` and `sampler` to `layout`.
///
/// It reads both back out of the graph on every run, so a replacement of
/// either is what a rebuild picks up.
fn bind_group_recipe(
    device: wgpu::Device,
    layout: wgpu::BindGroupLayout,
    view: ResHandle<TextureView>,
    sampler: ResHandle<wgpu::Sampler>,
) -> impl Fn(&ResourceGraph) -> wgpu::BindGroup {
    move |graph| {
        let view = graph
            .get(&view)
            .expect("the composited view is registered in the graph");
        let sampler = graph
            .get(&sampler)
            .expect("the composite's sampler is registered in the graph");
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::screen::bind_group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(view.view()),
                },
                wgpu::BindGroupEntry {
                    binding: SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }
}

/// The pipeline a composite draws with, specialized on `variant`.
///
/// The vertex and the fragment come from two shader modules: the fullscreen
/// vertex keeps its own name only while it is the main module of its compile,
/// so the composite pairs that module with the fragment's across two modules,
/// which wgpu allows. The layout is built here rather than shared, because a
/// variant's descriptor owns its whole pipeline and wgpu deduplicates
/// identical layouts on its side.
fn pipeline(
    device: &wgpu::Device,
    variant: ScreenVariant,
    cache: &mut ShaderCache,
) -> wgpu::RenderPipeline {
    // The vertex takes no flag or caller's source, so it composes once per
    // device however many targets the composite is specialized on.
    let vertex_label = "unlit3d::screen::vertex";
    let vertex = get_or_create(cache, device, VERTEX_MODULE, &[], vertex_label);
    let fragment_label = "unlit3d::screen::fragment";
    let features = [("SRGB_TO_LINEAR_OUTPUT", variant.srgb_to_linear_output)];
    let fragment = get_or_create(cache, device, FRAGMENT_MODULE, &features, fragment_label);
    let layout = bind_group_layout(device);
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d::screen::pipeline_layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let mut descriptor = RenderPipelineDesc {
        label: Some("unlit3d::screen".to_owned()),
        layout: Some(pipeline_layout),
        vertex: VertexStateDesc {
            module: vertex,
            entry_point: Some(VERTEX_ENTRY.to_owned()),
            compilation_options: Default::default(),
            buffers: Vec::new(),
        },
        // The triangle covers the target and the fragment writes every covered
        // sample; the state the pass owns is left to the pass.
        primitive: Default::default(),
        // A composite covers the target and has no depth of its own, but the
        // pass it is recorded into may carry a depth attachment, and wgpu
        // rejects a pipeline whose depth state disagrees with the pass's. So
        // the state is declared and defers to the target: the format is
        // rewritten by `set_surface`, which also drops the state entirely for a
        // target that has no depth.
        depth_stencil: variant
            .surface
            .depth_stencil_format
            .map(|format| wgpu::DepthStencilState {
                format,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
        multisample: Default::default(),
        fragment: Some(FragmentStateDesc {
            module: fragment,
            entry_point: Some(FRAGMENT_ENTRY.to_owned()),
            compilation_options: Default::default(),
            targets: vec![Some(wgpu::ColorTargetState {
                // Rewritten by `set_surface`; the key's own format keeps the
                // descriptor honest before it is.
                format: variant.surface.color_format,
                blend: variant.blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    descriptor.set_surface(variant.surface);
    descriptor.create(device, cache)
}

/// Compose `module` with `features` and build its shader, reusing the cache's
/// copy when the device already has one.
fn get_or_create(
    cache: &mut ShaderCache,
    device: &wgpu::Device,
    module: &'static str,
    features: &[(&'static str, bool)],
    label: &str,
) -> wgpu::ShaderModule {
    cache.get_or_create(
        device,
        &ShaderRecipe::new(module, features, None),
        label,
        || {
            unlit_wgpu::compose::compose(module, features, None)
                .unwrap_or_else(|error| panic!("{label}: {error}"))
        },
    )
}
