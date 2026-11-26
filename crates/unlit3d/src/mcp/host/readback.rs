//! MCP buffer and image readback, encoding, and file output.

use base64::Engine as _;
use image::ImageEncoder as _;
use unlit_ecs::mcp::host::{SavedFile, extension_of, saved_name, to_value, write_file};
use unlit_ecs::mcp::serde_json::Value;
use unlit_ecs::mcp::{EncodedImage, Output, ToolError};
use unlit_ecs::{Entity, World};
use unlit_wgpu::readback::{try_readback_buffer, try_readback_texture};
use unlit_wgpu::resources::{ResId, Resource};

use super::{graph_of, render_context, render_frame};
use crate::prelude::RenderContext;

/// The bytes read out of a buffer.
#[derive(facet::Facet)]
struct BufferRead {
    /// The offset the read started at.
    offset: u64,
    /// How many bytes were read.
    size: usize,
    /// The bytes, base64-encoded.
    data: String,
}

fn device_of(world: &World, context: RenderContext) -> Result<wgpu::Device, ToolError> {
    context
        .device(&world.root_scope())
        .map_err(|_| ToolError::rejected("the render context has no device"))
}

/// The queue the context names, cloned out of the world.
fn queue_of(world: &World, context: RenderContext) -> Result<wgpu::Queue, ToolError> {
    context
        .queue(&world.root_scope())
        .map_err(|_| ToolError::rejected("the render context has no queue"))
}

/// The graph the context names, borrowed from the world.
/// The bytes a buffer read produced, with the offset it started at.
fn read_buffer_bytes(
    world: &World,
    id: ResId,
    offset: u64,
    size: Option<u64>,
) -> Result<(u64, Vec<u8>), ToolError> {
    let context = render_context(world)?;
    let buffer = {
        let mut graph_row = graph_of(world, context)?;
        let graph = graph_row.get();
        let handle = graph
            .resolve(id)
            .ok_or_else(|| ToolError::rejected(format!("stale resource id {}", id.to_bits())))?;
        match graph.get::<Resource>(&handle) {
            Some(Resource::Buffer(buffer)) => buffer.clone(),
            _ => {
                return Err(ToolError::rejected(format!(
                    "resource id {} is not a buffer",
                    id.to_bits()
                )));
            }
        }
    };
    if !buffer.usage().contains(wgpu::BufferUsages::COPY_SRC) {
        return Err(ToolError::rejected(format!(
            "the buffer behind id {} was not created with COPY_SRC, so it cannot be read back",
            id.to_bits()
        )));
    }
    let total = buffer.size();
    if offset > total {
        return Err(ToolError::rejected(format!(
            "offset {offset} is past the buffer's {total} bytes"
        )));
    }
    // A copy reads whole aligned runs: wgpu rejects anything else outright, and
    // a caller that named the edges of a field cannot know the alignment, so
    // the misalignment is reported here instead of as a copy wgpu refused.
    let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
    if !offset.is_multiple_of(alignment) {
        return Err(ToolError::rejected(format!(
            "offset {offset} is not a multiple of the {alignment} bytes a buffer copy reads in"
        )));
    }
    let size = size.unwrap_or(total - offset).min(total - offset);
    if !size.is_multiple_of(alignment) {
        return Err(ToolError::rejected(format!(
            "size {size} is not a multiple of the {alignment} bytes a buffer copy reads in"
        )));
    }
    if size == 0 {
        // Nothing to copy, and a staging buffer of no bytes is not a buffer
        // wgpu will create.
        return Ok((offset, Vec::new()));
    }
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    // The tool runs inside a synchronous closure on the world's thread, so the
    // readback's future is driven to completion here rather than awaited.
    let bytes = pollster::block_on(try_readback_buffer(&device, &queue, &buffer, offset, size))
        .map_err(|error| ToolError::failed(format!("failed to read the buffer back: {error}")))?;
    Ok((offset, bytes))
}

