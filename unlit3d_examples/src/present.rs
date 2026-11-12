use super::*;

/// Bring the scene and its presentation path up, as far as the state allows.
///
/// Spawning the scene world and building its surface both need `&mut World`,
/// so they are queued. The next frame finishes whichever stage has landed.
pub(crate) fn present(world: &World, app: Entity) {
    let Some(gpu) = gpu(world, app) else {
        return;
    };
    let Some(window) = window(world) else {
        return;
    };
    let window_size = window.inner_size();
    let size = (window_size.width.max(1), window_size.height.max(1));
    let def = world
        .root_scope()
        .query_one::<&InitialScene>(app)
        .expect("the app entity carries its initial scene")
        .get()
        .0;
    let mcp = world
        .root_scope()
        .query_one::<&McpEnabled>(app)
        .is_ok_and(|mut mcp| mcp.get().0);
    let scene_entity = match world
        .root_scope()
        .query_one::<&SceneEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0)
    {
        Some(entity) => entity,
        None => {
            let (scene_world, scene) = Scene::new(
                gpu.device.clone(),
                gpu.queue.clone(),
                gpu.capabilities,
                gpu.health(),
                size,
                scenes::SceneOptions {
                    ui: def.ui,
                    selector: true,
                    reproducible: false,
                    letterbox: true,
                    sequence_step: def.step_seconds,
                },
                def,
                PanelRects::read(world, app),
            );
            let entity = world.root_scope().queue().spawn(RenderWorld {
                world: scene_world,
                scene,
                name: "render",
            });
            let _ = world
                .root_scope()
                .query_one::<&mut SceneEntity>(app)
                .map(|mut slot| slot.get().0 = Some(entity));
            window.set_title(&format!(
                "unlit3d{} — {}",
                if mcp { " (MCP)" } else { "" },
                def.title
            ));
            return;
        }
    };
    if world
        .root_scope()
        .query_one::<&World>(scene_entity)
        .is_err()
    {
        return;
    }
    let (renderer, pass) = {
        let mut scene = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .expect("the scene entity carries a scene");
        scene.get().resize(size, def.baseline);
        (scene.get().renderer, scene.get().pass)
    };
    if presenter_entity(world, app).is_some() {
        return;
    }
    let surface = match gpu.instance.create_surface(window.clone()) {
        Ok(surface) => surface,
        Err(error) => {
            log::error!("failed to build the presentation surface: {error}");
            return;
        }
    };
    let path = if mcp {
        PresentationPath::Intermediate(TargetConfig {
            samples: def.samples,
            with_depth: def.depth,
            ..TargetConfig::default()
        })
    } else {
        PresentationPath::Surface {
            samples: SAMPLE_COUNT,
            with_depth: true,
        }
    };
    world.root_scope().queue().push(BuildPresenter {
        app,
        scene: scene_entity,
        renderer,
        pass,
        gpu,
        size,
        surface,
        path,
    });
    window.request_redraw();
}

/// The entity carrying the presentation path, while one is up.
pub(crate) fn presenter_entity(world: &World, app: Entity) -> Option<Entity> {
    world
        .root_scope()
        .query_one::<&PresenterEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0)
}

/// Release the swap chain, keeping the presentation path for a later resume.
pub(crate) fn release_surface(world: &World, app: Entity) {
    let Some(entity) = presenter_entity(world, app) else {
        return;
    };
    let released = world
        .root_scope()
        .query_one::<&mut Presenter>(entity)
        .map(|mut presenter| presenter.get().release(world));
    if let Ok(Err(error)) = released {
        log::error!("failed to release the presentation path: {error}");
    }
}

/// Release the presentation path and drop it, before its scene goes.
pub(crate) fn release_presenter(world: &World, app: Entity) {
    release_surface(world, app);
    let entity = world
        .root_scope()
        .query_one::<&mut PresenterEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0.take());
    if let Some(entity) = entity {
        world.root_scope().queue().despawn(entity);
    }
}

#[cfg(target_os = "android")]
pub(crate) fn release(world: &World, app: Entity) {
    release_surface(world, app);
}

/// Rebuild the app around a different scene.
pub(crate) fn switch_scene(world: &World, app: Entity, def: &'static scenes::SceneDef) {
    // The target and blit belong to the scene world, so both go with it.
    // The switching frame skips drawing before the replacement is built.
    release_presenter(world, app);
    if let Some(entity) = world
        .root_scope()
        .query_one::<&mut SceneEntity>(app)
        .ok()
        .and_then(|mut slot| slot.get().0.take())
    {
        world.root_scope().queue().despawn(entity);
    }
    if let Ok(mut initial) = world.root_scope().query_one::<&mut InitialScene>(app) {
        initial.get().0 = def;
    }
    present(world, app);
}
