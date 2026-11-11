use super::{
    API_DOCS_AREA, API_DOCS_URL, FRAME_RATE_AREA, FULLSCREEN_AREA, FrameRate, GPU_INFO_AREA,
    GPU_INFO_MIN_WIDTH, GpuInfo, OVERLAY_BUTTON_MIN_SIZE, OVERLAY_MARGIN, api_docs_button, egui,
    frame_rate_readout, fullscreen_button, gpu_info_panel, limit_rows,
};
/// An egui input for a `screen`-point screen carrying `events`.
fn raw_input(screen: (f32, f32), events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(screen.0, screen.1),
        )),
        events,
        ..Default::default()
    }
}

/// Run one egui pass over a `screen`-point screen with `events`, drawing the
/// fullscreen button in the state `active`, and report what it drew.
///
/// What the pass drew is both what pressing the button did — the returned
/// flag — and every galley it laid out, which is how a test reads the
/// label without reaching into egui's internals.
fn fullscreen_pass(
    ctx: &egui::Context,
    screen: (f32, f32),
    active: bool,
    events: Vec<egui::Event>,
) -> (bool, Vec<String>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(screen.0, screen.1),
        )),
        events,
        ..Default::default()
    };
    let mut clicked = false;
    let mut output = ctx.run_ui(input, |ui| {
        clicked = fullscreen_button(ui, active);
    });
    // egui panics if a texture delta is dropped unapplied; a real frame
    // hands it to the integration, and this test discards it.
    output.textures_delta.clear();
    let labels = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    (clicked, labels)
}

/// The centre of the area with `id`, which a previous pass has to have
/// drawn for this to answer.
fn area_centre(ctx: &egui::Context, id: &str) -> egui::Pos2 {
    area_rect(ctx, id).center()
}

/// The rectangle of the area with `id`, which a previous pass has to have
/// drawn for this to answer.
fn area_rect(ctx: &egui::Context, id: &str) -> egui::Rect {
    ctx.memory(|memory| {
        memory
            .area_rect(egui::Id::unique(id))
            .expect("the area was drawn")
    })
}

/// The pointer events that begin a drag at `pos`: the pointer arrives and
/// the button goes down.
fn drag_start(pos: egui::Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// The pointer events that move a held pointer to `to`. The movement has
/// to be a pass of its own: egui measures a drag as the delta between two
/// passes, not between two events of one.
fn drag_move(to: egui::Pos2) -> Vec<egui::Event> {
    vec![egui::Event::PointerMoved(to)]
}

/// The pointer events that release a held pointer at `pos`.
fn drag_release(pos: egui::Pos2) -> Vec<egui::Event> {
    vec![egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    }]
}

/// The pointer events that tap `pos`: a press and, on a later pass, the
/// release that egui reports as a click.
fn tap(pos: egui::Pos2) -> (Vec<egui::Event>, Vec<egui::Event>) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    (
        vec![egui::Event::PointerMoved(pos), button(true)],
        vec![button(false)],
    )
}

/// Run one egui pass over a `screen`-point screen with `events`, drawing
/// the GPU-info panel for `info` where `initial` says, and report every
/// galley it laid out.
fn gpu_info_pass(
    ctx: &egui::Context,
    screen: (f32, f32),
    info: &GpuInfo,
    initial: Option<egui::Rect>,
    events: Vec<egui::Event>,
) -> Vec<String> {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(screen.0, screen.1),
        )),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        gpu_info_panel(ui, info, initial);
    });
    // egui panics if a texture delta is dropped unapplied; a real frame
    // hands it to the integration, and this test discards it.
    output.textures_delta.clear();
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

