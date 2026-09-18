//! The object that allocates the attachments and owns the built-in pipelines.
//!
//! A [`RenderContext`] is the long-lived hub of a frame loop. It holds the
//! [`Renderer`]'s attachments — the color view the caller hands over, plus
//! the multisample and depth textures the context allocates as transient
//! attachments, so mobile GPUs can keep them in tile memory — and a cache of
//! built-in pipelines keyed by everything that shapes one ([`UnlitOptions`],
//! the attachment formats and the sample count).
//!
//! When the configuration changes — a resize, a format switch, MSAA off —
//! the context recreates the transient attachments and invalidates every
//! cached pipeline, so the next call to [`Self::pipeline`] or
//! [`Self::render`] transparently rebuilds what the change affected.
//! Callers resize and draw; rebuilding is not their problem.
//!
//! Pipelines are only built for variants that are actually asked for;
//! nothing is eagerly compiled. A variant drawn by several scenes is built
//! once and shared.
//!
//! Sharing resources more broadly — textures, buffers, bind groups across
//! scenes — is the job of [`crate::resources::ResourceGraph`], which a caller
//! can keep alongside the context: the context tracks what it owns, the
//! graph tracks what the caller registered, and both are rebuilt by the same
//! target-change signal. A [`crate::scene::Scene`] stays the caller's,
//! assembled per frame from the handles both hand out.
//!
//! ```no_run
//! # use wgpu_unlit_render::pipeline::UnlitOptions;
//! # use wgpu_unlit_render::render_context::{RenderContext, RendererOptions};
//! # use wgpu_unlit_render::scene::Scene;
//! # fn frame(device: &wgpu::Device, view: wgpu::TextureView, scene: &Scene<'_>) {
//! let mut context = RenderContext::new(
//!     device,
//!     Some(view),
//!     RendererOptions::new(1280, 720),
//! );
//!
//! // Pipelines are cached by their variant and target; asking twice for the
//! // same one builds it once.
//! let key = context.pipeline_key(&UnlitOptions::standard());
//! let pipeline = context.pipeline(&key).expect("built on first request");
//!
//! // A resize recreates the attachments and invalidates the pipelines; the
//! // next frame rebuilds them.
//! context.set_options(device, None, RendererOptions::new(640, 480));
//! context.pipeline(&key); // rebuilt here, or at the next `render`
//!
//! let mut encoder = device.create_command_encoder(&Default::default());
//! context.render(&mut encoder, wgpu::Color::BLACK, scene);
//! # }
//! ```

use crate::pipeline::{UnlitOptions, UnlitPipeline};
use crate::renderer::Renderer;
use crate::resources::{Resource, ResourceGraph};
use hashbrown::HashMap;

/// Configuration for a [`RenderContext`]: the attachments' formats, the
/// target size and the sample count.
///
/// Every field is also an invalidation signal: changing any of them makes the
/// context recreate its transient attachments and rebuild its pipelines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RendererOptions {
    /// Color attachment format, matching the context's color view when that
    /// is `Some`; `None` exactly when the pass is depth-only.
    pub color: Option<wgpu::TextureFormat>,
    /// Depth attachment format, or `None` for a pass without depth.
    ///
    /// At least one of [`Self::color`] and this must be `Some`: a pass with
    /// neither attachment cannot exist.
    pub depth: Option<wgpu::TextureFormat>,
    /// Width of the attachments, in pixels.
    pub width: u32,
    /// Height of the attachments, in pixels.
    pub height: u32,
    /// MSAA sample count. `1` disables multisampling.
    pub sample_count: u32,
}

impl RendererOptions {
    /// The color, depth, size and sample count for an `Rgba8UnormSrgb` +
    /// `Depth32Float` target of `width` x `height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            color: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
            depth: Some(wgpu::TextureFormat::Depth32Float),
            width,
            height,
            sample_count: 4,
        }
    }
}

/// Everything that shapes a built-in pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    /// The variant the pipeline was built for.
    pub options: UnlitOptions,
    /// The color format the pipeline was built for.
    pub color_format: Option<wgpu::TextureFormat>,
    /// The depth format the pipeline was built for.
    pub depth_format: Option<wgpu::TextureFormat>,
    /// The sample count the pipeline was built for.
    pub sample_count: u32,
}

/// Owns the renderer's attachments and the built-in pipeline cache,
/// rebuilding both when the configuration changes.
pub struct RenderContext {
    renderer: Renderer,
    /// Kept alive so the views in `renderer` stay valid: wgpu keeps the
    /// texture alive as long as any view borrows it, but holding the
    /// textures here makes the ownership explicit.
    _textures: Vec<wgpu::Texture>,
    /// The ledger of the raw wgpu resources behind the renderer: the color,
    /// depth and MSAA attachment textures and views. A caller can register
    /// further resources alongside these and track dependencies on them.
    graph: ResourceGraph,
    /// The built-in pipelines, keyed by what shapes them. These are composite
    /// objects (a render pipeline plus its bind group layouts), not raw wgpu
    /// handles, so they live beside the graph rather than in it.
    pipelines: HashMap<PipelineKey, UnlitPipeline>,
    /// Set when the configuration changed and the pipelines have not been
    /// rebuilt yet.
    stale: bool,
}

