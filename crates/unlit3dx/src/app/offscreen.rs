//! The offscreen target shared by the headless and interactive drivers.

use unlit_ecs::{Entity, EntityAccessError, RemoteEntityError, World};
use unlit_wgpu::{context::RenderDevice, readback::try_readback_texture, resources::ResHandle};
use unlit3d::offscreen::{PersistentTarget, TargetConfig};
use unlit3d::renderer::Renderer;

use super::RenderSetup;

/// Why the offscreen target could not be built, drawn or read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OffscreenError {
    /// The requested size is not one the device can hold.
    #[error("invalid offscreen size {width}×{height}: dimensions must be in 1..={limit}")]
    Size {
        /// The width asked for.
        width: u32,
        /// The height asked for.
        height: u32,
        /// The largest dimension the device supports.
        limit: u32,
    },
    /// A remote renderer or pass does not name anything in this world.
    #[error("the render setup names an entity this world does not hold: {0}")]
    Resolve(#[from] RemoteEntityError),
    /// The renderer the setup names is gone, or is not a renderer.
    #[error("the offscreen renderer is unavailable: {0}")]
    Renderer(#[source] EntityAccessError),
    /// The target's pass or the frame's resource graph is gone.
    #[error("the offscreen target could not be bound: {0}")]
    Target(#[source] EntityAccessError),
    /// The frame's resource graph is gone.
    #[error("the frame's resource graph is unavailable: {0}")]
    Graph(#[source] EntityAccessError),
    /// The target's texture is not in the graph.
    #[error("the offscreen texture is unavailable")]
    Texture,
    /// The texture could not be read back.
    #[error("the offscreen target could not be read back: {0}")]
    Readback(#[from] unlit_wgpu::readback::ReadbackError),
}

pub(super) fn bind(
    world: &World,
    setup: RenderSetup,
    device: &RenderDevice,
    size: (u32, u32),
) -> Result<(Entity, PersistentTarget), OffscreenError> {
    let limit = device.device.limits().max_texture_dimension_2d;
    if size.0 == 0 || size.1 == 0 || size.0 > limit || size.1 > limit {
        return Err(OffscreenError::Size {
            width: size.0,
            height: size.1,
            limit,
        });
    }
    let renderer = world.resolve(setup.renderer)?;
    let pass = world.resolve(setup.pass)?;
    let context = world
        .root_scope()
        .try_query_one::<&Renderer>(renderer)
        .map_err(OffscreenError::Renderer)?
        .get()
        .context();
    let target = PersistentTarget::bind(
        world,
        context,
        pass,
        &device.device,
        size,
        TargetConfig::default(),
    )
    .map_err(OffscreenError::Target)?;
    Ok((renderer, target))
}

pub(super) fn render(world: &World, renderer: Entity) -> Result<(), OffscreenError> {
    world
        .root_scope()
        .try_query_one::<&mut Renderer>(renderer)
        .map_err(OffscreenError::Renderer)?
        .get()
        .render(world);
    Ok(())
}

pub(super) fn texture(
    world: &World,
    renderer: Entity,
    target: &ResHandle<wgpu::Texture>,
) -> Result<wgpu::Texture, OffscreenError> {
    let context = world
        .root_scope()
        .try_query_one::<&Renderer>(renderer)
        .map_err(OffscreenError::Renderer)?
        .get()
        .context();
    context
        .try_graph(&world.root_scope())
        .map_err(OffscreenError::Graph)?
        .get()
        .get(target)
        .cloned()
        .ok_or(OffscreenError::Texture)
}

pub(super) async fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<(u32, u32, Vec<u8>), OffscreenError> {
    let pixels = try_readback_texture(device, queue, texture).await?;
    Ok((texture.width(), texture.height(), pixels))
}