/// Run one egui pass over a `screen`-point screen, drawing the GPU-info
/// panel for `info`, and report every galley it laid out with the number of
/// rows it was broken into. A fact on a single row is a fact the panel was
/// wide enough to keep on one line.
fn gpu_info_rows(
    ctx: &egui::Context,
    screen: (f32, f32),
    info: &GpuInfo,
    initial: Option<egui::Rect>,
) -> Vec<(String, usize)> {
    let mut output = ctx.run_ui(raw_input(screen, Vec::new()), |ui| {
        gpu_info_panel(ui, info, initial);
    });
    // egui panics if a texture delta is dropped unapplied; a real frame
    // hands it to the integration, and this test discards it.
    output.textures_delta.clear();
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => {
                Some((text.galley.text().to_owned(), text.galley.rows.len()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_limit_list_comes_from_the_limits_own_description() {
    // The labels are wgpu's own field names, so the test names the ones the
    // type is known to carry rather than a copy of the whole list.
    let rows = limit_rows(&wgpu::Limits::defaults());
    assert!(
        rows.len() > 50,
        "a limits struct has dozens of fields, got {}",
        rows.len()
    );
    for expected in [
        ("maxBindGroups", "4"),
        ("maxTextureDimension1D", "8192"),
        ("maxBufferSize", "268435456"),
    ] {
        assert!(
            rows.iter()
                .any(|(name, value)| name == expected.0 && value == expected.1),
            "the limits do not show {expected:?}"
        );
    }
}

/// The facts a GPU-info panel is asked to show, small enough to read.
fn sample_gpu_info() -> GpuInfo {
    GpuInfo {
        name: "Test Adapter".to_owned(),
        summary: "vulkan · discrete GPU · test driver".to_owned(),
        rows: vec![("driver info".to_owned(), "1.2.3".to_owned())],
        features: vec!["DEPTH_CLIP_CONTROL", "TEXTURE_COMPRESSION_BC"],
        limits: vec![("max_bind_groups".to_owned(), "4".to_owned())],
    }
}

#[test]
fn the_gpu_info_panel_opens_in_the_bottom_left_corner() {
    // The panel is an overlay on the scene, so the first build has to put
    // it in its corner and keep it inside the window.
    for screen in [(1280.0, 577.0), (800.0, 600.0), (390.0, 844.0)] {
        let ctx = egui::Context::default();
        let info = sample_gpu_info();
        // Two passes: the first measures the content, the second lays the
        // window out against that measurement.
        let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());
        let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());

        let panel = area_rect(&ctx, GPU_INFO_AREA);
        assert!(
            (panel.left() - OVERLAY_MARGIN).abs() < 1.0,
            "at {screen:?} the panel {panel:?} does not keep the left margin"
        );
        assert!(
            (screen.1 - panel.bottom() - OVERLAY_MARGIN).abs() < 1.0,
            "at {screen:?} the panel {panel:?} does not keep the bottom margin"
        );
        assert!(
            panel.right() <= screen.0,
            "at {screen:?} the panel {panel:?} runs off the right"
        );
        assert!(
            panel.width() >= GPU_INFO_MIN_WIDTH,
            "at {screen:?} the panel {panel:?} is narrower than its floor"
        );
    }
}

#[test]
fn the_gpu_info_panel_follows_its_corner_when_the_screen_changes_shape() {
    // The panel opens against the bottom-left corner. When the display
    // rotates, the frame becomes a different shape and egui only clamps the
    // remembered rectangle, so the panel has to be moved by how far that
    // corner moved — otherwise it stays where the old, now-rotated screen
    // put it.
    let ctx = egui::Context::default();
    let info = sample_gpu_info();
    for (from, to) in [
        ((1280.0, 577.0), (577.0, 1280.0)),
        ((800.0, 600.0), (600.0, 800.0)),
    ] {
        // Two passes: the first measures the content, the second lays the
        // window out against that measurement.
        let _ = gpu_info_pass(&ctx, from, &info, None, Vec::new());
        let _ = gpu_info_pass(&ctx, from, &info, None, Vec::new());

        let _ = gpu_info_pass(&ctx, to, &info, None, Vec::new());
        let panel = area_rect(&ctx, GPU_INFO_AREA);
        assert!(
            (panel.left() - OVERLAY_MARGIN).abs() < 1.0
                && (to.1 - panel.bottom() - OVERLAY_MARGIN).abs() < 1.0,
            "after {from:?} -> {to:?} the panel {panel:?} left its corner"
        );
    }
}

#[test]
fn the_gpu_info_panel_opens_wide_enough_for_its_facts() {
    // A limit name is the device's own, and some run past fifty characters.
    // A panel that opened at a fixed width would wrap them, so every fact
    // has to come out on one row.
    let info = sample_gpu_info();
    for screen in [(1280.0, 577.0), (800.0, 600.0), (390.0, 844.0)] {
        let ctx = egui::Context::default();
        // Two passes: the first measures the content, the second lays the
        // window out against that measurement.
        let _ = gpu_info_rows(&ctx, screen, &info, None);
        for (text, rows) in gpu_info_rows(&ctx, screen, &info, None) {
            assert_eq!(
                rows, 1,
                "at {screen:?} the panel wrapped {text:?} onto {rows} rows"
            );
        }
    }
}

#[test]
fn the_gpu_info_panel_wraps_a_fact_wider_than_the_window() {
    // The panel's opening width comes from its facts, so a fact wider than
    // the window cannot be kept on one row: it is wrapped inside the window
    // instead of the panel hanging off the edge.
    let mut info = sample_gpu_info();
    info.limits
        .push(("max_long_limit_name".to_owned(), "x".repeat(200)));
    let screen = (800.0, 600.0);
    let ctx = egui::Context::default();
    let _ = gpu_info_rows(&ctx, screen, &info, None);
    let lines = gpu_info_rows(&ctx, screen, &info, None);

    let panel = area_rect(&ctx, GPU_INFO_AREA);
    assert!(
        panel.right() <= screen.0,
        "the panel {panel:?} runs off the right of the {screen:?} screen"
    );
    assert!(
        lines
            .iter()
            .any(|(text, rows)| text.starts_with("max_long_limit_name") && *rows > 1),
        "the oversized fact was not wrapped inside the window"
    );
}

#[test]
fn the_gpu_info_panel_can_be_dragged_by_its_title() {
    // The panel is a window, not a pinned overlay: a user has to be able to
    // move it off whatever it covers. The drag is a gesture spread over
    // several passes, as egui sees a real one.
    let ctx = egui::Context::default();
    let info = sample_gpu_info();
    let screen = (800.0, 600.0);
    let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());
    let _ = gpu_info_pass(&ctx, screen, &info, None, Vec::new());
    let opened = area_rect(&ctx, GPU_INFO_AREA);

    // Grab the title bar and pull the window up and to the right.
    let from = egui::pos2(opened.center().x, opened.top() + 8.0);
    let to = from + egui::vec2(60.0, -40.0);
    let _ = gpu_info_pass(&ctx, screen, &info, None, drag_start(from));
    let _ = gpu_info_pass(&ctx, screen, &info, None, drag_move(to));
    let _ = gpu_info_pass(&ctx, screen, &info, None, drag_release(to));

    let moved = area_rect(&ctx, GPU_INFO_AREA);
    assert!(
        (moved.min - opened.min - (to - from)).length() < 1.0,
        "the panel did not follow the drag: opened {opened:?}, moved {moved:?}"
    );
}

#[test]
fn the_gpu_info_panel_prints_the_adapter_features_and_limits() {
    let ctx = egui::Context::default();
    let info = sample_gpu_info();
    let _ = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new());
    let text = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new()).join("\n");

    for expected in [
        "Test Adapter",
        "vulkan · discrete GPU · test driver",
        "driver info: 1.2.3",
        "Features (2)",
        "DEPTH_CLIP_CONTROL",
        "TEXTURE_COMPRESSION_BC",
        "Limits (1)",
        "max_bind_groups = 4",
    ] {
        assert!(
            text.contains(expected),
            "the panel does not show {expected:?}"
        );
    }
}