impl RenderContext {
    /// Create a context drawing into `color_view` (or into the depth
    /// attachment alone when it is `None`), allocating the transient
    /// attachments `options` ask for.
    ///
    /// # Panics
    /// If `color_view` is `Some` while `options.color` is `None`, or the
    /// reverse: the attachment and its format must be present together. If
    /// both the view and the depth format are absent: a pass needs at least
    /// one attachment.
    pub fn new(
        device: &wgpu::Device,
        color_view: Option<wgpu::TextureView>,
        options: RendererOptions,
    ) -> Self {
        assert_eq!(
            color_view.is_some(),
            options.color.is_some(),
            "the color attachment and its format must be present together"
        );
        assert!(
            color_view.is_some() || options.depth.is_some(),
            "a render pass needs at least one attachment: a depth-only \
             pass still requires `RendererOptions::depth`"
        );
        let mut textures = Vec::new();
        let depth_view = options.depth.map(|format| {
            let texture = transient_texture(device, "wgpu_unlit_render::depth", format, &options);
            let view = texture.create_view(&Default::default());
            textures.push(texture);
            view
        });
        let msaa_view = options.color.and_then(|format| {
            (options.sample_count > 1).then(|| {
                let texture =
                    transient_texture(device, "wgpu_unlit_render::msaa", format, &options);
                let view = texture.create_view(&Default::default());
                textures.push(texture);
                view
            })
        });
        let renderer = Renderer {
            color_view,
            depth_view,
            msaa_view,
        };
        let mut graph = ResourceGraph::new();
        let mut register = |resource: Resource| {
            graph
                .insert(resource, &[])
                .expect("an empty dependency list always resolves")
        };
        if let Some(view) = renderer.color_view.as_ref() {
            register(Resource::TextureView(view.clone()));
            register(Resource::Texture(view.texture().clone()));
        }
        if let Some(view) = renderer.depth_view.as_ref() {
            register(Resource::TextureView(view.clone()));
        }
        if let Some(view) = renderer.msaa_view.as_ref() {
            register(Resource::TextureView(view.clone()));
        }
        Self {
            renderer,
            _textures: textures,
            graph,
            pipelines: HashMap::new(),
            stale: false,
        }
    }

    /// The key that identifies a built-in pipeline for the current
    /// configuration.
    pub fn pipeline_key(&self, options: &UnlitOptions) -> PipelineKey {
        PipelineKey {
            options: options.clone(),
            color_format: self.renderer.color_format(),
            depth_format: self.renderer.depth_format(),
            sample_count: self.renderer.sample_count(),
        }
    }

    /// The built-in pipeline for `key`, building it on first request and
    /// rebuilding it if the configuration changed since.
    ///
    /// The pipeline lives in the context's [`ResourceGraph`], so callers who
    /// register their own resources alongside it see the same ledger.
    pub fn pipeline(&mut self, key: &PipelineKey) -> Option<&UnlitPipeline> {
        if self.stale {
            self.rebuild();
        }
        self.pipelines.get(key)
    }

    /// The resource graph holding the raw wgpu resources behind the
    /// renderer's attachments.
    ///
    /// A caller registers their own textures, buffers and bind groups here
    /// and records what depends on what; replacing an attachment texture
    /// marks its dependents dirty through the same mechanism.
    pub fn graph(&mut self) -> &mut ResourceGraph {
        &mut self.graph
    }

    /// The renderer, recording the scenes the caller builds around pipelines
    /// from [`Self::pipeline`].
    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    /// Point the context at a new color view of the same format and size.
    /// The pipelines are rebuilt at the next frame.
    ///
    /// A change of format or size goes through [`Self::set_options`], since
    /// the transient attachments follow them.
    pub fn set_color_view(&mut self, color_view: Option<wgpu::TextureView>) {
        self.renderer.color_view = color_view;
        self.stale = true;
    }

    /// Change the configuration: the transient attachments are recreated and
    /// every cached pipeline is rebuilt at the next frame.
    ///
    /// The pipelines are invalidated rather than dropped: a caller drawing
    /// the same scene at a new size rebuilds instead of re-registering.
    ///
    /// # Panics
    /// Under the same conditions as [`Self::new`].
    pub fn set_options(
        &mut self,
        device: &wgpu::Device,
        color_view: Option<wgpu::TextureView>,
        options: RendererOptions,
    ) {
        *self = Self::new(device, color_view, options);
        self.stale = true;
    }

    /// The renderer's current configuration.
    pub fn options(&self) -> RendererOptions {
        RendererOptions {
            color: self.renderer.color_format(),
            depth: self.renderer.depth_format(),
            width: self.renderer.width(),
            height: self.renderer.height(),
            sample_count: self.renderer.sample_count(),
        }
    }

    /// Whether the configuration changed and has not been rebuilt yet.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Drop every cached pipeline, so the next [`Self::pipeline`] call
    /// rebuilds what it is asked for. Called implicitly by
    /// [`Self::pipeline`] and [`Self::render`] after a configuration change.
    pub fn rebuild(&mut self) {
        self.pipelines.clear();
        self.stale = false;
    }

    /// Record `scene` into `encoder` as one render pass, rebuilding stale
    /// pipelines first if the configuration changed.
    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        clear: wgpu::Color,
        scene: &crate::scene::Scene<'_>,
    ) {
        if self.stale {
            self.rebuild();
        }
        self.renderer.render(encoder, clear, scene);
    }
}

/// Create a transient attachment texture: cleared and consumed inside a
/// single pass, so it is never sampled or copied afterwards.
///
/// The usage must be exactly `RENDER_ATTACHMENT | TRANSIENT_ATTACHMENT`;
/// anything more makes the texture non-transient.
fn transient_texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    options: &RendererOptions,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: options.width.max(1),
            height: options.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: options.sample_count,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
        view_formats: &[],
    })
}
