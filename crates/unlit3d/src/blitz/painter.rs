//! The Vello GPU painter a page is recorded through.
//!
//! `blitz-paint` draws a laid-out document through `PaintScene`, the drawing
//! interface `anyrender` defines; this adapter forwards those calls to
//! `vello_gpu::Scene`, which turns them into strips the GPU draws. A painter
//! belongs to one target at one size: the scene, the renderer and the resources
//! the renderer draws with are all of them that target's.
//!
//! Two things Vello's GPU backend cannot do decide the shape of the adapter. It
//! has no way to upload an image, and `blitz-paint` hands images over as
//! `Paint::Image` rather than as a resource the drawing side could register.
//! Every image a page draws is therefore uploaded here, into a texture of its
//! own that the painter keeps, and bound by id when the scene is rendered.

use std::sync::Arc;

use anyrender::filters::{self, EdgeMode, FilterEffect};
use anyrender::{NormalizedCoord, Paint, PaintRef, PaintScene, RenderContext};
use glifo::FontEmbolden;
use hashbrown::HashMap;
use unlit_wgpu::resources::TextureExt;
use vello_gpu::filter_effects::FilterPrimitive;
use vello_gpu::kurbo::{Affine, Diagonal2, Rect, Shape, Stroke};
use vello_gpu::peniko::{BlendMode, Color, Fill, FontData, ImageBrush, ImageData, StyleRef};
use vello_gpu::{ImageSource, PaintType, TextureId, TextureRegion};

/// How far a path may deviate from its flattened form, in device pixels.
const DEFAULT_TOLERANCE: f64 = 0.1;

/// What a pushed clip wants popped.
enum Layer {
    /// A layer pushed through `PaintScene::push_layer`, which pushed a clip path
    /// and the layer itself.
    Layer,
    /// A clip pushed through `PaintScene::push_clip_layer`, which pushed only the
    /// path.
    Clip,
}

/// An image a page drew, uploaded once and drawn with from then on.
struct UploadedImage {
    /// Whether any of its texels is not opaque, which lets the renderer skip the
    /// opaque sampling path.
    may_have_transparency: bool,
}

/// A page's drawing commands, and the GPU state that rasterizes them.
pub(crate) struct Painter {
    /// The device the images are uploaded with.
    device: wgpu::Device,
    /// The queue the images are uploaded through.
    queue: wgpu::Queue,
    /// The largest texture the device makes, which bounds an image.
    limit: u32,
    /// What turns the scene into strips.
    renderer: vello_gpu::Renderer,
    /// The renderer's persistent state, handed to it on every render.
    resources: vello_gpu::Resources,
    /// The page's drawing commands, recorded through the `PaintScene` impl.
    scene: vello_gpu::Scene,
    /// The image textures the scene's paints name.
    bindings: vello_gpu::TextureBindings,
    /// The images themselves, keyed by the id of the bytes they came from, so a
    /// page that draws one image in several places uploads it once.
    images: HashMap<u64, UploadedImage>,
    /// The clips pushed and not yet popped.
    layers: Vec<Layer>,
}

impl Painter {
    /// A painter for a target of `size`.
    ///
    /// The renderer is built for the target's format, which its pipelines are
    /// compiled for, and resizes itself to whatever target it is given.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32)) -> Self {
        let (renderer, resources) = vello_gpu::Renderer::new(
            device,
            queue,
            &vello_gpu::RenderTargetConfig {
                format: super::PAGE_FORMAT,
                width: size.0 as u16,
                height: size.1 as u16,
            },
        );
        Self {
            device: device.clone(),
            queue: queue.clone(),
            limit: device.limits().max_texture_dimension_2d,
            renderer,
            resources,
            scene: vello_gpu::Scene::new(size.0 as u16, size.1 as u16),
            bindings: vello_gpu::TextureBindings::new(),
            images: HashMap::new(),
            layers: Vec::new(),
        }
    }

    /// Start a new page, dropping what the last one recorded.
    ///
    /// The images a page drew stay uploaded: the one after it is most often the
    /// same page with a control moved, and its pictures are the same pictures.
    pub(crate) fn clear(&mut self) {
        self.scene.reset();
        self.layers.clear();
    }

    /// Draw the recorded scene into `view`, recording the work in `encoder`.
    ///
    /// The commands are recorded, not submitted: the frame submits them with
    /// everything else it drew.
    #[expect(
        clippy::disallowed_types,
        reason = "the view is handed to Vello as the raw handle it renders into"
    )]
    pub(crate) fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    ) -> Result<(), vello_gpu::RenderError> {
        self.renderer.render(
            &self.scene,
            &mut self.resources,
            encoder,
            vello_gpu::RenderTarget {
                view,
                depth_view: None,
            },
            &self.bindings,
            vello_gpu::RenderOptions::default(),
        )
    }
}