#[test]
fn the_gpu_info_panel_says_none_when_no_feature_is_enabled() {
    let ctx = egui::Context::default();
    let mut info = sample_gpu_info();
    info.features.clear();
    let _ = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new());
    let text = gpu_info_pass(&ctx, (800.0, 600.0), &info, None, Vec::new()).join("\n");
    assert!(text.contains("Features (0)"), "the count is still stated");
    assert!(text.contains("none"), "an empty feature list is stated");
}

#[test]
fn the_fullscreen_button_offers_the_action_that_is_not_in_effect() {
    let ctx = egui::Context::default();
    // Two passes, because an anchored area is laid out against the size the
    // previous pass left it.
    for active in [false, true] {
        let _ = fullscreen_pass(&ctx, (800.0, 600.0), active, Vec::new());
        let (clicked, labels) = fullscreen_pass(&ctx, (800.0, 600.0), active, Vec::new());
        assert!(!clicked, "nothing was pressed");
        let label = labels.join(" ");
        if active {
            assert!(
                label.contains("Exit fullscreen"),
                "leaving is offered while fullscreen, got {label:?}"
            );
        } else {
            assert!(
                label.contains("Fullscreen") && !label.contains("Exit"),
                "entering is offered while windowed, got {label:?}"
            );
        }
    }
}

