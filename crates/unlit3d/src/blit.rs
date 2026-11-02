//! A frame source that copies a texture over the pass's target.
//!
//! [`BlitSource`] draws one screen-covering triangle, sampling a texture view
//! it is handed. It is how a frame presents what an earlier pass drew: a first
//! pass renders into an offscreen texture, and a blit source in a later pass
//! copies that texture into the target.
//!
//! The view is the source's own input, so a world decides what to hand it and
//! may swap it between frames through its [`view`](BlitSource::view) field. It lives in the
//! frame's [`ResourceGraph`], and the bind group is a node there that a
//! replace of the view rebuilds; the pipeline is specialized on the pass's
//! target through the generic variant cache, like any other source's, so the
//! same source presents into a swap chain or into another offscreen target.

use unlit_ecs::Scope;
use unlit_wgpu::resources::{Rebuild, ResHandle, Resource, ResourceGraph, TextureView};
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use unlit_wgpu::specialize::{
    FragmentStateDesc, PipelineDescriptor, PipelineVariant, RenderPipelineDesc, SurfaceKey,
    SurfaceTarget, Variants, VertexStateDesc,
};

use crate::pass::PassInfo;
use crate::source::{FrameSource, FrameSourceOrder, RenderContext};

/// The WESL module whose fullscreen vertex the blit draws.
const VERTEX_MODULE: &str = "fullscreen_vertex";
/// The entry point of the vertex module.
const VERTEX_ENTRY: &str = "fullscreen_vertex_shader";
/// The WESL module whose fragment samples the blitted texture.
const FRAGMENT_MODULE: &str = "blit";
/// The entry point of the fragment module.
const FRAGMENT_ENTRY: &str = "fs_main";

/// A frame source that blits one texture view over its pass's target.
///
/// The view the source samples is the caller's, registered in the frame's
/// [`ResourceGraph`], so a world may hand over a different one through the
/// [`view`](BlitSource::view) field. The pipeline is built on the first frame that
/// knows its target, the way [`UiSource`](crate::ui::UiSource) specializes its
/// own.
///
/// The blit covers the whole target, so it records at
/// [`FrameSourceOrder::MESH`].
pub struct BlitSource {
    /// The view the source copies, in the frame's resource graph.
    ///
    /// Assigning a different handle makes the next frame rebuild the bind
    /// group; the pipeline and the sampler stay.
    pub view: ResHandle<TextureView>,
    /// The pipelines, specialized on the target each pass draws into.
    ///
    /// A source may be recorded by several passes, and the passes of one frame
    /// need not share a specialization key — a multisampled offscreen pass and
    /// a single-sampled swap chain differ in sample count. Specializing keeps
    /// each pass's pipeline rather than rebuilding as the passes alternate.
    pipelines: Option<Variants<wgpu::RenderPipeline, BlitVariant>>,
    /// The sampler, the layout and the bind group, built on the first frame.
    ///
    /// None of these depends on the target: the layout declares a texture and
    /// a sampler, the group binds the view and that sampler, and a blit writes
    /// every sample it covers. One of each serves every pass; only the
    /// pipeline varies with the target.
    core: Option<Core>,
}

/// The target-independent GPU state a blit reads its view through.
struct Core {
    /// The layout the bind group is built against.
    layout: wgpu::BindGroupLayout,
    /// The sampler the view is read with, in the frame's graph.
    sampler: ResHandle<wgpu::Sampler>,
    /// The view the current bind group reads.
    view: ResHandle<TextureView>,
    /// The graph node binding the view and its sampler.
    bind_group: ResHandle<wgpu::BindGroup>,
}

/// The pipeline variant a blit resolves to.
///
/// The target is the whole variant: a blit blends nothing, tests no depth and
/// writes every sample it covers, so the format, the sample count and the
/// presence of depth are the only things a pass can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct BlitVariant {
    /// The target the blit is drawn into.
    surface: SurfaceKey,
}

impl PipelineVariant<wgpu::RenderPipeline> for BlitVariant {
    type Descriptor = Self;

    fn descriptor(&self, _device: &wgpu::Device) -> Self {
        *self
    }
}

impl PipelineDescriptor<wgpu::RenderPipeline> for BlitVariant {
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        pipeline(device, self.surface)
    }
}