impl Painter {
    /// The vello paint `paint` names, uploading what it stands for.
    fn convert_paint(&mut self, paint: PaintRef<'_>) -> PaintType {
        match paint {
            Paint::Solid(color) => PaintType::Solid(color),
            Paint::Gradient(gradient) => PaintType::Gradient(gradient.clone()),
            Paint::Image(brush) => PaintType::Image(ImageBrush {
                image: self.upload(brush.image),
                sampler: brush.sampler,
            }),
            // Nothing serves a page's images as a resource, so neither of these
            // can arrive; drawing them as nothing keeps a page painting rather
            // than failing the frame.
            Paint::Resource(_) | Paint::Custom(_) => {
                PaintType::Solid(vello_gpu::peniko::color::palette::css::TRANSPARENT)
            }
        }
    }

    /// The source `image` is sampled from, uploading it the first time.
    fn upload(&mut self, image: &ImageData) -> ImageSource {
        let id = image.data.id();
        if let Some(uploaded) = self.images.get(&id) {
            return ImageSource::external_texture(
                TextureId(id),
                TextureRegion::Full,
                uploaded.may_have_transparency,
            );
        }

        // The conversion is what premultiplies the texels, which is the state a
        // scene draws them in, and is the only way to get them as bytes.
        let ImageSource::Pixmap(pixmap) = ImageSource::from_peniko_image_data(image) else {
            unreachable!("converting image data always yields a pixmap");
        };
        let width = u32::from(pixmap.width());
        let height = u32::from(pixmap.height());
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // A page may name an image the device cannot make a texture of; one that
        // large is drawn as nothing rather than failing the page.
        if size.width > self.limit || size.height > self.limit {
            return ImageSource::external_texture(TextureId(id), TextureRegion::Full, true);
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blitz::image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: super::PAGE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            pixmap.data_as_u8_slice(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            size,
        );
        let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
        self.bindings.insert(TextureId(id), view.view().clone());

        // The binding map holds the view, and the view holds the texture, so
        // the upload lives as long as the page can draw with it.
        let may_have_transparency = pixmap.may_have_transparency();
        self.images.insert(
            id,
            UploadedImage {
                may_have_transparency,
            },
        );
        ImageSource::external_texture(TextureId(id), TextureRegion::Full, may_have_transparency)
    }
}

impl RenderContext for Painter {}

impl PaintScene for Painter {
    fn reset(&mut self) {
        self.clear();
    }

    fn push_layer(
        &mut self,
        fill: Fill,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
        filter: Option<Arc<filters::Filter>>,
        _backdrop_filter: Option<Arc<filters::Filter>>,
    ) {
        // A scene records a filter of the layer it is pushed on, not of what is
        // behind it, so a backdrop filter is drawn without rather than wrongly.
        let filter = filter.and_then(convert_filter);
        self.scene.set_transform(transform);
        self.scene.set_fill_rule(fill);
        self.layers.push(Layer::Layer);
        self.scene
            .push_clip_path(&clip.into_path(DEFAULT_TOLERANCE));
        self.scene
            .push_layer(None, Some(blend.into()), Some(alpha), None, filter);
    }

    fn push_clip_layer(&mut self, fill: Fill, transform: Affine, clip: &impl Shape) {
        self.scene.set_transform(transform);
        self.scene.set_fill_rule(fill);
        self.layers.push(Layer::Clip);
        self.scene
            .push_clip_path(&clip.into_path(DEFAULT_TOLERANCE));
    }

    fn pop_layer(&mut self) {
        match self.layers.pop() {
            Some(Layer::Layer) => {
                self.scene.pop_layer();
                self.scene.pop_clip();
            }
            Some(Layer::Clip) => self.scene.pop_clip(),
            None => {}
        }
    }

    fn stroke<'a>(
        &mut self,
        style: &Stroke,
        transform: Affine,
        paint: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.scene.set_transform(transform);
        self.scene.set_stroke(style.clone());
        let paint = self.convert_paint(paint.into());
        self.scene.set_paint(paint);
        self.scene
            .set_paint_transform(brush_transform.unwrap_or(Affine::IDENTITY));
        self.scene.stroke_path(&shape.into_path(DEFAULT_TOLERANCE));
    }