#[test]
fn the_fullscreen_button_reports_a_press() {
    let ctx = egui::Context::default();
    let screen = (800.0, 600.0);
    // An anchored area is laid out at the position the previous pass gave
    // it, and it is only pinned to its corner from the pass after the
    // first, so the tap is aimed at where two passes have put it.
    let _ = fullscreen_pass(&ctx, screen, false, Vec::new());
    let _ = fullscreen_pass(&ctx, screen, false, Vec::new());
    let (press, release) = tap(area_centre(&ctx, FULLSCREEN_AREA));

    // A release with no press before it is not a click, so the request is
    // not raised by a stray event.
    let (clicked, _) = fullscreen_pass(&ctx, screen, false, release.clone());
    assert!(!clicked, "a release alone is not a click");
    // Neither is a press the user has not let go of yet.
    let (clicked, _) = fullscreen_pass(&ctx, screen, false, press);
    assert!(!clicked, "a press alone is not a click");
    let (clicked, _) = fullscreen_pass(&ctx, screen, false, release);
    assert!(clicked, "the release inside the button is a click");
}

#[test]
fn the_fullscreen_button_grows_to_its_wider_label() {
    let ctx = egui::Context::default();
    // The area remembers the size of the narrower enter label, which is
    // what it is laid out against when the exit label is first drawn.
    let _ = fullscreen_pass(&ctx, (800.0, 600.0), false, Vec::new());
    let (_, enter_labels) = fullscreen_pass(&ctx, (800.0, 600.0), false, Vec::new());
    assert_eq!(enter_labels.len(), 1, "the enter label is one line");
    let enter = area_rect(&ctx, FULLSCREEN_AREA);
    let _ = fullscreen_pass(&ctx, (800.0, 600.0), true, Vec::new());
    let leaving = area_rect(&ctx, FULLSCREEN_AREA);
    // A wrapped label makes the button a row taller, which is the bug: the
    // exit label is wider than the enter one, not taller than it.
    assert!(
        (leaving.height() - enter.height()).abs() < 1.0,
        "the exit label wrapped: {enter:?} became {leaving:?}"
    );
    assert!(
        leaving.width() > enter.width(),
        "the exit label is the wider of the two: {enter:?} to {leaving:?}"
    );
}

/// The screens the corner overlays are laid out on: wide enough for all
/// three to share a row, and narrow ones — a portrait phone — where they
/// have to stack.
const OVERLAY_SCREENS: [(f32, f32); 4] = [
    (1280.0, 577.0),
    (800.0, 600.0),
    (390.0, 844.0),
    (320.0, 480.0),
];

