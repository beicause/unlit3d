use super::*;

/// The scene's world and the frame loop that drives it.
///
/// Deliberately knows nothing about how a frame reaches a display: it advances
/// the scene's behaviour and renders into whatever target is bound, so the
/// windowed run and the snapshot tests differ only in what they bind and what
/// with the result. The scene itself is a [`scenes::SceneDef`] whose build
/// populated the world and returned the [`SceneControl`] this drives.
///
/// Public because the snapshot tests drive the same scenes offscreen: the web
/// renders them and compares each frame against its snapshot, which is the
/// comparison they make.
///
/// The world is *not* owned here: a scene is the driving state of a world the
/// caller owns, and every frame method takes that world back as `&mut World`.
/// A windowed run stores the world as a component of its host world and pulls
/// it out per frame; the snapshot tests keep a local one. Either way one world
/// has exactly one scene driving it, and the scene never reaches across to
/// another.
pub struct Scene {
    /// The renderer resource entity: the handle every renderer access goes
    /// through.
    pub renderer: Entity,
    /// The size the scene's content is drawn at, in pixels: the target's own,
    /// or the letterboxed part of it when [`Self::viewport`] is set.
    ///
    /// This is what the scene's camera is built for and what its behaviour is
    /// handed, so the projection matches what the viewport scales into place.
    size: (u32, u32),
    /// The region of the target the 3D content is drawn into, or `None` to draw
    /// into the whole of it.
    ///
    /// Set when the scene declares an aspect of its own — see
    /// [`scenes::SceneDef::baseline`] — so a target of any shape shows the same
    /// picture at a different scale rather than showing more of it.
    pub viewport: Option<ViewportRect>,
    /// The frame pass the scene's own sources record into.
    ///
    /// Everything a scene builds hangs under it, which is what puts the scene
    /// in the frame: a pass records the sources in its own scope, so the pass a
    /// source's entity sits under is the pass its scene is drawn in.
    pub pass: Entity,
    /// The snapshot the scene's test compares each frame against, if any.
    ///
    /// The scene's per-frame behaviour is not here: it was moved into the
    /// scene's own world as a frame behaviour at build time, so a frame
    /// advances it without `&mut Scene`.
    pub snapshot: scenes::Snapshot,
    /// The frame-rate measurement the windowed shell's readout displays.
    ///
    /// Unused by the snapshot tests, which have no display to report a rate on.
    frame_rate: Entity,
    /// The index of the next frame the scene's behaviour advances from.
    ///
    /// Carried across calls because [`Self::advance`] hands it to the frame
    /// behaviours through the frame's context, and a fixed sequence may skip a
    /// frame without advancing it.
    frame: u32,
    /// The scene-selector's switch component, read once per frame.
    switch: Entity,
    /// The fullscreen button's request component, read once per frame.
    fullscreen: Entity,
    /// The window's fullscreen state, written once per frame for the button to
    /// label itself from: a panel runs with no window to ask.
    fullscreen_active: Entity,
    /// A scene switch requested by the selector, consumed by the frame loop.
    pub(crate) pending: Option<&'static scenes::SceneDef>,
}

