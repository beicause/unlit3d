use super::*;
use unlit_wgpu::context::GpuContext as Gpu;

impl Bundle for SceneFrame {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            SceneAdvance(Some(self.advance)),
            SequenceStep(self.step),
            SequenceClock(0.0),
            SequenceStepped(true),
        ]
    }
}

/// Run the scene's own per-frame behaviour, which needs `&mut World`.
pub(crate) struct RunSceneAdvance {
    /// The entity carrying the scene's [`SceneAdvance`] and sequence state.
    pub(crate) entity: Entity,
    /// The frame the behaviour is advanced by.
    pub(crate) frame: Frame,
}

impl Command for RunSceneAdvance {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let Self { entity, frame } = *self;
        let advance = world
            .root_scope()
            .query_one::<&mut SceneAdvance>(entity)
            .ok()
            .and_then(|mut slot| slot.get().0.take());
        let Some(mut advance) = advance else {
            return Ok(());
        };
        advance(world, frame.index, frame.delta_time, frame.size);
        let _ = world
            .root_scope()
            .query_one::<&mut SceneAdvance>(entity)
            .map(|mut slot| slot.get().0 = Some(advance));
        Ok(())
    }
}

/// Run one MCP command against a nested world of the host.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub(crate) struct RunInWorld {
    /// The host entity carrying the world the command names.
    pub(crate) entity: Entity,
    /// The command to run.
    pub(crate) command: unlit_ecs::mcp::McpCommand,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Command for RunInWorld {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let Self { entity, command } = *self;
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(entity) else {
            command.fail(format!("host entity {entity:?} carries no world"));
            return Ok(());
        };
        command.dispatch(&mut *nested.get());
        Ok(())
    }
}

/// Run one MCP command against the host world itself.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub(crate) struct RunInHost {
    /// The command to run.
    pub(crate) command: unlit_ecs::mcp::McpCommand,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Command for RunInHost {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        self.command.dispatch(world);
        Ok(())
    }
}

/// The GPU context, once the asynchronous setup has landed.
pub(crate) fn gpu(world: &World, app: Entity) -> Option<Gpu> {
    let mut state = world.root_scope().query_one::<&GpuState>(app).ok()?;
    match state.get() {
        GpuState::Ready(gpu) => Some(gpu.clone()),
        GpuState::Idle | GpuState::Requested => None,
    }
}

/// Build the presentation path once the scene's queued world has landed.
///
/// The window is created before the GPU request, so the raw surface can travel
/// through the asynchronous setup. `BuildPresenter` creates the library surface
/// from that raw handle on the main thread, after the scene world exists.
pub(crate) struct BuildPresenter {
    pub(crate) app: Entity,
    pub(crate) scene: Entity,
    pub(crate) renderer: Entity,
    pub(crate) pass: Entity,
    pub(crate) gpu: Gpu,
    pub(crate) size: (u32, u32),
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) path: PresentationPath,
}

impl Command for BuildPresenter {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let presenter = match build_presenter(
            world,
            Some(self.scene),
            self.renderer,
            self.pass,
            self.surface,
            &self.gpu.instance,
            &self.gpu.adapter,
            self.size,
            self.path,
        ) {
            Ok(presenter) => presenter,
            Err(error) => {
                log::error!("failed to build the presentation path: {error}");
                return Ok(());
            }
        };
        let entity = world.root_scope_mut().spawn(bundle![presenter]);
        world
            .root_scope()
            .query_one::<&mut PresenterEntity>(self.app)?
            .get()
            .0 = Some(entity);
        Ok(())
    }
}