/// Lay the three corner overlays out on `screen` until their anchored
/// positions settle, and return their rectangles.
///
/// Each of them anchors against what the previous pass remembered, so one
/// pass proves nothing: the layout is only settled once every area has a
/// geometry for the next one to read.
fn corner_rects(ctx: &egui::Context, screen: (f32, f32)) -> (egui::Rect, egui::Rect, egui::Rect) {
    let text = "60.0 fps · 16.7 ms";
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(screen.0, screen.1),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            frame_rate_readout(ui, text);
            fullscreen_button(ui, false);
            api_docs_button(ui);
        });
        output.textures_delta.clear();
    }
    (
        area_rect(ctx, FRAME_RATE_AREA),
        area_rect(ctx, FULLSCREEN_AREA),
        area_rect(ctx, API_DOCS_AREA),
    )
}

#[test]
fn the_corner_overlays_do_not_cover_each_other() {
    // The readout sits centred at the top, the fullscreen button anchors to
    // the top-right corner and the docs link under it. On a window wide
    // enough for them they share the top row, and on a narrow one — a
    // portrait phone — the button drops under the readout rather than hide
    // the end of the reading. The link then follows the button down.
    for screen in OVERLAY_SCREENS {
        let ctx = egui::Context::default();
        let (readout, button, docs) = corner_rects(&ctx, screen);
        for (first, second) in [(readout, button), (readout, docs), (button, docs)] {
            assert!(
                !first.intersects(second),
                "at {screen:?} {first:?} and {second:?} share screen space"
            );
        }

        // The readout is centred, its top a margin from the window's top.
        assert!(
            (readout.center().x - screen.0 / 2.0).abs() < 1.0,
            "at {screen:?} the readout {readout:?} is not centred"
        );
        assert!(
            (readout.top() - OVERLAY_MARGIN).abs() < 1.0,
            "at {screen:?} the readout {readout:?} does not keep the top margin"
        );

        // Both corner buttons keep the margin from the right edge and stay
        // on screen, the link under the button.
        for (name, rect) in [("button", button), ("docs", docs)] {
            assert!(
                (screen.0 - rect.right() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the {name} {rect:?} does not keep the right margin"
            );
            assert!(
                rect.bottom() <= screen.1,
                "at {screen:?} the {name} {rect:?} runs off the bottom"
            );
        }
        assert!(
            docs.top() >= button.bottom(),
            "at {screen:?} the docs link {docs:?} is not under the button {button:?}"
        );

        // Wide windows hold the readout and the button on the top row;
        // narrow ones stack the button under the readout.
        if screen.0 >= 800.0 {
            assert!(
                (button.top() - OVERLAY_MARGIN).abs() < 1.0,
                "at {screen:?} the button {button:?} should share the top row"
            );
        } else {
            assert!(
                button.top() >= readout.bottom(),
                "at {screen:?} the button {button:?} should drop under the readout {readout:?}"
            );
        }
    }
}

#[test]
fn the_docs_button_asks_for_the_api_docs() {
    // The docs link goes through egui's own URL command, which the UI
    // source publishes in the world; nothing here opens a page.
    let ctx = egui::Context::default();
    let screen = (800.0, 600.0);
    // The link is only under the button once a pass has settled both.
    for _ in 0..2 {
        let _ = corner_rects(&ctx, screen);
    }
    let (press, release) = tap(area_centre(&ctx, API_DOCS_AREA));

    let mut output = ctx.run_ui(raw_input(screen, press), |ui| {
        api_docs_button(ui);
    });
    assert!(
        output.platform_output.commands.is_empty(),
        "a press alone asks for nothing"
    );
    output.textures_delta.clear();

    let mut output = ctx.run_ui(raw_input(screen, release), |ui| {
        api_docs_button(ui);
    });
    let commands = std::mem::take(&mut output.platform_output.commands);
    output.textures_delta.clear();
    let opened: Vec<(String, bool)> = commands
        .iter()
        .filter_map(|command| match command {
            egui::OutputCommand::OpenUrl(open) => Some((open.url.clone(), open.new_tab)),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened,
        vec![(API_DOCS_URL.to_owned(), false)],
        "the release asks for the API docs in this tab"
    );
}

#[test]
fn the_fullscreen_button_is_at_least_a_touch_target() {
    // A control a phone has to be hit with a finger must not shrink to a
    // row of text: the button is grown to the declared minimum, including
    // the padding that centres the label, on every state and width.
    for screen in [(1280.0, 577.0), (390.0, 844.0)] {
        for active in [false, true] {
            let ctx = egui::Context::default();
            for _ in 0..2 {
                let _ = fullscreen_pass(&ctx, screen, active, Vec::new());
            }
            let button = area_rect(&ctx, FULLSCREEN_AREA);
            assert!(
                button.width() >= OVERLAY_BUTTON_MIN_SIZE.x
                    && button.height() >= OVERLAY_BUTTON_MIN_SIZE.y,
                "at {screen:?} {active} the button {button:?} is below the touch-target size"
            );
        }
    }
}

/// Every line count the readout laid a text galley out with, for a screen
/// of `screen` points showing each of `texts` in turn.
///
/// The readout is anchored, and an anchored area lays its contents out
/// against the size it remembered from the previous pass, so one pass
/// proves nothing: the failure it is prone to only shows once the text
/// outgrows what the last pass left room for. `texts` is therefore a
/// sequence of readings, one per pass, and the first pass — egui's
/// invisible sizing pass — draws no text at all and contributes nothing.
fn readout_lines(screen: (f32, f32), texts: &[&str]) -> Vec<usize> {
    let ctx = egui::Context::default();
    let mut lines = Vec::new();
    for text in texts {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(screen.0, screen.1),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| frame_rate_readout(ui, text));
        // egui panics if a texture delta is dropped unapplied; a real frame
        // hands it to the integration, and this test discards it.
        output.textures_delta.clear();
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                lines.push(text.galley.rows.len());
            }
        }
    }
    lines
}