impl Scene {
    /// Build the scene `def` into a fresh world.
    ///
    /// Sync, and on the main thread: the ECS world is single-threaded, so it
    /// lives on the thread the event loop runs on. The world starts with the
    /// frame's GPU context and the renderer resource entity; the scene's build
    /// adds its sources, meshes, camera and entities, and returns the
    /// [`SceneControl`] that drives them.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        capabilities: DeviceCapabilities,
        health: DeviceHealth,
        size: (u32, u32),
        options: scenes::SceneOptions,
        def: &'static scenes::SceneDef,
        panels: PanelRects,
    ) -> (World, Self) {
        let mut world = World::new();
        let context = spawn_context(
            &mut world,
            device,
            queue,
            ResourceGraph::new(),
            capabilities,
            health,
        );
        let renderer = world
            .root_scope_mut()
            .spawn(bundle![Renderer::new(context)]);
        // The pass the scene draws in. Everything the scene builds hangs under
        // it — its sources, and the UI source mounted below — because a pass
        // records the sources in its own scope: the pass a source sits under is
        // the pass its drawing lands in. Its order is the first of the frame,
        // so a presenting pass mounted later draws after it.
        let load = RenderLoadOps {
            color: match def.clear {
                Some(color) => wgpu::LoadOp::Clear(color),
                None => color_clear(),
            },
            ..RenderLoadOps::default()
        };
        let pass = world.root_scope_mut().spawn(Pass {
            load,
            ..Pass::new(SCENE_PASS_ORDER)
        });
        // The scene's content is built for the region it will be drawn into,
        // not for the whole target: a camera built for the target's aspect
        // would be sheared back out to it by the viewport. A run that does not
        // letterbox — every capture — draws into the whole target, so its
        // content size is the target's own and nothing about it changes.
        let (viewport, content) = letterbox(def.baseline, size, options.letterbox);
        // The pass carries the region as well as the scene: the driver asks the
        // pass where its sources draw, so a viewport kept only on this struct
        // would never reach the renderer and the content would stretch to the
        // whole target.
        world
            .root_scope()
            .query_one::<&mut FramePass>(pass)
            .map(|mut pass| pass.get().viewport = viewport)
            .expect("the pass was just spawned");

        // The UI source is mounted before the scene is built, so a scene's own
        // panels hang in the scope it draws: a source renders its own subtree,
        // and a panel spawned beside it would never be shown.
        let ui = (options.ui || options.selector).then(|| {
            let mut source_ui = UiSource::new();
            if options.reproducible && def.reproducible_ui {
                // egui fades a window in over the first frames and animates
                // widget transitions, all measured against the clock it is
                // handed. With no animation time every one of them is already
                // over, so the first frame is fully drawn and does not depend
                // on when it was taken.
                source_ui
                    .context_mut()
                    .all_styles_mut(|style| style.animation_time = 0.0);
            }
            world
                .scope_mut(pass)
                .expect("the pass was just spawned")
                .spawn(bundle![Source::new(source_ui)])
        });
        let control = (def.build)(&mut world, context, pass, ui, content, options);
        let SceneControl { advance, snapshot } = control;

        // The shell's own state — the selector's switch, the fullscreen
        // request and its state, the frame-rate reading — lives in the shared
        // subtree. A panel reads and writes it from the UI source's scope,
        // which reaches the shared subtree and nothing beside it.
        let shared = world.shared();
        let (switch, fullscreen, fullscreen_active, frame_rate) = {
            let mut shared_view = world.scope_mut(shared).expect("the shared scope exists");
            (
                shared_view.spawn(bundle![SceneSwitch(None)]),
                shared_view.spawn(bundle![FullscreenRequest(false)]),
                shared_view.spawn(bundle![FullscreenActive(false)]),
                shared_view.spawn(bundle![FrameRate::default()]),
            )
        };
        // The scene's per-frame state and its own behaviour, as components: the
        // frame's behaviours advance them without `&mut Scene`. One entity
        // carries them so the behaviour that advances the scene and the state
        // it reads are reached together.
        let frame_entity = world.root_scope_mut().spawn(SceneFrame {
            advance,
            step: options.sequence_step,
        });
        // The frame's own dispatch: the input runs first and states no order,
        // since nothing else here reads what it dispatched. Stepping the index
        // and advancing by the frame it names are the one dependency: the
        // advance states an order after the step that records it.
        world
            .root_scope_mut()
            .spawn(bundle![dispatch_input_on_frame()]);
        world
            .root_scope_mut()
            .spawn(bundle![step_sequence(frame_entity)]);
        world
            .root_scope_mut()
            .spawn(bundle![run_scene_advance(frame_entity)]);
        // A scene world has no window: it starts whole and nothing short-
        // circuits it. Spawning the flag lets a scene behaviour do so.
        world.root_scope_mut().spawn(bundle![FrameSkip::default()]);

        if let Some(ui) = ui
            && options.selector
        {
            mount_selector(&mut world, ui, switch, def, panels.selector);
            mount_frame_rate(&mut world, ui, frame_rate);
            let device = context
                .device(&world.root_scope())
                .expect("the device resource");
            mount_gpu_info(&mut world, ui, device, panels.gpu_info);
            // Every platform has a window the button can take fullscreen, so
            // it is drawn everywhere. A platform whose backend cannot — Android
            // already takes the whole screen for its activity — draws the
            // button and ignores the press.
            mount_fullscreen(&mut world, ui, fullscreen, fullscreen_active);
            // The API docs are a page every platform can open, so the link is
            // drawn everywhere. Mounted after the fullscreen button, which it
            // is placed under.
            mount_api_docs(&mut world, ui);
        }

        (
            world,
            Self {
                renderer,
                size: content,
                viewport,
                pass,
                snapshot,
                frame_rate,
                frame: 0,
                switch,
                fullscreen,
                fullscreen_active,
                pending: None,
            },
        )
    }

    /// Re-state the target's size, re-letterboxing the content for its shape.
    ///
    /// Called on every resize and on the first frame that knows the window's
    /// size, so the projection and the size the scene's behaviour is handed
    /// follow the target together. The region is the scene's own state here and
    /// reaches the pass that draws it from [`restate_viewport`], which is the one
    /// place holding both that pass and this scene.
    pub(crate) fn resize(&mut self, target: (u32, u32), baseline: Option<(u32, u32)>) {
        let (viewport, content) = letterbox(baseline, target, true);
        self.size = content;
        self.viewport = viewport;
    }

    /// Advance the scene by `delta_time` seconds.
    ///
    /// A frame is the dispatch of the scene world's `OnFrame` behaviours. The
    /// input and the index state no order; the scene's own behaviour states one
    /// after the index, since it advances by the frame the index names. The
    /// index and the content size travel in the frame's context, so a behaviour
    /// reads them without a component of its own.
    pub fn advance(&mut self, world: &mut World, delta_time: f32) {
        // Measured before anything else, so the reading covers the whole frame
        // — input, behaviour, render and present — and not just the part
        // between here and the next call.
        world
            .root_scope()
            .query_one::<&mut FrameRate>(self.frame_rate)
            .map(|mut rate| rate.get().push(delta_time))
            .expect("the frame-rate component exists");

        let mut frame = Frame {
            delta_time,
            index: self.frame,
            size: self.size,
        };
        dispatch_frame(world, &mut frame);
        self.frame = frame.index;

        // The selector's request, read once so the frame loop can act on it.
        self.pending = world
            .root_scope()
            .query_one::<&mut SceneSwitch>(self.switch)
            .map(|mut s| s.get().0.take())
            .expect("the switch component exists");
    }

    /// The selector window's rectangle the last time it was laid out, or
    /// `None` before it has been shown once.
    ///
    /// The window's rectangle is remembered by egui, in this scene's context;
    /// the shell reads it from here so a scene switch — which rebuilds that
    /// context — can rebuild the window in the same place and at the same size.
    pub(crate) fn selector_rect(&self, world: &World) -> Option<egui::Rect> {
        self.panel_rect(world, SELECTOR_WINDOW)
    }

    /// The GPU-info panel's rectangle the last time it was laid out, or `None`
    /// before it has been shown once.
    ///
    /// Carried across a scene switch exactly like [`Self::selector_rect`]: the
    /// panel is a window the user may drag, and the switch rebuilds the context
    /// that remembers where it was left.
    pub(crate) fn gpu_info_rect(&self, world: &World) -> Option<egui::Rect> {
        self.panel_rect(world, GPU_INFO_AREA)
    }

    /// The rectangle egui remembers for the window with `id`.
    ///
    /// Several sources share the world; only the UI one holds egui's window
    /// memory, so skip every other source's entity.
    fn panel_rect(&self, world: &World, id: &str) -> Option<egui::Rect> {
        world
            .root_scope()
            .query::<(Entity, &Source)>()
            .iter()
            .find_map(|(_, source)| {
                let ui = source.as_ref::<UiSource>().ok()?;
                ui.context()
                    .memory(|memory| memory.area_rect(egui::Id::unique(id)))
            })
    }

    /// Record whether the window is fullscreen and report whether the button
    /// asked for a change, clearing the request.
    ///
    /// Both are read and written once per frame by the windowed loop, which is
    /// what owns the window; the scene keeps no opinion of its own about
    /// fullscreen.
    pub(crate) fn fullscreen_request(&mut self, world: &World, active: bool) -> bool {
        let _ = world
            .root_scope()
            .query_one::<&mut FullscreenActive>(self.fullscreen_active)
            .map(|mut state| state.get().0 = active);
        world
            .root_scope()
            .query_one::<&mut FullscreenRequest>(self.fullscreen)
            .map(|mut request| std::mem::take(&mut request.get().0))
            .expect("the fullscreen request component exists")
    }

    /// Render one frame into whatever target the caller bound.
    ///
    /// Only the snapshot tests draw through this: the windowed run acquires
    /// the swap chain's image and renders between the acquire and the present,
    /// so it needs both under one borrow.
    pub fn render(&mut self, world: &World) {
        world
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)
            .map(|mut renderer| renderer.get().render(world))
            .expect("the renderer is a resource entity");
    }

    /// Finish the frame: apply whatever a behaviour component or panel queued.
    ///
    /// Nothing is cleared: every consumer reads the events from its own
    /// subscription, and the dispatch that reads them takes them.
    pub fn end_frame(&mut self, world: &mut World) -> Result<(), EntityAccessError> {
        world.apply()
    }
}