/// Read bytes out of a buffer resource.
pub(crate) fn read_buffer(
    world: &World,
    id: ResId,
    offset: u64,
    size: Option<u64>,
) -> Result<Value, ToolError> {
    let (offset, bytes) = read_buffer_bytes(world, id, offset, size)?;
    to_value(&BufferRead {
        offset,
        size: bytes.len(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Read bytes out of a buffer resource and write them to a file.
pub(crate) fn save_buffer(
    world: &World,
    id: ResId,
    offset: u64,
    size: Option<u64>,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, ToolError> {
    let (_, bytes) = read_buffer_bytes(world, id, offset, size)?;
    let file_name = saved_name(file_name, "buffer", "bin");
    to_value(&write_file(directory, &file_name, &bytes)?)
}

struct Frame {
    /// The pixel bytes.
    bytes: Vec<u8>,
    /// The frame's width in pixels.
    width: u32,
    /// The frame's height in pixels.
    height: u32,
    /// The format the bytes are in.
    format: wgpu::TextureFormat,
}

impl Frame {
    /// The color type an encoder reads these bytes as.
    fn color(&self) -> Result<image::ExtendedColorType, ToolError> {
        match self.format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
                Ok(image::ExtendedColorType::Rgba8)
            }
            other => Err(ToolError::rejected(format!(
                "cannot encode a {other:?} texture"
            ))),
        }
    }

    /// The frame's pixels with their alpha channel dropped, for an encoder
    /// that takes no alpha.
    fn rgb8(&self) -> Result<Vec<u8>, ToolError> {
        self.color()?;
        let mut rgb = Vec::with_capacity(self.bytes.len() / 4 * 3);
        for pixel in self.bytes.as_chunks::<4>().0 {
            rgb.extend_from_slice(&pixel[..3]);
        }
        Ok(rgb)
    }
}

/// The image format an image tool writes or sends, named by a file's
/// extension or by a format argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFormat {
    /// PNG.
    Png,
    /// Lossless WebP.
    Webp,
    /// JPEG, which is lossy and carries no alpha channel.
    Jpeg,
}

impl ImageFormat {
    /// The format an image tool uses when it is asked for none.
    const DEFAULT: Self = Self::Webp;

    /// The format `name` names, or an error when it names none this server
    /// can write.
    fn from_name(name: &str) -> Result<Self, ToolError> {
        match name.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::Webp),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            other => Err(ToolError::rejected(format!(
                "cannot write a {other:?} image; use .png, .webp, .jpg or .jpeg"
            ))),
        }
    }

    /// The format the file named `file_name` asks for.
    fn of(file_name: &str) -> Result<Self, ToolError> {
        extension_of(file_name).map_or_else(
            || {
                Err(ToolError::rejected(format!(
                    "the file name {file_name:?} has no extension"
                )))
            },
            Self::from_name,
        )
    }

    /// The MIME type an image content block carries.
    fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Jpeg => "image/jpeg",
        }
    }

    /// Encode a frame read back from a texture.
    fn encode(self, frame: &Frame) -> Result<Vec<u8>, ToolError> {
        let color = frame.color()?;
        let mut bytes = Vec::new();
        match self {
            Self::Png => image::codecs::png::PngEncoder::new(&mut bytes).write_image(
                &frame.bytes,
                frame.width,
                frame.height,
                color,
            ),
            Self::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut bytes).write_image(
                &frame.bytes,
                frame.width,
                frame.height,
                color,
            ),
            Self::Jpeg => image::codecs::jpeg::JpegEncoder::new(&mut bytes).write_image(
                &frame.rgb8()?,
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgb8,
            ),
        }
        .map_err(|error| ToolError::failed(error.to_string()))?;
        Ok(bytes)
    }
}

/// Encode a frame into `directory`, as the image format `file_name` names.
fn save_image(directory: &str, file_name: &str, frame: &Frame) -> Result<SavedFile, ToolError> {
    let bytes = ImageFormat::of(file_name)?.encode(frame)?;
    write_file(directory, file_name, &bytes)
}

/// The frame a texture read produced.
fn read_texture_frame(world: &World, id: ResId) -> Result<Frame, ToolError> {
    let context = render_context(world)?;
    let texture = {
        let mut graph_row = graph_of(world, context)?;
        let graph = graph_row.get();
        let handle = graph
            .resolve(id)
            .ok_or_else(|| ToolError::rejected(format!("stale resource id {}", id.to_bits())))?;
        match graph.get::<Resource>(&handle) {
            Some(Resource::Texture(texture)) => texture.clone(),
            Some(Resource::TextureView(view)) => view.texture().clone(),
            _ => {
                return Err(ToolError::rejected(format!(
                    "resource id {} is not a texture",
                    id.to_bits()
                )));
            }
        }
    };
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return Err(ToolError::rejected(format!(
            "the texture behind id {} was not created with COPY_SRC, so it cannot be read back",
            id.to_bits()
        )));
    }
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = pollster::block_on(try_readback_texture(&device, &queue, &texture))
        .map_err(|error| ToolError::failed(format!("failed to read the texture back: {error}")))?;
    Ok(Frame {
        bytes,
        width: texture.width(),
        height: texture.height(),
        format: texture.format(),
    })
}

/// Read a texture resource and encode it as an inline image.
pub(crate) fn read_texture_as_image(
    world: &World,
    id: ResId,
    format: Option<&str>,
) -> Result<Output, ToolError> {
    let format = match format {
        Some(name) => ImageFormat::from_name(name)?,
        None => ImageFormat::DEFAULT,
    };
    let frame = read_texture_frame(world, id)?;
    image_value(&format.encode(&frame)?, frame.width, frame.height, format)
}