    fn fill<'a>(
        &mut self,
        style: Fill,
        transform: Affine,
        paint: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.scene.set_transform(transform);
        self.scene.set_fill_rule(style);
        let paint = self.convert_paint(paint.into());
        self.scene.set_paint(paint);
        self.scene
            .set_paint_transform(brush_transform.unwrap_or(Affine::IDENTITY));
        self.scene.fill_path(&shape.into_path(DEFAULT_TOLERANCE));
    }

    fn draw_glyphs<'a, 's: 'a>(
        &'a mut self,
        font: &'a FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'a [NormalizedCoord],
        embolden: vello_gpu::kurbo::Vec2,
        style: impl Into<StyleRef<'a>>,
        paint: impl Into<PaintRef<'a>>,
        _brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = anyrender::Glyph> + Clone,
    ) {
        let paint = self.convert_paint(paint.into());
        self.scene.set_paint(paint);
        self.scene.reset_paint_transform();
        self.scene.set_transform(transform);

        // A glyph the font cannot draw is reported to the run rather than to the
        // page: the rest of the page still paints.
        let _ = match style.into() {
            StyleRef::Fill(fill) => {
                self.scene.set_fill_rule(fill);
                self.scene
                    .glyph_run(&mut self.resources, font)
                    .font_size(font_size)
                    .hint(hint)
                    .normalized_coords(normalized_coords)
                    .font_embolden(FontEmbolden::new(Diagonal2::new(embolden.x, embolden.y)))
                    .glyph_transform(glyph_transform.unwrap_or_default())
                    .fill_glyphs(glyphs.map(|glyph| glifo::Glyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    }))
            }
            StyleRef::Stroke(stroke) => {
                self.scene.set_stroke(stroke.clone());
                self.scene
                    .glyph_run(&mut self.resources, font)
                    .font_size(font_size)
                    .hint(hint)
                    .normalized_coords(normalized_coords)
                    .glyph_transform(glyph_transform.unwrap_or_default())
                    .stroke_glyphs(glyphs.map(|glyph| glifo::Glyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    }))
            }
        };
    }

    fn draw_box_shadow(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.scene.set_transform(transform);
        self.scene.set_paint(PaintType::Solid(color));
        self.scene.reset_paint_transform();
        self.scene
            .fill_blurred_rounded_rect(&rect, radius as f32, std_dev as f32);
    }
}

/// The filter `filter` describes, as far as a scene can record it.
///
/// The scene takes one primitive per layer, so a graph of several of them is
/// drawn without the effect rather than with only its first step.
fn convert_filter(filter: Arc<filters::Filter>) -> Option<vello_gpu::filter_effects::Filter> {
    let node = filter.nodes().first()?;
    let primitive = convert_filter_effect(&node.effect)?;
    Some(vello_gpu::filter_effects::Filter::from_primitive(primitive))
}

/// The vello primitive `effect` stands for, if the scene has one.
fn convert_filter_effect(effect: &FilterEffect) -> Option<FilterPrimitive> {
    Some(match effect {
        FilterEffect::Flood(color) => FilterPrimitive::Flood { color: *color },
        FilterEffect::GaussianBlur(blur) => FilterPrimitive::GaussianBlur {
            std_deviation_x: blur.std_deviation,
            std_deviation_y: blur.std_deviation,
            edge_mode: convert_edge_mode(blur.edge_mode),
        },
        FilterEffect::DropShadow(shadow) => FilterPrimitive::DropShadow {
            dx: shadow.dx,
            dy: shadow.dy,
            std_deviation: shadow.std_deviation,
            color: shadow.color,
            edge_mode: convert_edge_mode(shadow.edge_mode),
        },
        FilterEffect::Offset(offset) => FilterPrimitive::Offset {
            dx: offset.x as f32,
            dy: offset.y as f32,
        },
        _ => return None,
    })
}

fn convert_edge_mode(edge_mode: EdgeMode) -> vello_gpu::filter_effects::EdgeMode {
    match edge_mode {
        EdgeMode::Duplicate => vello_gpu::filter_effects::EdgeMode::Duplicate,
        EdgeMode::Wrap => vello_gpu::filter_effects::EdgeMode::Wrap,
        EdgeMode::Mirror => vello_gpu::filter_effects::EdgeMode::Mirror,
        EdgeMode::None => vello_gpu::filter_effects::EdgeMode::None,
    }
}
