//! The offscreen target shared by the headless and interactive drivers.

use unlit_ecs::{Entity, World};
use unlit_wgpu::{context::RenderDevice, readback::try_readback_texture, resources::ResHandle};
use unlit3d::offscreen::{PersistentTarget, TargetConfig};
use unlit3d::renderer::Renderer;

use super::RenderSetup;

pub(super) fn bind(
    world: &World,
    setup: RenderSetup,
    device: &RenderDevice,
    size: (u32, u32),
) -> Result<(Entity, PersistentTarget), String> {
    let limit = device.device.limits().max_texture_dimension_2d;
    if size.0 == 0 || size.1 == 0 || size.0 > limit || size.1 > limit {
        return Err(format!(
            "invalid offscreen size {}×{}: dimensions must be in 1..={limit}",
            size.0, size.1
        ));
    }
    let renderer = world
        .resolve(setup.renderer)
        .map_err(|error| error.to_string())?;
    let pass = world
        .resolve(setup.pass)
        .map_err(|error| error.to_string())?;
    let context = world
        .root_scope()
        .try_query_one::<&Renderer>(renderer)
        .map_err(|error| format!("the offscreen renderer is unavailable: {error}"))?
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
    .map_err(|error| format!("the offscreen target could not be bound: {error}"))?;
    Ok((renderer, target))
}

pub(super) fn render(world: &World, renderer: Entity) -> Result<(), String> {
    world
        .root_scope()
        .try_query_one::<&mut Renderer>(renderer)
        .map_err(|error| format!("the offscreen renderer is unavailable: {error}"))?
        .get()
        .render(world);
    Ok(())
}

pub(super) fn texture(
    world: &World,
    renderer: Entity,
    target: &ResHandle<wgpu::Texture>,
) -> Result<wgpu::Texture, String> {
    let context = world
        .root_scope()
        .try_query_one::<&Renderer>(renderer)
        .map_err(|error| format!("the offscreen renderer is unavailable: {error}"))?
        .get()
        .context();
    context
        .try_graph(&world.root_scope())
        .map_err(|error| format!("the frame's resource graph is unavailable: {error}"))?
        .get()
        .get(target)
        .cloned()
        .ok_or_else(|| "the offscreen texture is unavailable".to_owned())
}

pub(super) async fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<(u32, u32, Vec<u8>), String> {
    let pixels = try_readback_texture(device, queue, texture)
        .await
        .map_err(|error| format!("the offscreen target could not be read back: {error}"))?;
    Ok((texture.width(), texture.height(), pixels))
}