/// Read a texture resource and write it to a file as an image.
pub(crate) fn save_texture_as_image(
    world: &World,
    id: ResId,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, ToolError> {
    let frame = read_texture_frame(world, id)?;
    let file_name = saved_name(file_name, "texture", "webp");
    to_value(&save_image(directory, &file_name, &frame)?)
}

/// The offscreen texture a screenshot reads.
///
/// `id` may name the texture or any view of it, since a caller that draws
/// into an offscreen target knows the view its pass binds.
fn screenshot_target(
    world: &World,
    context: RenderContext,
    id: ResId,
) -> Result<wgpu::Texture, ToolError> {
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    let handle = graph
        .resolve(id)
        .ok_or_else(|| ToolError::rejected(format!("stale resource id {}", id.to_bits())))?;
    let texture = match graph.get::<Resource>(&handle) {
        Some(Resource::Texture(texture)) => texture.clone(),
        Some(Resource::TextureView(view)) => view.texture().clone(),
        _ => {
            return Err(ToolError::rejected(format!(
                "resource id {} is not a texture",
                id.to_bits()
            )));
        }
    };
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return Err(ToolError::rejected(format!(
            "resource id {} is a texture no one can read back: it lacks COPY_SRC",
            id.to_bits()
        )));
    }
    Ok(texture)
}

/// Render one frame and read the offscreen target back.
fn screenshot_frame(
    world: &mut World,
    renderer: Option<Entity>,
    target: ResId,
) -> Result<Frame, ToolError> {
    let context = render_context(world)?;
    let texture = screenshot_target(world, context, target)?;
    render_frame(world, renderer)?;
    let device = device_of(world, context)?;
    let queue = queue_of(world, context)?;
    let bytes = pollster::block_on(try_readback_texture(&device, &queue, &texture))
        .map_err(|error| ToolError::failed(format!("failed to read the texture back: {error}")))?;
    Ok(Frame {
        bytes,
        width: texture.width(),
        height: texture.height(),
        format: texture.format(),
    })
}

/// Render one frame and read the offscreen target back as an inline image.
pub(crate) fn read_screenshot(
    world: &mut World,
    renderer: Option<Entity>,
    target: ResId,
    format: Option<&str>,
) -> Result<Output, ToolError> {
    let format = match format {
        Some(name) => ImageFormat::from_name(name)?,
        None => ImageFormat::DEFAULT,
    };
    let frame = screenshot_frame(world, renderer, target)?;
    image_value(&format.encode(&frame)?, frame.width, frame.height, format)
}

/// Render one frame and write the offscreen target to a file as an image.
pub(crate) fn save_screenshot(
    world: &mut World,
    renderer: Option<Entity>,
    target: ResId,
    directory: &str,
    file_name: Option<&str>,
) -> Result<Value, ToolError> {
    let frame = screenshot_frame(world, renderer, target)?;
    let file_name = saved_name(file_name, "screenshot", "webp");
    to_value(&save_image(directory, &file_name, &frame)?)
}

/// An image's size, as structured output.
#[derive(facet::Facet)]
struct ImageSize {
    /// The image's width in pixels.
    width: u32,
    /// The image's height in pixels.
    height: u32,
}

/// An encoded image the transport sends as an image content block.
fn image_value(
    bytes: &[u8],
    width: u32,
    height: u32,
    format: ImageFormat,
) -> Result<Output, ToolError> {
    Ok(Output::Image(EncodedImage {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime_type: format.mime_type().to_string(),
        structured: to_value(&ImageSize { width, height })?,
    }))
}

#[cfg(test)]
mod tests {
    use super::ImageFormat;

    #[test]
    fn an_extension_names_the_image_format() {
        assert_eq!(
            ImageFormat::of("a.png").expect("Png is a known format"),
            ImageFormat::Png
        );
        assert_eq!(
            ImageFormat::of("a.webp").expect("Webp is a known format"),
            ImageFormat::Webp
        );
        assert_eq!(
            ImageFormat::of("a.jpg").expect("Jpeg is a known format"),
            ImageFormat::Jpeg
        );
        assert_eq!(
            ImageFormat::of("a.jpeg").expect("Jpeg is a known format"),
            ImageFormat::Jpeg
        );
        assert_eq!(
            ImageFormat::of("a.JPG").expect("Jpeg is a known format"),
            ImageFormat::Jpeg
        );
        assert_eq!(
            ImageFormat::of("a.PNG").expect("Png is a known format"),
            ImageFormat::Png
        );
        assert_eq!(
            ImageFormat::from_name("jpeg").expect("Jpeg is a known format"),
            ImageFormat::Jpeg
        );
        assert!(ImageFormat::of("a").is_err());
        assert!(ImageFormat::of("a.gif").is_err());
    }
}
