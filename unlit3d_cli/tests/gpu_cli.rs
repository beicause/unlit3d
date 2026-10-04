//! End-to-end tests: render the reference glTF scene through the library and
//! compare the frames against the repository snapshots.

use unlit_wgpu_test_util::{
    Tolerance, assert_image_snapshot_with_tolerance, gpu_test_main, gpu_tests, snapshots,
};
use unlit3d_cli::{
    AnimationRef, CameraConfig, Config, DocumentConfig, Frame, OutputConfig, PlacementConfig,
};

const FOX: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../unlit3d_asset_files/assets/Fox.glb"
);
const MORPH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../unlit3d_asset_files/assets/MorphStressTest.glb"
);

/// How long the named animation of a document lasts, in seconds.
fn duration(path: &str, name: &str) -> f32 {
    let document = unlit3d::gltf::UnlitGltf::load(path).expect("the document loads");
    let index = (0..document.animation_count())
        .find(|&animation| document.animation_name(animation) == Some(name))
        .expect("the animation exists");
    document.animation_duration(index)
}

/// The tolerance the glTF frames are compared with, matching the example's
/// GLTF_TOLERANCE.
const TOLERANCE: Tolerance = Tolerance {
    min_score: Some(75.0),
    max_outliers: Some(0.005),
    channel_delta: 8,
};

/// The reference scene of unlit3d_examples/src/scenes/gltf.rs, at one phase.
fn reference(fox_time: f32, morph_time: f32, size: (u32, u32), scale: (f32, f32)) -> Config {
    Config {
        output: OutputConfig {
            size: Some(size),
            render_size: Some((256, 192)),
            scale,
            samples: 1,
            depth: true,
            clear: [0.0, 0.0, 0.0, 1.0],
        },
        camera: CameraConfig {
            eye: [160.5, 109.0, 49.5],
            target: [4.5, 38.0, 26.5],
            up: [0.0, 1.0, 0.0],
            fov_y: 60.0,
            z_near: 0.1,
            matrix: None,
        },
        documents: vec![
            DocumentConfig {
                path: FOX.to_owned(),
                placement: Some(PlacementConfig {
                    translation: [-4.54, 0.0, -1.01],
                    rotation: [0.0, -0.216_439_62, 0.0, 0.976_296],
                    scale: [1.0, 1.0, 1.0],
                }),
                animation: Some(AnimationRef::Name("Walk".to_owned())),
                time: fox_time,
            },
            DocumentConfig {
                path: MORPH.to_owned(),
                placement: Some(PlacementConfig {
                    translation: [20.0, 0.0, 90.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [20.0, 20.0, 20.0],
                }),
                animation: Some(AnimationRef::Name("Pulse".to_owned())),
                time: morph_time,
            },
        ],
    }
}

async fn renders_the_reference_gltf_scene() {
    let frames = snapshots![
        "gltf/frame_00.webp",
        "gltf/frame_01.webp",
        "gltf/frame_02.webp",
        "gltf/frame_03.webp",
        "gltf/frame_04.webp",
        "gltf/frame_05.webp"
    ];
    let fox_duration = duration(FOX, "Walk");
    let morph_duration = duration(MORPH, "Pulse");
    for (index, snapshot) in frames.iter().enumerate() {
        let phase = index as f32 / frames.len() as f32;
        let config = reference(
            fox_duration * phase,
            morph_duration * phase,
            (256, 192),
            (1.0, 1.0),
        );
        let frame = unlit3d_cli::render(&config)
            .await
            .expect("the reference scene renders");
        assert_image_snapshot_with_tolerance(
            *snapshot,
            &frame.rgba,
            frame.width,
            frame.height,
            TOLERANCE,
        );
    }
}

async fn renders_a_narrow_render_size() {
    // A 192x288 output with a 256x192 render size scaled by 288/192 on both
    // axes: the viewport is 384x288, filling the height and cropping the sides.
    let config = reference(0.0, 0.0, (192, 288), (288.0 / 192.0, 288.0 / 192.0));
    let frame = unlit3d_cli::render(&config)
        .await
        .expect("the narrow scene renders");
    assert_image_snapshot_with_tolerance(
        snapshots!["gltf/narrow.webp"][0],
        &frame.rgba,
        frame.width,
        frame.height,
        TOLERANCE,
    );
}

async fn saves_by_extension() {
    let frame = Frame {
        rgba: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
        width: 2,
        height: 2,
    };
    let directory = std::env::temp_dir().join("unlit3d_cli_saves_by_extension");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("the temporary directory is created");
    for extension in ["png", "webp", "jpeg"] {
        let path = directory.join(format!("frame.{extension}"));
        unlit3d_cli::save(&path, &frame).expect("the frame is saved");
        let decoded = image::open(&path).expect("the saved image decodes");
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
    }
    let _ = std::fs::remove_dir_all(&directory);
}

gpu_tests! {
    renders_the_reference_gltf_scene,
    renders_a_narrow_render_size,
    saves_by_extension,
}

gpu_test_main!(all_tests());
