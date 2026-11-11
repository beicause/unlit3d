use super::*;

/// Run the scene world's input dispatch for the frame.
pub(crate) fn dispatch_input_on_frame() -> OnFrame {
    OnFrame::new(|world, _entity, _frame| {
        dispatch_input(world);
    })
}

/// Decide whether this frame advances the scene's sequence.
///
/// A scene with a fixed step advances once its clock has accumulated one; one
/// without advances every frame. The decision is recorded in `SequenceStepped`
/// for the behaviour that runs the advance.
pub(crate) fn step_sequence(entity: Entity) -> OnFrame {
    OnFrame::new(move |world, _entity, frame| {
        let stepped = match world
            .root_scope()
            .query_one::<&SequenceStep>(entity)
            .ok()
            .and_then(|mut step| step.get().0)
        {
            Some(step) => world
                .root_scope()
                .query_one::<&mut SequenceClock>(entity)
                .map(|mut clock| tick(&mut clock.get().0, step, frame.delta_time))
                .expect("the scene frame entity carries a sequence clock"),
            None => true,
        };
        let _ = world
            .root_scope()
            .query_one::<&mut SequenceStepped>(entity)
            .map(|mut flag| flag.get().0 = stepped);
    })
}

/// Run the scene's own per-frame behaviour when the sequence advanced, and
/// count the frame.
///
/// The behaviour needs `&mut World`, which a frame callback does not have, so
/// it is queued; the index is captured before it is counted on, so the
/// behaviour is handed the frame it advances from.
pub(crate) fn run_scene_advance(entity: Entity) -> OnFrame {
    OnFrame::with_order(SCENE_ADVANCE_ORDER, move |world, _entity, frame| {
        let stepped = world
            .root_scope()
            .query_one::<&SequenceStepped>(entity)
            .is_ok_and(|mut flag| flag.get().0);
        if !stepped {
            return;
        }
        world.root_scope().queue().push(RunSceneAdvance {
            entity,
            frame: *frame,
        });
        frame.index = frame.index.wrapping_add(1);
    })
}

/// Build the scene and its surface, and short-circuit the frame until both are
/// ready.
///
/// The scene's world is spawned through the queue, so it lands on a later
/// `apply`; the surface is built out of that world. Each frame retries, and
/// the frames that cannot yet draw are skipped.
pub(crate) fn ensure_presented(app: Entity) -> OnFrame {
    OnFrame::with_order(READY_ORDER, move |world, _entity, _frame| {
        let resumed = world
            .root_scope()
            .query::<(Entity, &Resumed)>()
            .iter()
            .next()
            .is_some_and(|(_, state)| state.0);
        if !resumed {
            FrameSkip::skip(world);
            return;
        }
        if world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .map_or(true, |mut slot| slot.get().0.is_none())
        {
            present(world, app);
            FrameSkip::skip(world);
            return;
        }
        if presenter_entity(world, app).is_some() {
            return;
        }
        present(world, app);
        if presenter_entity(world, app).is_none() {
            FrameSkip::skip(world);
        }
    })
}

/// Open a URL the UI asked for, in a new tab when it asked for one.
///
/// The UI cannot open it itself: a panel runs while the world is borrowed and
/// has no page to reach, so it publishes the request in the world and the frame
/// loop serves it here — the same split the fullscreen request follows. One
/// call covers every platform the example builds for.
pub(crate) fn open_url(url: &OpenUrl) {
    // The target hint only means anything in a browser, where it decides
    // between a new tab and the page the app is already on; elsewhere it is
    // ignored.
    let target = if url.new_tab { "_blank" } else { "_self" };
    let mut options = webbrowser::BrowserOptions::new();
    options.with_target_hint(target);
    if let Err(error) =
        webbrowser::open_browser_with_options(webbrowser::Browser::Default, &url.url, &options)
    {
        log::debug!("the URL {} could not be opened: {error}", url.url);
    }
}

