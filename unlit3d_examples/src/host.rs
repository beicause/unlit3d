use super::*;
use unlit_wgpu::context::GpuContext as Gpu;

/// Request the GPU context once a window exists to present through.
///
/// The window is created by the host at the end of the resume callback, so the
/// request waits for a later frame; a `GpuState` that is no longer idle keeps
/// a later frame from starting it twice.
pub(crate) fn request_gpu(app: Entity) -> OnFrame {
    OnFrame::new(move |world, _entity, _frame| {
        if !world
            .root_scope()
            .query_one::<&GpuState>(app)
            .is_ok_and(|mut state| matches!(state.get(), GpuState::Idle))
        {
            return;
        }
        let Some(window) = window(world) else {
            return;
        };
        let Some(display) = world
            .root_scope()
            .query::<(Entity, &DisplayHandle)>()
            .iter()
            .next()
            .map(|(_, handle)| handle.0.clone())
        else {
            return;
        };
        let Ok(proxy) = world
            .root_scope()
            .query_one::<&WinitProxy<UserEvent>>(app)
            .map(|mut proxy| proxy.get().0.clone())
        else {
            return;
        };
        let _ = world
            .root_scope()
            .query_one::<&mut GpuState>(app)
            .map(|mut state| *state.get() = GpuState::Requested);
        // The display handle is owned, not borrowed: the instance it builds has
        // to outlive this callback. The descriptor is the environment-aware one
        // so `WGPU_BACKEND` still selects a backend.
        spawn(async move {
            let instance = wgpu::util::new_instance_with_webgpu_detection(
                wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)),
            )
            .await;
            // A high-performance adapter, so a machine with two GPUs draws with
            // the discrete one; `UNLIT3D_DEVICE_TIER` decides how much of the
            // WebGPU baseline the device is asked for.
            let request = Gpu::present_to(
                instance,
                window,
                DeviceTier::from_env(),
                wgpu::PowerPreference::HighPerformance,
                "unlit3d_examples",
            )
            .await;
            match request {
                Ok(gpu) => {
                    let _ = proxy.send_event(UserEvent::Ready(gpu));
                }
                Err(error) => {
                    let _ = proxy.send_event(UserEvent::Failed(error.to_string()));
                }
            }
        });
    })
}

/// Serve the events the app posts to itself.
pub(crate) fn handle_user_event(app: Entity) -> OnUserEvent<UserEvent> {
    OnUserEvent::new(move |world, _entity, event| match event {
        UserEvent::Ready(gpu) => {
            let _ = world
                .root_scope()
                .query_one::<&mut GpuState>(app)
                .map(|mut state| {
                    *state.get() = GpuState::Ready(gpu.clone());
                });
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
            if world
                .root_scope()
                .query_one::<&McpEnabled>(app)
                .is_ok_and(|mut mcp| mcp.get().0)
            {
                start_mcp(world, app);
            }
            present(world, app);
        }
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        UserEvent::Command(command) => {
            if let Some(command) = command.take() {
                dispatch_command(world, command);
            }
        }
        UserEvent::Failed(error) => {
            log::error!("failed to start the renderer: {error}");
            let _ = world
                .root_scope()
                .query_one::<&mut ExitRequest>(app)
                .map(|mut request| request.get().0 = true);
        }
    })
}

/// Re-state the scene and the surface for a new window size.
pub(crate) fn resize_scene(app: Entity) -> OnWindowEvent {
    OnWindowEvent::new(move |world, _entity, payload| {
        let WindowEvent::Resized(size) = &payload.event else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let size = (size.width, size.height);
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let baseline = world
            .root_scope()
            .query_one::<&InitialScene>(app)
            .expect("the app entity carries its initial scene")
            .get()
            .0
            .baseline;
        let _ = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .map(|mut scene| scene.get().resize(size, baseline));
        let Some(presenter) = presenter_entity(world, app) else {
            return;
        };
        let resized = world
            .root_scope()
            .query_one::<&mut Presenter>(presenter)
            .map(|mut presenter| presenter.get().resize(world, size));
        if let Ok(Err(error)) = resized {
            log::error!("failed to resize the presentation path: {error}");
        }
    })
}

/// Drop the swap chain the platform is about to invalidate.
#[cfg(target_os = "android")]
pub(crate) fn release_on_suspend(app: Entity) -> OnSuspended {
    OnSuspended::new(move |world, _entity, ()| {
        release(world, app);
    })
}

/// Start the MCP stdio transport, which posts each command back to the loop.
///
/// It runs on its own thread because serving the protocol blocks; the proxy is
/// what carries a command back, because a world is not `Send`.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub(crate) fn start_mcp(world: &World, app: Entity) {
    let Ok(proxy) = world
        .root_scope()
        .query_one::<&WinitProxy<UserEvent>>(app)
        .map(|mut proxy| proxy.get().0.clone())
    else {
        return;
    };
    std::thread::Builder::new()
        .name("unlit3d-mcp-transport".to_owned())
        .spawn(move || {
            let sink = move |command| proxy.send_event(UserEvent::Command(Some(command))).is_ok();
            if let Err(error) = unlit_ecs::mcp::serve_stdio_with(sink) {
                log::error!("the MCP server stopped: {error}");
            }
        })
        .expect("the MCP transport thread starts");
}

/// Queue one MCP command against the world it names.
///
/// Running it needs `&mut World`, so it is queued as a command and applied
/// between two event callbacks. The host answers `list_worlds` itself: it is
/// the only one that knows which entities carry a world and what they are
/// called.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub(crate) fn dispatch_command(world: &World, command: unlit_ecs::mcp::McpCommand) {
    use unlit_ecs::mcp::{WorldEntry, WorldRef};
    if command.wants_worlds() {
        let mut worlds = vec![WorldEntry {
            world: WorldRef::Host,
            name: "host",
        }];
        for (entity, _) in world.root_scope().query::<(Entity, &World)>().iter() {
            let Ok(mut name) = world.root_scope().query_one::<&WorldName>(entity) else {
                continue;
            };
            let name = name.get();
            worlds.push(WorldEntry {
                world: WorldRef::handle(entity),
                name: name.0,
            });
        }
        command.answer_worlds(worlds);
        return;
    }
    match command.world().entity() {
        Some(entity) => world
            .root_scope()
            .queue()
            .push(RunInWorld { entity, command }),
        None => world.root_scope().queue().push(RunInHost { command }),
    }
}
