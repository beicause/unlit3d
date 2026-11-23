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
use hashbrown::{HashMap, HashSet};
use unlit_wgpu::resources::TextureExt;
use vello_gpu::filter_effects::FilterPrimitive;
use vello_gpu::kurbo::{Affine, Diagonal2, Rect, Shape, Stroke};
use vello_gpu::peniko::color::palette::css;
use vello_gpu::peniko::{
    BlendMode, Color, Fill, FontData, ImageBrush, ImageData, ImageFormat, StyleRef,
};
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

/// What became of an image a page drew.
enum UploadedImage {
    /// The texture the scene samples, and whether any of its texels is not
    /// opaque, which lets the renderer skip the opaque sampling path.
    Texture { may_have_transparency: bool },
    /// An image the device cannot hold, which is drawn as nothing.
    ///
    /// It is remembered like an uploaded one so the page asks for it once
    /// rather than on every frame that draws it.
    Missing,
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
    /// The images the recording under way has drawn.
    drawn: HashSet<u64>,
    /// The images the recording before that one drew.
    previous: HashSet<u64>,
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
            drawn: HashSet::new(),
            previous: HashSet::new(),
            layers: Vec::new(),
        }
    }

    /// Start a new page, dropping what the last one recorded.
    ///
    /// An image outlives the recording that drew it: the page after it is most
    /// often the same page with a control moved, and its pictures are the same
    /// pictures. One the page has stopped drawing is let go of once neither of
    /// the last two recordings named it, so a page that cycles through pictures
    /// holds a bounded number of them without re-uploading one it draws every
    /// frame.
    pub(crate) fn clear(&mut self) {
        let mut evicted = Vec::new();
        self.images.retain(|id, _| {
            if forgotten(*id, &self.drawn, &self.previous) {
                evicted.push(*id);
                false
            } else {
                true
            }
        });
        for id in evicted {
            self.bindings.remove(TextureId(id));
        }
        // This recording's images become the last two recordings' worth the
        // next one is measured against.
        core::mem::swap(&mut self.previous, &mut self.drawn);
        self.drawn.clear();
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

/// `paint` with every colour it holds multiplied by `alpha`.
///
/// `peniko::Brush` multiplies alpha itself, but only for the image type its
/// default parameter names, and a scene's paint carries an uploaded texture id
/// instead of image bytes.
fn scale_alpha(paint: PaintType, alpha: f32) -> PaintType {
    match paint {
        PaintType::Solid(color) => PaintType::Solid(color.multiply_alpha(alpha)),
        PaintType::Gradient(gradient) => PaintType::Gradient(gradient.multiply_alpha(alpha)),
        PaintType::Image(image) => PaintType::Image(image.multiply_alpha(alpha)),
    }
}

impl Painter {
    /// The vello paint `paint` names, uploading what it stands for.
    fn convert_paint(&mut self, paint: PaintRef<'_>) -> PaintType {
        match paint {
            Paint::Solid(color) => PaintType::Solid(color),
            Paint::Gradient(gradient) => PaintType::Gradient(gradient.clone()),
            Paint::Image(brush) => match self.upload(brush.image) {
                Some(image) => PaintType::Image(ImageBrush {
                    image,
                    sampler: brush.sampler,
                }),
                None => PaintType::Solid(css::TRANSPARENT),
            },
            // Nothing serves a page's images as a resource, so neither of these
            // can arrive; drawing them as nothing keeps a page painting rather
            // than failing the frame.
            Paint::Resource(_) | Paint::Custom(_) => PaintType::Solid(css::TRANSPARENT),
        }
    }

    /// The source `image` is sampled from, or [`None`] if it is drawn as nothing.
    ///
    /// An image is uploaded the first time a page draws it and kept by the id of
    /// the bytes it came from, so every page the target shows shares one copy of
    /// each picture. [`Painter::clear`] decides when one is let go of.
    fn upload(&mut self, image: &ImageData) -> Option<ImageSource> {
        let id = image.data.id();
        self.drawn.insert(id);
        if let Some(uploaded) = self.images.get(&id) {
            return match uploaded {
                UploadedImage::Texture {
                    may_have_transparency,
                } => Some(ImageSource::external_texture(
                    TextureId(id),
                    TextureRegion::Full,
                    *may_have_transparency,
                )),
                UploadedImage::Missing => None,
            };
        }

        // Vello's own conversion asserts that an image fits a 16-bit extent and
        // is a format it knows, so both are turned away before it is reached: a
        // page may name an image the device cannot hold, and one that large is
        // drawn as nothing rather than failing the page.
        if !drawable(image, self.limit) {
            self.images.insert(id, UploadedImage::Missing);
            return None;
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
        // The page format is uncompressed, so one texel is one step of a row.
        let texel = super::PAGE_FORMAT
            .block_copy_size(None)
            .expect("the page format is uncompressed");
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
                bytes_per_row: Some(width * texel),
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
            UploadedImage::Texture {
                may_have_transparency,
            },
        );
        Some(ImageSource::external_texture(
            TextureId(id),
            TextureRegion::Full,
            may_have_transparency,
        ))
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
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = anyrender::Glyph> + Clone,
    ) {
        // The brush alpha reaches the paint itself rather than a layer: text is
        // drawn straight into the page's scene, so a layer would need a matching
        // pop. Multiplying each kind of brush keeps the alpha correct without one.
        let paint = scale_alpha(self.convert_paint(paint.into()), brush_alpha);
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
/// drawn without the effect rather than with only its first step. That covers
/// the CSS colour filters — brightness, contrast, grayscale and the rest —
/// which a filter graph states as one colour-matrix step: they are drawn
/// without rather than wrongly, since Vello has no colour-matrix primitive.
fn convert_filter(filter: Arc<filters::Filter>) -> Option<vello_gpu::filter_effects::Filter> {
    let [node] = filter.nodes() else {
        return None;
    };
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

/// Whether a painter lets go of image `id`.
///
/// A recording and the one before it are both allowed to name an image: the one
/// being recorded has not been read yet when the recording before it is
/// measured, so letting go of a picture on a single recording that did not name
/// it would drop one the page is about to draw again.
fn forgotten(id: u64, drawn: &HashSet<u64>, previous: &HashSet<u64>) -> bool {
    !drawn.contains(&id) && !previous.contains(&id)
}

/// Whether a scene can draw `image` at all.
///
/// Vello's conversion asserts that the extent fits 16 bits and that the format
/// is one it knows, so an image either of those rules out would fail the frame
/// rather than draw nothing: it is turned away here instead.
fn drawable(image: &ImageData, limit: u32) -> bool {
    let limit = limit.min(u32::from(u16::MAX));
    let known = matches!(image.format, ImageFormat::Rgba8 | ImageFormat::Bgra8);
    known && image.width <= limit && image.height <= limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyrender::filters::color_transformation::ColorMatrix;

    /// Image data of `width` by `height`, with no texels behind it: whether a
    /// scene would draw it is decided before any of them are read.
    fn image(width: u32, height: u32, format: ImageFormat) -> ImageData {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(Vec::new());
        ImageData {
            data: vello_gpu::peniko::Blob::new(data),
            format,
            alpha_type: vello_gpu::peniko::ImageAlphaType::Alpha,
            width,
            height,
        }
    }

    #[test]
    fn an_image_past_the_extent_vello_names_is_turned_away() {
        assert!(drawable(&image(64, 64, ImageFormat::Rgba8), 4096));
        assert!(drawable(&image(4096, 4096, ImageFormat::Bgra8), 4096));
        assert!(!drawable(&image(4097, 64, ImageFormat::Rgba8), 4096));
        assert!(!drawable(&image(64, 4097, ImageFormat::Rgba8), 4096));
        // A device whose textures may be larger than Vello's extents does not
        // extend them: what the scene cannot name is still too large.
        assert!(!drawable(&image(70_000, 8, ImageFormat::Rgba8), u32::MAX));
    }

    #[test]
    fn a_picture_is_kept_until_two_recordings_pass_without_it() {
        let drawn = HashSet::from_iter([1]);
        let previous = HashSet::from_iter([2]);
        assert!(!forgotten(1, &drawn, &previous), "one drawn now is kept");
        assert!(
            !forgotten(2, &drawn, &previous),
            "one the recording before drew is kept, because the page may draw it again"
        );
        assert!(
            forgotten(3, &drawn, &previous),
            "one neither recording named is let go of"
        );
    }

    #[test]
    fn a_single_primitive_the_scene_knows_is_recorded() {
        let flood = FilterEffect::Flood(css::TRANSPARENT);
        assert!(convert_filter(Arc::new(filters::Filter::single(flood))).is_some());
    }

    #[test]
    fn a_colour_filter_is_drawn_without() {
        // A graph of one colour matrix is how a CSS brightness, grayscale or
        // sepia arrives, and the scene has no primitive for it.
        let matrix = FilterEffect::ColorMatrix(ColorMatrix::brightness(1.5));
        assert!(convert_filter(Arc::new(filters::Filter::single(matrix))).is_none());
    }

    #[test]
    fn a_graph_of_several_primitives_is_drawn_without() {
        let graph = filters::Filter::linear_list(
            [css::TRANSPARENT, css::TRANSPARENT]
                .into_iter()
                .map(FilterEffect::Flood),
        );
        assert_eq!(graph.nodes().len(), 2);
        assert!(convert_filter(Arc::new(graph)).is_none());
    }
}