/// Serve the scene selector, the fullscreen button and the UI's URL requests.
///
/// A scene switch drops the scene's world, so nothing may still be borrowed
/// from it when the switch is served: the control state is read in one block
/// and acted on after.
pub(crate) fn serve_scene_controls(app: Entity) -> OnFrame {
    OnFrame::with_order(CONTROLS_ORDER, move |world, _entity, _frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let active = window(world).is_some_and(|window| fullscreen::active(&window));
        let (selector_pos, gpu_info_pos, switch, fullscreen) = {
            let Ok(mut nested) = world.root_scope().query_one::<&World>(scene_entity) else {
                return;
            };
            let (selector_pos, gpu_info_pos) = {
                let mut scene = world
                    .root_scope()
                    .query_one::<&Scene>(scene_entity)
                    .expect("the scene component exists");
                (
                    scene.get().selector_rect(nested.get()),
                    scene.get().gpu_info_rect(nested.get()),
                )
            };
            let switch = world
                .root_scope()
                .query_one::<&mut Scene>(scene_entity)
                .ok()
                .and_then(|mut scene| scene.get().pending.take());
            let fullscreen = world
                .root_scope()
                .query_one::<&mut Scene>(scene_entity)
                .is_ok_and(|mut scene| scene.get().fullscreen_request(nested.get(), active));
            (selector_pos, gpu_info_pos, switch, fullscreen)
        };
        let _ = world
            .root_scope()
            .query_one::<&mut SelectorPos>(app)
            .map(|mut slot| slot.get().0 = selector_pos);
        let _ = world
            .root_scope()
            .query_one::<&mut GpuInfoPos>(app)
            .map(|mut slot| slot.get().0 = gpu_info_pos);
        if let Some(def) = switch {
            switch_scene(world, app, def);
            FrameSkip::skip(world);
            return;
        }
        if fullscreen && let Some(window) = window(world) {
            fullscreen::toggle(&window);
        }
        // The UI publishes the URLs it was asked to open in the scene's own
        // world, so it is drained there rather than here in the host.
        if let Ok(mut nested) = world.root_scope().query_one::<&World>(scene_entity) {
            for url in drain_open_urls(nested.get()) {
                open_url(&url);
            }
        }
    })
}

/// Advance the scene by the frame's delta.
pub(crate) fn advance_scene(app: Entity) -> OnFrame {
    OnFrame::with_order(ADVANCE_ORDER, move |world, _entity, frame| {
        if FrameSkip::is_skipped(world) {
            return;
        }
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(scene_entity) else {
            return;
        };
        let _ = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .map(|mut scene| {
                scene.get().advance(&mut *nested.get(), frame.delta_time);
            });
    })
}

/// Restate the scene's letterboxed region on the pass that draws it.
///
/// A window resize changes the region, and the library's present renders the
/// pass this frame, so this runs before it. The blit pass, when there is one,
/// keeps its full-target viewport. A skipped frame still restates the region:
/// keeping the pass's viewport in step with the window costs nothing and
/// leaves the pass right for the frame that draws again.
pub(crate) fn restate_viewport(app: Entity) -> OnFrame {
    OnFrame::with_order(VIEWPORT_ORDER, move |world, _entity, _frame| {
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let (scene_pass, viewport) = {
            let mut scene = world
                .root_scope()
                .query_one::<&Scene>(scene_entity)
                .expect("the scene entity carries a scene");
            (scene.get().pass, scene.get().viewport)
        };
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(scene_entity) else {
            return;
        };
        nested
            .get()
            .root_scope()
            .query_one::<&mut FramePass>(scene_pass)
            .map(|mut pass| pass.get().viewport = viewport)
            .expect("the scene's own pass exists");
    })
}

/// Apply the commands the scene world queued while presenting.
///
/// The library's present renders the scene world, so its commands are applied
/// after it -- a later order -- rather than by the presenter itself. A skipped
/// frame still flushes: the commands queue up on the scene world either way,
/// and dropping none of them keeps the scene's own record intact.
pub(crate) fn flush_scene(app: Entity) -> OnFrame {
    OnFrame::with_order(FLUSH_ORDER, move |world, _entity, _frame| {
        let Some(scene_entity) = world
            .root_scope()
            .query_one::<&SceneEntity>(app)
            .ok()
            .and_then(|mut slot| slot.get().0)
        else {
            return;
        };
        let Ok(mut nested) = world.root_scope().query_one::<&mut World>(scene_entity) else {
            return;
        };
        let _ = world
            .root_scope()
            .query_one::<&mut Scene>(scene_entity)
            .map(|mut scene| {
                if let Err(error) = scene.get().end_frame(&mut *nested.get()) {
                    log::error!("a command queued by a scene failed: {error}");
                }
            });
    })
}