impl BlitSource {
    /// Blit `view` over the pass's target.
    ///
    /// `view` must already be registered in the frame's resource graph — the
    /// one the frame's [`RenderContext`] addresses — like the view it is
    /// blitted into.
    pub fn new(view: ResHandle<TextureView>) -> Self {
        Self {
            view,
            pipelines: None,
            core: None,
        }
    }

    /// Build the sampler, the layout and the bind group, on the first frame.
    fn build_core(&self, scope: &Scope<'_>, ctx: RenderContext) -> Core {
        let mut device_row = scope
            .query_one::<&wgpu::Device>(ctx.device)
            .expect("the context's device resource exists");
        let device = wgpu::Device::clone(device_row.get());
        let sampler = ctx.graph(scope).get().insert(sampler(&device), None);
        let layout = bind_group_layout(&device);
        let bind_group = allocate_bind_group(
            scope,
            ctx,
            &device,
            &layout,
            sampler.clone(),
            self.view.clone(),
        );
        Core {
            layout,
            sampler,
            view: self.view.clone(),
            bind_group,
        }
    }
}

/// The sampler a blit reads its view with: linear filtering, clamped at the
/// edges.
///
/// This is what a blit of a same-sized target wants: one texel of the source
/// lands on one fragment of the target, and the clamp keeps a fractional edge
/// sample inside the texture.
fn sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("unlit3d::blit::sampler"),
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

impl FrameSource for BlitSource {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        // The graph settles on every path out of this function, so a sampler
        // or bind group this build registers is realised when the scope drops,
        // whatever this frame does.
        let maintain = ctx.maintain_scope(*scope);

        // The sampler, the layout and the group serve every pass; they are
        // built once and rebuilt only when the view changes.
        if self.core.is_none() {
            self.core = Some(self.build_core(scope, ctx));
        }
        let device = scope
            .query_one::<&wgpu::Device>(ctx.device)
            .expect("the context's device resource exists")
            .get()
            .clone();

        // The pass's target picks the pipeline, and only the pipeline: the
        // passes of one frame need not share a key, and specializing keeps
        // each pass's pipeline rather than rebuilding as they alternate.
        let pipelines = self.pipelines.get_or_insert_with(|| Variants::new(&device));
        let ordinal = pipelines.specialize(BlitVariant {
            surface: info.surface,
        });

        // The same target, a different view: the layout and the sampler stay,
        // and the view's new id gets its own group.
        let core = self.core.as_mut().expect("the core is built above");
        if core.view != self.view {
            core.bind_group = allocate_bind_group(
                scope,
                ctx,
                &device,
                &core.layout,
                core.sampler.clone(),
                self.view.clone(),
            );
            core.view = self.view.clone();
        }

        // Drop the scope so the graph maintains before the group is read back
        // out: a replace this build made must be visible to the draw.
        drop(maintain);
        let mut graph_holder = ctx.graph(scope);
        let graph = graph_holder.get();
        let pipeline = &pipelines.get(ordinal).pipeline;
        let core = self.core.as_ref().expect("the core is built above");
        let bind_group = graph
            .get(&core.bind_group)
            .expect("the bind group is registered in the graph");
        scene.draws.push(
            DrawEntry::new(pipeline, DrawRange::vertices(0..3)).with_bind_group(0, bind_group),
        );
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::MESH
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
            .expect("the blit view is registered in the graph");
        let sampler = graph
            .get(&sampler)
            .expect("the blit sampler is registered in the graph");
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unlit3d::blit::bind_group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }
}

/// The pipeline a blit draws with, specialized on `surface`.
///
/// The vertex and the fragment come from two shader modules: the fullscreen
/// vertex keeps its own name only while it is the main module of its compile,
/// so the blit pairs that module with the fragment's across two modules, which
/// wgpu allows. The layout is built here rather than shared, because a
/// variant's descriptor owns its whole pipeline and wgpu deduplicates
/// identical layouts on its side.
fn pipeline(device: &wgpu::Device, surface: SurfaceKey) -> wgpu::RenderPipeline {
    let vertex = crate::builtin::shader_module(device, VERTEX_MODULE, "unlit3d::blit::vertex", &[]);
    let fragment =
        crate::builtin::shader_module(device, FRAGMENT_MODULE, "unlit3d::blit::fragment", &[]);
    let layout = bind_group_layout(device);
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d::blit::pipeline_layout"),
        bind_group_layouts: &[Some(&layout)],
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