/// The region a scene's content is drawn into and the size its camera is built
/// for, for a `target`-pixel render target.
///
/// `baseline` is the aspect to keep, or `None` for a scene with no aspect of
/// its own — a UI-only one — which draws into the whole target. `letterbox`
/// turns the whole mechanism off, which is what a capture wants: its
/// captures are the scene's own size and must stay comparable with the stored
/// snapshots.
pub(crate) fn letterbox(
    baseline: Option<(u32, u32)>,
    target: (u32, u32),
    letterbox: bool,
) -> (Option<ViewportRect>, (u32, u32)) {
    match baseline.filter(|_| letterbox) {
        Some(baseline) => (
            Some(scenes::baseline_viewport(baseline, target)),
            scenes::content_size(baseline, target),
        ),
        None => (None, target),
    }
}

/// Accumulate `delta_time` into `clock` and report whether a `step`-long
/// interval has passed, subtracting it from the clock if so.
///
/// At most one interval is consumed per call, so a long stall advances a
/// fixed-sequence scene by one frame rather than skipping several. The
/// leftover stays in the clock, so the sequence keeps pace over time.
pub(crate) fn tick(clock: &mut f32, step: f32, delta_time: f32) -> bool {
    *clock += delta_time;
    if *clock >= step {
        *clock -= step;
        true
    } else {
        false
    }
}