#[test]
fn the_frame_rate_readout_stays_on_one_line() {
    // The reading is re-laid out every frame, starting from the empty
    // placeholder, and it grows a digit as the average settles: the readout
    // must hold one line through all of it, at any window width.
    for screen in [(1600.0, 900.0), (640.0, 480.0), (320.0, 240.0)] {
        for texts in [
            vec!["60.0 fps · 16.7 ms"; 4],
            vec!["— fps"; 4],
            vec![
                "— fps",
                "60.0 fps · 16.7 ms",
                "60.0 fps · 16.7 ms",
                "100.0 fps · 10.0 ms",
            ],
        ] {
            let lines = readout_lines(screen, &texts);
            assert!(
                !lines.is_empty(),
                "the readout drew nothing at {screen:?} {texts:?}"
            );
            assert_eq!(
                lines,
                vec![1; lines.len()],
                "screen {screen:?} text {texts:?}"
            );
        }
    }
}

#[test]
fn the_frame_rate_reads_nothing_until_a_frame_was_measured() {
    let rate = FrameRate::default();
    assert_eq!(rate.mean_seconds(), None);
    assert_eq!(rate.text(), "— fps");
}

#[test]
fn the_frame_rate_averages_its_window_and_reports_both_units() {
    let mut rate = FrameRate::default();
    // A steady 16 ms a frame is a little over 60 fps.
    for _ in 0..FrameRate::WINDOW {
        rate.push(0.016);
    }
    let mean = rate.mean_seconds().expect("a measured frame");
    assert!((mean - 0.016).abs() < 1e-6);
    assert_eq!(rate.text(), "62.5 fps · 16.0 ms");
}

#[test]
fn the_frame_rate_window_slides_rather_than_averaging_the_session() {
    let mut rate = FrameRate::default();
    // A long slow stretch, then enough fast frames to fill the window
    // again: the reading has to follow, or a scene that stalled once would
    // read slow for the rest of the session.
    for _ in 0..FrameRate::WINDOW * 2 {
        rate.push(0.1);
    }
    for _ in 0..FrameRate::WINDOW {
        rate.push(0.01);
    }
    assert_eq!(rate.samples.len(), FrameRate::WINDOW);
    let mean = rate.mean_seconds().expect("a measured frame");
    assert!((mean - 0.01).abs() < 1e-6, "got {mean}");
}
