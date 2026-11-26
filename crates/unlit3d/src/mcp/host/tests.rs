use super::*;

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

#[test]
fn a_document_that_starts_with_a_mark_is_read_as_json() {
    let document = "\u{feff}{\"asset\":{\"version\":\"2.0\"}}";
    let bytes = gltf_document_bytes(document).expect("the mark is not part of the text");
    assert_eq!(bytes, br#"{"asset":{"version":"2.0"}}"#);
}

#[test]
fn a_document_without_a_brace_is_read_as_base64() {
    let bytes = gltf_document_bytes("Z2xURg==").expect("the text is base64");
    assert_eq!(bytes, b"glTF");
    let error = gltf_document_bytes("not base64!").expect_err("the text names no alphabet");
    assert_eq!(error.kind(), unlit_ecs::mcp::ToolErrorKind::Failed);
}

#[test]
fn a_failure_to_load_a_document_keeps_its_own_structure() {
    let mut world = World::new();
    let error = spawn_gltf(
        &mut world,
        None,
        None,
        None,
        Some(r#"{"asset":{"version":"2.0"},"meshes":[{"primitives":[{"attributes":{}}]}]}"#),
    )
    .expect_err("the primitive states no positions");
    let details = error
        .details()
        .unwrap_or_else(|| panic!("the failure names what it was: {error}"));
    assert_eq!(
        details["gltf"]["kind"], "Parse",
        "the structured content names the check: {details}"
    );
    assert!(
        details["gltf"]["message"].is_string(),
        "a parse failure carries the glTF crate's own report: {details}"
    );
}

#[test]
fn the_frame_clock_averages_the_frames_after_its_first_call() {
    let mut world = World::new();
    world.root_scope_mut().spawn(bundle![FrameClock::default()]);
    for index in 0..3u32 {
        let mut frame = crate::frame::Frame {
            delta_time: 1.0 / 60.0,
            index,
        };
        dispatch_frame(&mut world, &mut frame);
    }

    // The first call spawns the monitor, which has seen no frame yet.
    let clock = frame_clock(&mut world).expect("the clock reports");
    assert_eq!(clock.index, Some(2));
    assert_eq!(clock.average_fps, None);

    // The frames after it fold into the rolling window.
    for index in 3..5u32 {
        let mut frame = crate::frame::Frame {
            delta_time: 1.0 / 30.0,
            index,
        };
        dispatch_frame(&mut world, &mut frame);
    }
    let clock = frame_clock(&mut world).expect("the clock reports");
    assert_eq!(clock.index, Some(4));
    assert_eq!(clock.average_window_frames, Some(2));
    let fps = clock.fps.expect("the last frame states a rate");
    assert!(
        (fps - 30.0).abs() < 0.1,
        "the instantaneous rate is the last frame's: {clock:?}"
    );
    let average = clock.average_fps.expect("an average exists");
    assert!(
        (average - 30.0).abs() < 0.1,
        "the window is the two 30 Hz frames: {clock:?}"
    );
}

#[test]
fn a_world_that_never_dispatched_a_frame_reports_none() {
    let mut world = World::new();
    let clock = frame_clock(&mut world).expect("the clock reports");
    assert_eq!(clock.index, None);
    assert_eq!(clock.delta_time, None);
    assert_eq!(clock.time, None);
    assert_eq!(clock.fps, None);
    assert_eq!(clock.average_fps, None);

    // A world without a clock gains none behind the client's back, so a
    // frame dispatched into it still reads as no frame rather than a zero
    // the client would take for a real first frame.
    let mut frame = crate::frame::Frame {
        delta_time: 1.0 / 60.0,
        index: 7,
    };
    dispatch_frame(&mut world, &mut frame);
    let clock = frame_clock(&mut world).expect("the clock reports");
    assert_eq!(clock.index, None);
}
