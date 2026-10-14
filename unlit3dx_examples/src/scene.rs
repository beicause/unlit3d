//! The demo's scene: its constants, meshes, camera, and the gameplay
//! that drives the worlds.
//!
//! The windowed binary and the headless run both build on this module:
//! everything here is world-shaped, free of window and thread plumbing,
//! so the same gameplay logic drives the windowed show and the
//! deterministic headless capture.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use glam::Vec3;
use hashbrown::HashMap;
use unlit_wgpu::resources::ResourceGraph;
use unlit3d::prelude::*;
use unlit3d::ui::{UiPanel, UiSource, egui};
use unlit3d::winit::event::OnAboutToWait;
use unlit3dx::prelude::*;

/// The soundfont the demo plays, embedded at build time by `build.rs`.
///
/// When the soundfont file is missing the build writes an empty stub, so
/// the demo starts and runs silent rather than failing to compile.
pub const SOUNDFONT: &[u8] = include_bytes!(env!("SOUNDFONT_FILE"));

/// The window opens at this size, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (1024, 720);

/// The render sub-world's command queue depth. One scene build queues
/// seven asset requests and forty-nine instance requests; the capacity
/// keeps all of them in flight at once.
pub const RENDER_COMMAND_CAPACITY: usize = 64;

/// How many balls the scene keeps around. The pool never grows: gameplay
/// requests the instances from the render world once, and a spawned ball
/// reuses the first free entity.
pub const POOL_SIZE: usize = 48;

/// Every ball's radius, in metres.
pub const BALL_RADIUS: f32 = 0.8;

/// Where balls spawn, in metres above the ground.
pub const SPAWN_HEIGHT: f32 = 7.0;

/// How far from the centre a ball may spawn, in metres.
pub const SPAWN_SPREAD: f32 = 3.0;

/// The ground box's half extents, in metres.
pub const GROUND_HALF: Vec3 = Vec3::new(12.0, 0.5, 12.0);

/// Where the ground box sits; its top face is the y = 0 plane.
pub const GROUND_POSITION: Vec3 = Vec3::new(0.0, -0.5, 0.0);

/// Where idle pool entities wait, far below everything the camera sees.
pub const HIDDEN_POSITION: Vec3 = Vec3::new(0.0, -1000.0, 0.0);

/// The factor every synthesizer sample scales by, making up the headroom
/// soundfont presets leave.
pub const SYNTH_GAIN: f32 = 2.0;

/// The track name the looping background music plays on.
pub const BGM_TRACK: &str = "background";

/// The track name the demo's MIDI sequence plays on.
pub const SEQUENCE_TRACK: &str = "sequence";

/// The General MIDI music box, as a zero-based program number: a bell
/// like plucked timbre that stays smooth where a piano turns brash.
pub const MUSIC_BOX_PROGRAM: u8 = 10;

/// The General MIDI electric piano, as a zero-based program number: soft
/// enough to sit under the sequence without competing with it.
pub const ELECTRIC_PIANO_PROGRAM: u8 = 4;

/// The pentatonic scale the sequence walks, as MIDI keys.
///
/// The octave sits low on purpose: the soundfont's higher keys turn
/// brittle, and a lower register stays mellow.
pub const SCALE: [i32; 10] = [48, 50, 52, 55, 57, 60, 62, 64, 67, 69];

/// Push one note onto an event list: a press at `start`, a release
/// `hold` seconds later, both on `channel`.
fn note(events: &mut Vec<MidiEvent>, channel: u8, key: i32, start: f64, hold: f64, velocity: u8) {
    events.push((start, 0x90 | channel, key as u8, velocity));
    events.push((start + hold, 0x80 | channel, key as u8, 0));
}

/// The demo's MIDI sequence: the scale walked up and back down, one note
/// every 0.28 seconds, each held for 0.25 of them, on the music box.
pub fn sequence_midi() -> Arc<MidiFile> {
    let mut events = vec![(0.0, 0xC0, MUSIC_BOX_PROGRAM, 0u8)];
    for (index, &key) in SCALE.iter().chain(SCALE.iter().rev().skip(1)).enumerate() {
        let start = index as f64 * 0.28;
        note(&mut events, 0, key, start, 0.25, 96);
    }
    events.sort_by(|(one, ..), (other, ..)| one.total_cmp(other));
    midi_file(&events).expect("the demo sequence is well formed")
}

/// The bars of the background loop, as chords: three keys each, root
/// first. Four bars of a I-vi-IV-V turnround, twice, so the loop breathes
/// without changing key.
const BGM_CHORDS: [[i32; 3]; 8] = [
    [48, 55, 60],
    [45, 52, 57],
    [41, 53, 57],
    [43, 50, 59],
    [48, 55, 60],
    [45, 52, 57],
    [41, 53, 60],
    [43, 55, 62],
];

/// The melody over the loop, as scale degrees: an index into [`SCALE`],
/// or `None` for a rest. One entry per half bar.
const BGM_MELODY: [Option<usize>; 16] = [
    Some(5),
    None,
    Some(4),
    Some(3),
    Some(2),
    None,
    Some(3),
    Some(4),
    Some(5),
    None,
    Some(7),
    Some(6),
    Some(4),
    None,
    Some(2),
    Some(3),
];

/// How long one bar of the background loop lasts, in seconds.
const BGM_BAR_SECONDS: f64 = 2.0;

/// Which chord key sounds on each of a bar's eight eighths: up the chord
/// and back down again, so the accompaniment moves with the balls.
const BGM_ARPEGGIO: [usize; 8] = [0, 1, 2, 1, 0, 1, 2, 1];

/// The looping background music: a bass note per bar, the chord's keys
/// arpeggiated across its eight eighths, and a slow melody on top, all on
/// the electric piano. Written out as events rather than read from a file:
/// the demo carries no MIDI asset, and the loop is short enough to state.
pub fn bgm_midi() -> Arc<MidiFile> {
    let eighth = BGM_BAR_SECONDS / 8.0;
    // Three channels, one instrument each: the bass, the arpeggio and the
    // melody, so the loop reads as three parts instead of one chord wall.
    let mut events = vec![
        (0.0, 0xC0, ELECTRIC_PIANO_PROGRAM, 0u8),
        (0.0, 0xC1, ELECTRIC_PIANO_PROGRAM, 0u8),
        (0.0, 0xC2, MUSIC_BOX_PROGRAM, 0u8),
    ];
    for (bar, chord) in BGM_CHORDS.iter().enumerate() {
        let start = bar as f64 * BGM_BAR_SECONDS;
        // The bass holds the bar: an octave below its root.
        note(&mut events, 0, chord[0] - 12, start, BGM_BAR_SECONDS, 56);
        // The chord walks up and back over the bar's eighths, so the
        // accompaniment moves without ever leaving the harmony.
        for (step, &finger) in BGM_ARPEGGIO.iter().enumerate() {
            let key = chord[finger] + 12;
            note(
                &mut events,
                1,
                key,
                start + step as f64 * eighth,
                eighth,
                44,
            );
        }
        // The melody: one note per half bar, resting where the chart says.
        for half in 0..2 {
            let Some(degree) = BGM_MELODY[bar * 2 + half] else {
                continue;
            };
            note(
                &mut events,
                2,
                SCALE[degree] + 12,
                start + half as f64 * (BGM_BAR_SECONDS / 2.0),
                BGM_BAR_SECONDS / 2.0 * 0.9,
                72,
            );
        }
    }
    events.sort_by(|(one, ..), (other, ..)| one.total_cmp(other));
    midi_file(&events).expect("the background loop is well formed")
}

/// The gameplay world's tick rate, in hertz.
pub const GAME_HZ: f64 = 120.0;

/// The render pace, in hertz: the windowed mode draws as fast as the
/// compositor asks, but the headless run steps its frames at this rate,
/// and the gameplay and physics rates keep their multiples of it.
pub const RENDER_HZ: f64 = 60.0;

/// How many gameplay ticks pass per render frame; the gameplay rate is a
/// multiple of the render rate, so both stay in step.
pub const GAMEPLAY_TICKS_PER_FRAME: u32 = (GAME_HZ / RENDER_HZ) as u32;

/// How many gameplay ticks pass between automatic ball drops.
pub const SPAWN_TICKS: u32 = 84;

/// How many gameplay ticks a spawn request may stay unanswered before its
/// render entity returns to the pool.
pub const PENDING_TIMEOUT_TICKS: u32 = 600;

/// The base colors the balls cycle through, as sRGB bytes.
pub const BALL_COLORS: [[u8; 4]; 4] = [
    [228, 92, 74, 255],
    [96, 178, 92, 255],
    [86, 132, 228, 255],
    [232, 192, 76, 255],
];

/// The ground's base color, as sRGB bytes.
pub const GROUND_COLOR: [u8; 4] = [126, 130, 138, 255];

/// The checker's darker square, as sRGB bytes.
pub const GROUND_CHECKER_DARK: [u8; 4] = [88, 92, 100, 255];

/// The ground texture's edge length in pixels; the checker draws eight
/// squares per side, so each square is a block of
/// `GROUND_CHECKER_SIZE / 8` pixels.
pub const GROUND_CHECKER_SIZE: u32 = 256;

/// The checker squares per texture side.
const GROUND_CHECKER_SQUARES: u32 = 8;

/// The ground's checker texture: `GROUND_CHECKER_SQUARES` squares per
/// side, the light ones in [`GROUND_COLOR`] and the dark ones in
/// [`GROUND_CHECKER_DARK`].
#[must_use]
pub fn checker_ground() -> Vec<[u8; 4]> {
    let block = GROUND_CHECKER_SIZE / GROUND_CHECKER_SQUARES;
    let mut pixels = Vec::with_capacity((GROUND_CHECKER_SIZE * GROUND_CHECKER_SIZE) as usize);
    for y in 0..GROUND_CHECKER_SIZE {
        for x in 0..GROUND_CHECKER_SIZE {
            let square = (x / block + y / block).is_multiple_of(2);
            pixels.push(if square {
                GROUND_COLOR
            } else {
                GROUND_CHECKER_DARK
            });
        }
    }
    pixels
}

/// The pattern textures the shape materials draw from, in pixels per
/// side: big enough that a pattern reads across a metre-wide body,
/// small enough that sixteen of them stay cheap.
pub const PATTERN_SIZE: u32 = 128;

/// One body shape of the pool: the mesh it draws, the collider physics
/// simulates, and the texture pattern its material wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    /// A sphere: a dotted texture over the round mesh.
    Ball,
    /// A box: a checker across the six faces.
    Cube,
    /// A cylinder along y: vertical stripes wrap the wall.
    Cylinder,
    /// A cone pointing up y: horizontal rings stack down the wall.
    Cone,
}

/// Every shape the pool cycles through, in spawn order.
pub const SHAPE_KINDS: [ShapeKind; 4] = [
    ShapeKind::Ball,
    ShapeKind::Cube,
    ShapeKind::Cylinder,
    ShapeKind::Cone,
];

/// The shape kind a pool slot of the given index draws.
pub fn shape_kind(index: usize) -> ShapeKind {
    SHAPE_KINDS[index % SHAPE_KINDS.len()]
}

/// Scale a color's bytes by a factor, for the pattern accents.
fn tinted(color: [u8; 4], factor: f32) -> [u8; 4] {
    [
        (f32::from(color[0]) * factor).round().clamp(0.0, 255.0) as u8,
        (f32::from(color[1]) * factor).round().clamp(0.0, 255.0) as u8,
        (f32::from(color[2]) * factor).round().clamp(0.0, 255.0) as u8,
        color[3],
    ]
}

/// The texture pattern one shape kind wears, tinted with a base color:
/// dots for the ball, a checker for the cube, stripes for the cylinder
/// and rings for the cone. Every pattern puts the accents where the
/// body's silhouette bends, so the tumbling reads as motion.
#[must_use]
pub fn pattern_texture(kind: ShapeKind, color: [u8; 4]) -> Vec<[u8; 4]> {
    let accent = tinted(color, 0.72);
    let size = PATTERN_SIZE;
    let block = match kind {
        // The checker blocks: four per side.
        ShapeKind::Cube => size / 4,
        // The stripe width: eight stripes wrap the wall.
        ShapeKind::Cylinder | ShapeKind::Cone => size / 8,
        // The dot pitch: a six by six grid of dots.
        ShapeKind::Ball => size / 6,
    };
    let mut pixels = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        for x in 0..size {
            let on = match kind {
                // The cube's checker: blocks alternate both ways.
                ShapeKind::Cube => (x / block + y / block).is_multiple_of(2),
                // The cylinder's stripes: vertical, so they wrap the
                // wall and read as rotation while the body rolls.
                ShapeKind::Cylinder => (x / block).is_multiple_of(2),
                // The cone's rings: horizontal, so they stack down the
                // wall and read as height while the body rolls.
                ShapeKind::Cone => (y / block).is_multiple_of(2),
                ShapeKind::Ball => {
                    // A dot at every block corner, a quarter block
                    // across, so the sphere wears a polka grid.
                    let cx = x % block;
                    let cy = y % block;
                    let half = block / 2;
                    let dx = cx.abs_diff(half);
                    let dy = cy.abs_diff(half);
                    dx * dx + dy * dy < half * half / 2
                }
            };
            pixels.push(if on { accent } else { color });
        }
    }
    pixels
}

/// The mesh one shape kind draws. Every kind is about [`BALL_RADIUS`]
/// across, so the pool shares one visual weight.
#[must_use]
pub fn shape_mesh(kind: ShapeKind) -> OwnedUnlitMeshDesc {
    match kind {
        ShapeKind::Ball => uv_sphere(BALL_RADIUS, 32, 22),
        ShapeKind::Cube => cuboid_mesh(Vec3::splat(BALL_RADIUS * 0.8)),
        ShapeKind::Cylinder => into_desc(uv_cylinder(BALL_RADIUS * 0.72, BALL_RADIUS, 24)),
        ShapeKind::Cone => into_desc(uv_cone(BALL_RADIUS * 0.9, BALL_RADIUS, 24)),
    }
}

/// Lift a raw mesh's arrays into the owned description the render server
/// takes.
fn into_desc(mesh: RawMesh) -> OwnedUnlitMeshDesc {
    let (positions, uvs, colors, indices) = mesh;
    OwnedUnlitMeshDesc {
        positions,
        uvs: Some(uvs),
        colors: Some(colors),
        indices: Some(indices),
    }
}

/// The collider physics simulates for one shape kind, sized to match
/// its mesh.
#[must_use]
pub fn shape_body(kind: ShapeKind) -> BodyShape {
    match kind {
        ShapeKind::Ball => BodyShape::Ball(BALL_RADIUS),
        ShapeKind::Cube => BodyShape::Cuboid(Vec3::splat(BALL_RADIUS * 0.8)),
        ShapeKind::Cylinder => BodyShape::Cylinder {
            half_height: BALL_RADIUS,
            radius: BALL_RADIUS * 0.72,
        },
        ShapeKind::Cone => BodyShape::Cone {
            half_height: BALL_RADIUS,
            radius: BALL_RADIUS * 0.9,
        },
    }
}

/// The host frame's behaviour order: the host's own input dispatch runs
/// after the setup and before the draw, whose UI source reads the same
/// events.
pub const FRAME_DISPATCH_INPUT: i32 = 5;

/// The host frame's behaviour order: the inline worlds step here, after
/// the input dispatch and before the draw, so the transforms they publish
/// are the ones the frame draws.
pub const FRAME_INLINE_STEP: i32 = 6;

/// The host frame's behaviour order: the frame's events are forgotten
/// after the draw, once every consumer has read them.
pub const FRAME_CLEAR_INPUT: i32 = 20;

/// The render world's behaviour order: the camera orbit runs last.
pub const RENDER_CAMERA: i32 = 100;

/// How fast the camera circles the scene, in radians per second.
pub const ORBIT_SPEED: f32 = 0.25;

/// The camera's orbit radius, in metres.
pub const ORBIT_RADIUS: f32 = 16.0;

/// The camera's orbit height, in metres.
pub const ORBIT_HEIGHT: f32 = 9.0;

/// The point the camera looks at.
pub const ORBIT_TARGET: Vec3 = Vec3::new(0.0, 0.6, 0.0);

/// Where the camera's orbit starts, in radians.
pub const ORBIT_START: f32 = 0.65;

/// The handles the control panel uses to reach the other worlds: the
/// gameplay command sender for the shapes, and — where there is a
/// synthesizer to drive — the audio command sender for the two music
/// tracks.
#[derive(Clone)]
pub struct ControlHooks {
    /// The gameplay world's command sender.
    pub gameplay: JobSender,
    /// The audio world's command sender, absent when the audio world has
    /// no synthesizer and so no tracks to transport.
    pub audio: Option<JobSender>,
}

/// One track's transport: its name, then a pause toggle and a rewind.
///
/// The row names the track so the two buttons need no suffix of their
/// own, and the toggle's label names what pressing it does next.
fn track_controls(ui: &mut egui::Ui, audio: &JobSender, name: &str, paused: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(match name {
            SEQUENCE_TRACK => "MIDI sequence",
            _ => "Background music",
        });
        let toggle = if *paused { "Resume" } else { "Pause" };
        if ui.button(toggle).clicked() {
            *paused = !*paused;
            audio.send(pause_track_job(name, *paused)).ok();
        }
        if ui.button("Rewind").clicked() {
            audio.send(rewind_track_job(name)).ok();
        }
    });
}

/// Mount the demo's control panel into the render sub-world.
///
/// The panel rides on the input handle the host distributes into every
/// nested world, so nothing input-related is spawned here: the UI source
/// reads that handle's events each frame, and the host clears them once
/// the frame is drawn.
///
/// Both tracks start on their own once the audio world can sound, so the
/// panel only ever changes what is already playing: each track pauses,
/// resumes and rewinds on its own, and the shapes drop or clear without
/// touching the music. A world with no synthesizer has nothing to
/// transport, so its panel shows the shapes alone.
pub fn mount_control_panel(world: &mut World, hooks: ControlHooks) {
    world.spawn_source(UiSource::new());
    let gameplay = hooks.gameplay;
    let audio = hooks.audio;
    let mut bgm_paused = false;
    let mut sequence_paused = false;
    world.spawn((UiPanel::new(move |_world, _entity, ui| {
        egui::Window::new("Controls").show(ui.ctx(), |ui| {
            if let Some(audio) = &audio {
                track_controls(ui, audio, BGM_TRACK, &mut bgm_paused);
                track_controls(ui, audio, SEQUENCE_TRACK, &mut sequence_paused);
                ui.separator();
            }
            ui.horizontal(|ui| {
                if ui.button("Drop the next shape").clicked() {
                    request_drop(&gameplay);
                }
                if ui.button("Clear the shapes").clicked() {
                    request_clear(&gameplay);
                }
            });
        });
    }),));
}

/// Set up the render sub-world's content: the resource graph, the camera
/// and its orbit, the asset-server source, and the transform mailbox
/// reader. Answers the renderer resource entity.
///
/// The world's meshes, materials and instances are not built here: they
/// arrive as jobs from gameplay, through the sub-world's drain in the
/// draw behaviour.
pub fn setup_render_world(
    host: &mut World,
    render_world: Entity,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    capabilities: DeviceCapabilities,
    frame_reader: SnapshotReader<TransformFrame>,
    controls: Option<ControlHooks>,
) -> Entity {
    let Some(mut nested) = host.get_mut::<World>(render_world) else {
        unreachable!("the sub-world is a child of the host world");
    };
    let context = spawn_context(
        &mut nested,
        device.clone(),
        queue.clone(),
        ResourceGraph::new(),
        capabilities,
    );
    let renderer = nested.spawn((Renderer::new(context),));
    let mut source = MeshSource::new(&nested, context);
    source.register_unlit_family(&nested);
    nested.spawn_source(source);

    // The camera orbits the scene; its behaviour keeps the projection
    // matched to the frame's own aspect. The azimuth advances with the
    // frame index, so the orbit speed follows the redraw rate.
    let mut azimuth = ORBIT_START;
    let camera_entity = nested.spawn((camera_looking_at(
        WINDOW_SIZE,
        orbit_eye(ORBIT_START),
        ORBIT_TARGET,
        60f32.to_radians(),
        0.1,
    ),));
    nested.spawn((OnFrame::new(
        FrameBehaviourOrder(RENDER_CAMERA),
        move |world, _entity, frame| {
            // The orbit advances by each frame's own delta, so the speed
            // does not follow the redraw rate.
            azimuth += ORBIT_SPEED * frame.delta_time;
            let camera = camera_looking_at(
                frame.size,
                orbit_eye(azimuth),
                ORBIT_TARGET,
                60f32.to_radians(),
                0.1,
            );
            let _ = world.with_mut::<Camera, _>(camera_entity, |current| {
                *current = camera;
            });
        },
    ),));

    // Physics snapshots become render transforms here, every frame,
    // before anything draws.
    let _ = nested.spawn_frame_behaviour(apply_transform_frames(frame_reader));
    if let Some(controls) = controls {
        mount_control_panel(&mut nested, controls);
    }
    renderer
}

/// The point on the orbit the camera sits at for one azimuth.
fn orbit_eye(azimuth: f32) -> Vec3 {
    Vec3::new(
        ORBIT_TARGET.x + azimuth.cos() * ORBIT_RADIUS,
        ORBIT_HEIGHT,
        ORBIT_TARGET.z + azimuth.sin() * ORBIT_RADIUS,
    )
}

/// A mesh as raw arrays: positions, uvs, colors, indices.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// The opaque white every mesh vertex carries; materials tint via
/// textures.
pub const WHITE_COLOR: [u8; 4] = [255, 255, 255, 255];

/// A lathe along the y axis: the side wall walks the rings top to
/// bottom, and the caps close whichever ends ask for it, so a cylinder
/// and a cone share one winding story.
fn lathe(rings: &[(f32, f32); 2], caps: (bool, bool), slices: usize) -> RawMesh {
    let rows = rings.len();
    let ring_len = slices + 1;
    let vertices = ring_len * rows + ring_len * (usize::from(caps.0) + usize::from(caps.1));
    let mut positions = Vec::with_capacity(vertices);
    let mut uvs = Vec::with_capacity(vertices);
    let mut colors = Vec::with_capacity(vertices);
    let mut indices = Vec::with_capacity(
        (rows - 1) * slices * 6 + slices * 3 * (usize::from(caps.0) + usize::from(caps.1)),
    );
    // The side wall: the same outward winding the sphere's quads read,
    // (top, top-next, bottom) and (top-next, bottom-next, bottom)
    // against the outward direction.
    for (row, &(radius, y)) in rings.iter().enumerate() {
        let v = row as f32 / (rows - 1).max(1) as f32;
        for slice in 0..=slices {
            let u = slice as f32 / slices as f32;
            let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
            positions.push([radius * cos, y, radius * sin]);
            uvs.push([u, v]);
            colors.push(WHITE_COLOR);
        }
    }
    for row in 0..rows - 1 {
        for slice in 0..slices {
            let top = (row * ring_len + slice) as u32;
            let bottom = top + ring_len as u32;
            indices.extend_from_slice(&[top, top + 1, bottom, top + 1, bottom + 1, bottom]);
        }
    }
    // A cap fans its ring around a centre point. The ring runs with
    // growing angle, which reads clockwise from above, so the top cap
    // (facing +y) walks its fan backwards and the bottom cap forwards.
    let cap = |lathe: &mut RawMesh, y: f32, up: bool| {
        let base = lathe.0.len() as u32;
        let centre = base;
        lathe.0.push([0.0, y, 0.0]);
        lathe.1.push([0.5, 0.5]);
        lathe.2.push(WHITE_COLOR);
        let &(radius, ring_y) = if up {
            rings.first().expect("a top ring")
        } else {
            rings.last().expect("a bottom ring")
        };
        for slice in 0..=slices {
            let u = slice as f32 / slices as f32;
            let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
            lathe.0.push([radius * cos, ring_y, radius * sin]);
            lathe.1.push([0.5 + 0.5 * cos, 0.5 + 0.5 * sin]);
            lathe.2.push(WHITE_COLOR);
        }
        for slice in 0..slices {
            let one = centre + 1 + slice as u32;
            let two = one + 1;
            let fan = if up {
                [centre, two, one]
            } else {
                [centre, one, two]
            };
            lathe.3.extend_from_slice(&fan);
        }
    };
    let mut lathe = (positions, uvs, colors, indices);
    if caps.0 {
        cap(&mut lathe, rings[0].1, true);
    }
    if caps.1 {
        cap(&mut lathe, rings[rows - 1].1, false);
    }
    lathe
}

/// A cylinder along the y axis: flat caps top and bottom, uvs mapped so
/// a texture wraps the wall and spots the caps.
pub fn uv_cylinder(radius: f32, half_height: f32, slices: usize) -> RawMesh {
    lathe(
        &[(radius, half_height), (radius, -half_height)],
        (true, true),
        slices,
    )
}

/// A cone pointing up the y axis: an apex ring collapsed to a point on
/// top, a flat base cap below.
pub fn uv_cone(radius: f32, half_height: f32, slices: usize) -> RawMesh {
    lathe(
        &[(0.0, half_height), (radius, -half_height)],
        (false, true),
        slices,
    )
}

/// The async scene build's outcome, filled by the build future and
/// consumed by the first tick after it lands.
enum BuildState {
    /// The build future is still in flight.
    Building,
    /// Every instance answered; the shape pool is live.
    Ready(Vec<(u64, ShapeKind)>),
    /// The render world dropped a request; the scene stays empty.
    Failed,
}

/// One body request in flight: the request itself, the pool slot its
/// render entity was reserved for, the tick the request was made, and
/// the scene generation it belongs to.
type PendingBody = (Request<RigidBodyHandle>, (u64, ShapeKind), u32, u32);

/// The gameplay state, living on its own entity in the gameplay world.
///
/// Gameplay is authoritative and the render world's client: it owns which
/// render entity maps to which body, requests bodies from the physics
/// world, builds the scene by requesting assets and instances from the
/// render server, and publishes transforms. It also starts the demo's two
/// music tracks once the audio world can sound; the panel drives them
/// afterwards without going through gameplay at all.
pub struct Gameplay {
    /// The mailbox gameplay publishes the per-frame transforms into.
    writer: SnapshotWriter<TransformFrame>,
    /// The physics world's latest snapshot.
    physics: SnapshotReader<PhysicsSnapshot>,
    /// The physics world's command channel.
    physics_sender: JobSender,
    /// The audio world's command channel.
    audio_sender: JobSender,
    /// The audio world's readiness mailbox: the music starts only once the
    /// world says it can sound, because a track job that reaches the world
    /// before its players exist finds nobody to address.
    audio_inbox: SnapshotReader<AudioReady>,
    /// Whether the two tracks have been started.
    music_started: bool,
    /// The executor driving the async scene build.
    builder: LocalPool,
    /// The build future's landing slot: it fills this, the tick reads it.
    build: Rc<RefCell<BuildState>>,
    /// Whether the build landed and the pool is live.
    ready: bool,
    /// Whether the build future still needs polling.
    build_pending: bool,
    /// Every render entity the pool owns, for a scene restart: the free
    /// slots are [`Gameplay::pool`], this list is all of them.
    slots: Vec<(u64, ShapeKind)>,
    /// Render entities not yet assigned to a body, as entity bits.
    pool: Vec<(u64, ShapeKind)>,
    /// Body handle to render entity bits, for bodies in the snapshot.
    by_handle: HashMap<RigidBodyHandle, u64>,
    /// Body requests in flight, with the entity bits reserved, the tick
    /// the request was made, and the generation it belongs to: a request
    /// from before a restart is despawned instead of mapped when it
    /// answers.
    ///
    /// These requests are polled from the tick rather than awaited in a
    /// future, unlike the scene build: answering one mutates gameplay
    /// state (the handle map, the pool, the rng), and a future cannot
    /// hold `&mut Gameplay` across an await; a clear cancels in-flight
    /// requests by generation, which a pool of spawned futures could
    /// only do with abort handles it does not have; and the timeout
    /// below counts ticks, not wall time, which keeps the headless
    /// snapshot deterministic regardless of thread timing.
    pending: Vec<PendingBody>,
    /// A clear requested but not yet consumed by the tick.
    clear: bool,
    /// Bumped on every clear, so stale requests are told apart.
    generation: u32,
    /// The snapshot the previous tick published, to skip repeated work.
    last_snapshot: Option<Arc<PhysicsSnapshot>>,
    /// Ball spawns requested but not yet consumed by the tick.
    requests: u32,
    /// Ticks since the gameplay world started, driving the auto spawn.
    ticks: u32,
    /// Transform entries the last publish carried, for the status line.
    transforms_published: usize,
    /// The xorshift state for spawn placement.
    rng: u64,
}

impl Gameplay {
    /// Build the state from the proxies and mailboxes the host wired up.
    ///
    /// A scene asks for this through [`SceneWiring::gameplay`], which
    /// holds the channels. The scene is built by an async future on the
    /// local pool: it awaits the meshes and materials, then the instances
    /// that use them, and leaves the pool in [`BuildState`] for the tick
    /// to pick up. The answer order matches the request order, which is
    /// how the build reads the bits back.
    fn new(
        writer: SnapshotWriter<TransformFrame>,
        physics: SnapshotReader<PhysicsSnapshot>,
        physics_sender: JobSender,
        audio_sender: JobSender,
        audio_inbox: SnapshotReader<AudioReady>,
        render: WorldHandle,
    ) -> Self {
        let builder = LocalPool::new();
        let build = Rc::new(RefCell::new(BuildState::Building));
        let slot = Rc::clone(&build);
        let server = render.clone();
        log::info!("the gameplay world started; requesting the scene assets from the render world");
        builder.spawner().spawn_local(async move {
            let assets = request_assets(&server);
            let answers = match try_join_all(assets).await {
                Ok(answers) => answers,
                Err(error) => {
                    log::error!(
                        "the render world dropped an asset request ({error}); the scene stays empty"
                    );
                    *slot.borrow_mut() = BuildState::Failed;
                    return;
                }
            };
            log::info!("the render assets landed; requesting the scene instances");
            let instances = request_instances(&server, &answers);
            let answers = match try_join_all(instances).await {
                Ok(answers) => answers,
                Err(error) => {
                    log::error!(
                        "the render world dropped an instance request ({error}); the scene stays empty"
                    );
                    *slot.borrow_mut() = BuildState::Failed;
                    return;
                }
            };
            *slot.borrow_mut() = BuildState::Ready(pool_from_instances(answers));
        })
        .expect("a fresh pool takes its first task");
        Self {
            writer,
            physics,
            physics_sender,
            audio_sender,
            audio_inbox,
            music_started: false,
            builder,
            build,
            ready: false,
            build_pending: true,
            slots: Vec::new(),
            pool: Vec::new(),
            by_handle: HashMap::new(),
            pending: Vec::new(),
            clear: false,
            generation: 0,
            last_snapshot: None,
            requests: 0,
            ticks: 0,
            transforms_published: 0,
            rng: 0x853c49e6748fea9b,
        }
    }

    /// Poll the build future once; when it has landed, take the shape pool.
    /// The pool stays empty if the render world dropped a request.
    fn poll_build(&mut self) {
        if !self.build_pending {
            return;
        }
        self.builder.run_until_stalled();
        if matches!(&*self.build.borrow(), BuildState::Building) {
            return;
        }
        self.build_pending = false;
        let landed = core::mem::replace(&mut *self.build.borrow_mut(), BuildState::Failed);
        match landed {
            BuildState::Ready(pool) => {
                self.slots = pool.clone();
                self.pool = pool;
                self.ready = true;
                log::info!("the shape pool is live: {} bodies", self.pool.len());
            }
            BuildState::Failed => {
                self.ready = false;
            }
            BuildState::Building => {}
        }
    }

    /// The next symmetric random float in -1..1.
    fn random_signed(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32 - 1.0
    }

    /// Request one body from the physics world, reserving a pool entity
    /// now so two in-flight requests cannot both claim the same render
    /// entity. The reserved slot's kind picks the collider shape.
    fn drop_ball(&mut self) {
        let Some((bits, kind)) = self.pool.pop() else {
            return;
        };
        let position = Vec3::new(
            self.random_signed() * SPAWN_SPREAD,
            SPAWN_HEIGHT,
            self.random_signed() * SPAWN_SPREAD,
        );
        let velocity = Vec3::new(self.random_signed() * 2.0, 0.0, self.random_signed() * 2.0);
        let description = BodyDesc {
            position,
            velocity,
            restitution: 0.65,
            shape: shape_body(kind),
        };
        let (job, request) = Request::wrap(Box::new(move |world: &mut World| {
            spawn_body(world, description)
        }));
        self.physics_sender.send(job).ok();
        self.pending
            .push((request, (bits, kind), self.ticks, self.generation));
        log::debug!("a body was requested at {position:?}");
    }

    /// Put every live body back in the pool: the shapes clear without
    /// rebuilding anything, so the same render entities drop again. The
    /// music is left alone — the panel drives it, not the shapes.
    /// Requests still in flight are despawned when they answer, using the
    /// generation bump.
    fn clear_shapes(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let live = self.by_handle.len();
        for (handle, _) in self.by_handle.drain() {
            self.physics_sender
                .send(Box::new(move |world: &mut World| {
                    let _ = despawn_body(world, handle);
                }))
                .ok();
        }
        // A clear also cancels the drops the panel asked for but the tick
        // has not spent yet: they would repopulate the scene immediately.
        self.requests = 0;
        self.pool = available_slots(&self.slots, &self.pending);
        self.last_snapshot = None;
        self.transforms_published = 0;
        log::info!(
            "the shapes were cleared: {live} bodies despawned, {} slots free",
            self.pool.len()
        );
    }

    /// Advance the build, publish the newest physics snapshot, and keep
    /// the drop cadence going: assign render transforms to the bodies the
    /// snapshot carries and drop a new shape on the beat.
    pub fn tick(&mut self) {
        self.ticks += 1;
        // The audio world says when it can sound. Until its players exist
        // a track job finds nobody to address and is lost, so the music
        // starts on the first moment the world reports it is up.
        if !self.music_started && self.audio_inbox.latest().is_some_and(|state| state.0) {
            self.music_started = true;
            self.audio_sender
                .send(play_track_job(BGM_TRACK, bgm_midi(), true))
                .ok();
            self.audio_sender
                .send(play_track_job(SEQUENCE_TRACK, sequence_midi(), false))
                .ok();
            log::info!("both music tracks are playing");
        }
        self.poll_build();
        if !self.ready {
            return;
        }

        // A clear first: every body goes back, and the tick keeps the
        // same pool.
        if core::mem::take(&mut self.clear) {
            self.clear_shapes();
        }

        // Retire stale requests: the physics world either ran them long
        // ago or the world died; either way the reservation must not
        // leak. A request from before a restart is despawned instead of
        // mapped, so its slot returns to the pool.
        let generation = self.generation;
        self.pending
            .retain_mut(|(request, (bits, kind), age, born)| {
                if self.ticks - *age > PENDING_TIMEOUT_TICKS {
                    self.pool.push((*bits, *kind));
                    return false;
                }
                match request.poll() {
                    Some(handle) => {
                        if *born == generation {
                            self.by_handle.insert(handle, *bits);
                        } else {
                            self.physics_sender
                                .send(Box::new(move |world: &mut World| {
                                    let _ = despawn_body(world, handle);
                                }))
                                .ok();
                            self.pool.push((*bits, *kind));
                        }
                        false
                    }
                    None => true,
                }
            });

        // Consume manual requests first, then the automatic cadence. This
        // runs before the snapshot gate below: a drop is a request to the
        // physics world, not a reaction to its output.
        while self.requests > 0 {
            self.requests -= 1;
            self.drop_ball();
        }
        if self.ticks.is_multiple_of(SPAWN_TICKS) {
            self.drop_ball();
        }

        let Some(snapshot) = self.physics.latest() else {
            return;
        };
        if self
            .last_snapshot
            .as_ref()
            .is_some_and(|last| Arc::ptr_eq(last, &snapshot))
        {
            return;
        }
        self.last_snapshot = Some(snapshot.clone());

        // One status line a second: the ball flow passes through the
        // pending requests, the handle map and the physics snapshot, and
        // a stall in any of the three hides every ball.
        if self.ticks.is_multiple_of(SPAWN_TICKS) {
            log::debug!(
                "pending ball requests: {}; mapped bodies: {}; snapshot bodies: {}; published: {}",
                self.pending.len(),
                self.by_handle.len(),
                snapshot.transforms.len(),
                // The published count goes out too: a mismatch against the
                // visible transforms means the applier, not the flow, stalls.
                self.transforms_published,
            );
        }

        // The render transform set: every pool slot, holding its body's
        // pose or the hidden position.
        let mut poses: HashMap<u64, (Vec3, glam::Quat)> =
            HashMap::with_capacity(snapshot.transforms.len());
        for (handle, translation, rotation) in snapshot.transforms.iter() {
            if let Some(bits) = self.by_handle.get(handle) {
                poses.entry(*bits).or_insert((*translation, *rotation));
            }
        }
        self.transforms_published = poses.len();
        self.writer.publish(scene_frame(&self.slots, &poses));
    }
}

/// The channels between the scene's worlds, wired the same way in both
/// run modes: transforms flow to the render world, body commands to
/// physics, transport jobs to audio, and one [`Gameplay`] state ties them
/// to the render server.
///
/// Each mode builds the struct from the worlds it has — remote worlds on
/// threads for the windowed show, inline worlds with unbounded queues for
/// the headless capture — and both then spawn the ground and the gameplay
/// state through the same two methods, so the wiring is written once.
#[derive(Clone)]
pub struct SceneWiring {
    /// The mailbox gameplay publishes render transforms into.
    pub frame_writer: SnapshotWriter<TransformFrame>,
    /// The render world's end of the same mailbox.
    pub frame_reader: SnapshotReader<TransformFrame>,
    /// Commands into the physics world.
    pub physics_sender: JobSender,
    /// The physics world's latest snapshot.
    pub physics_reader: SnapshotReader<PhysicsSnapshot>,
    /// Commands into the audio world.
    pub audio_sender: JobSender,
    /// The audio world's readiness mailbox.
    pub audio_ready: SnapshotReader<AudioReady>,
}

/// Install the demo's audio sources on an audio world: the mix's gain, one
/// live voice for note jobs and one `MidiPlayer` per name in `tracks`,
/// each with the `OnFill` that renders it.
///
/// The rate is not a choice but the output's: an audio thread publishes its
/// [`AudioFormat`] when its stream opens, and a synthesizer built before
/// that would render at the wrong speed. Until someone installs sources a
/// world stays silent, which is what its readiness inbox reports.
///
/// # Panics
///
/// Panics when two names in `tracks` are the same: a transport job could
/// not tell the two tracks apart.
pub fn install_synth(
    world: &mut World,
    soundfont: &[u8],
    gain: f32,
    tracks: &[&str],
    sample_rate: u32,
) {
    for (index, one) in tracks.iter().enumerate() {
        assert!(
            !tracks[..index].contains(one),
            "the track name `{one}` is given twice"
        );
    }
    world.spawn((AudioGain(gain),));
    // Decoding an sf3's vorbis samples is the load's whole cost: a few
    // seconds in release, tens of them in an unoptimized debug build.
    log::info!("loading the embedded soundfont");
    match load_soundfont(soundfont) {
        Ok(font) => {
            if let Err(error) = SynthVoice::from_font(world, &font, sample_rate) {
                log::error!("the synthesizer failed to start ({error})");
            }
            for track in tracks {
                if let Err(error) = MidiPlayer::from_font(world, &font, sample_rate, track) {
                    log::error!("the midi track `{track}` failed to start ({error})");
                }
            }
        }
        Err(error) => log::error!("the soundfont failed to load ({error})"),
    }
}

/// A job that installs the demo's two tracks on an audio world that has
/// just reported its output format.
pub fn install_audio_job(sample_rate: u32) -> Job<()> {
    install_audio_job_with(SOUNDFONT.into(), sample_rate)
}

/// The same install, from a soundfont the caller already holds.
pub fn install_audio_job_with(soundfont: Arc<[u8]>, sample_rate: u32) -> Job<()> {
    Box::new(move |world: &mut World| {
        install_synth(
            world,
            &soundfont,
            SYNTH_GAIN,
            &[BGM_TRACK, SEQUENCE_TRACK],
            sample_rate,
        );
    })
}

impl SceneWiring {
    /// Queue the ground the balls land on: the physics world must carry
    /// it before the first drop, in both run modes.
    pub fn spawn_ground(&self) {
        self.physics_sender
            .send(Box::new(|world: &mut World| {
                spawn_fixed_cuboid(world, GROUND_POSITION, GROUND_HALF, 0.9);
            }))
            .ok();
    }

    /// The gameplay state this wiring drives, talking to `render` as
    /// its asset server.
    #[must_use]
    pub fn gameplay(&self, render: WorldHandle) -> Gameplay {
        Gameplay::new(
            self.frame_writer.clone(),
            self.physics_reader.clone(),
            self.physics_sender.clone(),
            self.audio_sender.clone(),
            self.audio_ready.clone(),
            render,
        )
    }
}

/// How a scene's audio world gets its synthesizer.
pub enum AudioSetup {
    /// No synthesizer: the world stays silent, and nothing ever reports
    /// it ready.
    Silent,
    /// Install the demo's sources before the first frame, at `sample_rate`.
    ///
    /// A world the host steps has no device to wait for, so its rate is
    /// known up front and the sources are ready the moment the scene is.
    Sources {
        /// The soundfont to build the synthesizer from.
        soundfont: Arc<[u8]>,
        /// The rate the sources render at.
        sample_rate: u32,
    },
    /// Install the demo's sources once the output device reports its
    /// format.
    ///
    /// A self-driven world opens its own output on its first tick, and a
    /// synthesizer built at any rate but the device's would render at the
    /// wrong speed. The caller watches [`SceneWorlds::audio_format`] and
    /// calls [`install_sources_when_ready`].
    WaitForDevice,
}

/// The scene's worlds, however they are driven.
///
/// Every world is a host entity carrying the runner's proxy, so the host's
/// data is the same whether the worlds run on threads or the host steps
/// them; only who calls their ticks differs.
#[derive(Clone)]
pub struct SceneWorlds {
    /// The channels between the worlds.
    pub wiring: SceneWiring,
    /// The rapier world.
    pub physics: WorldHandle,
    /// The synthesizer's world.
    pub audio: WorldHandle,
    /// The client that drives the scene; its sender is the control
    /// panel's handle on it.
    pub gameplay: WorldHandle,
    /// The audio device's format, when the setup waits for it.
    pub audio_format: Option<SnapshotReader<AudioFormat>>,
    /// Whether the audio world carries sources to transport.
    audio_sources: bool,
}

impl SceneWorlds {
    /// The sender for the audio world's transport jobs, when the scene
    /// gave it a synthesizer: the control panel's handle on the tracks.
    #[must_use]
    pub fn audio_tracks(&self) -> Option<JobSender> {
        self.audio_sources.then(|| self.wiring.audio_sender.clone())
    }
}

/// Build the scene's worlds with the runner `R`: on their own threads, or
/// stepped by the host.
///
/// The worlds are spawned in the order they depend on each other - physics
/// and audio first, so the wiring can read their inboxes off their proxy
/// entities, then gameplay - and the ground is queued before gameplay
/// starts dropping bodies on it.
pub fn spawn_scene<R: WorldRunner>(
    host: &mut World,
    render: WorldHandle,
    audio_setup: AudioSetup,
) -> SceneWorlds {
    // The physics world: fixed step, default gravity, a ground plane
    // ready before the first ball drops.
    let physics = spawn_physics_world_with::<R>(host, PhysicsConfig::default());
    let physics_sender = physics.sender();
    let physics_reader = host
        .get::<PhysicsInbox>(physics.entity())
        .expect("the physics world carries its inbox")
        .0
        .clone();

    // The audio world: a synthesizer voice plus one MIDI player per track,
    // all fed by the soundfont, mixed into the output stream. A stepped
    // world opens no device, so its sources are what a host pulls with
    // dispatch_fill.
    let audio = spawn_audio_world_with::<R>(host, AudioConfig::default());
    let audio_sender = audio.sender();
    let audio_ready = host
        .get::<AudioReadyInbox>(audio.entity())
        .expect("the audio world carries its readiness inbox")
        .0
        .clone();
    let (audio_format, audio_sources) = match audio_setup {
        AudioSetup::Silent => (None, false),
        AudioSetup::Sources {
            soundfont,
            sample_rate,
        } => {
            // The sources travel as a job, exactly as the windowed mode
            // queues them once its device reports a rate; a stepped world
            // has no device to wait for, so one step installs them before
            // the first gameplay tick reads the readiness mailbox.
            if let Err(error) = audio_sender.send(install_audio_job_with(soundfont, sample_rate)) {
                log::error!("the audio world refused its sources ({error:?})");
            }
            step_stepped_world(host, audio.entity());
            (None, true)
        }
        AudioSetup::WaitForDevice => {
            let format = host
                .get::<AudioFormatInbox>(audio.entity())
                .expect("the audio world carries its format inbox")
                .0
                .clone();
            (Some(format), true)
        }
    };

    let (frame_writer, frame_reader) = snapshot_pair::<TransformFrame>();
    let wiring = SceneWiring {
        frame_writer,
        frame_reader,
        physics_sender,
        physics_reader,
        audio_sender,
        audio_ready,
    };
    wiring.spawn_ground();

    // The gameplay world: one entity of state, driving the scene as the
    // render server's client. A world the host steps is ticked once per
    // call, so it does not pace itself; a thread of its own runs at the
    // gameplay rate and sleeps between ticks.
    let gameplay = R::spawn(
        host,
        "gameplay",
        64,
        (),
        {
            let render = render.clone();
            let wiring = wiring.clone();
            move |world: &mut World| {
                world.spawn((wiring.gameplay(render),));
            }
        },
        {
            let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / GAME_HZ));
            move |world: &mut World, control: &ThreadControl, ()| {
                if control.stop_requested() {
                    return false;
                }
                if let Some((_, mut game)) = world.query::<&mut Gameplay>().next() {
                    game.tick();
                }
                if R::SELF_DRIVEN {
                    clock.wait(control);
                }
                true
            }
        },
    );

    SceneWorlds {
        wiring,
        physics,
        audio,
        gameplay,
        audio_format,
        audio_sources,
    }
}

/// Queue the demo's sources on the audio world once its device reports a
/// format.
///
/// Only a self-driven world needs this: it opens its output on its own
/// first tick, so the rate is not known before then. A scene whose setup
/// installed the sources up front has nothing to wait for.
pub fn install_sources_when_ready(world: &mut World, worlds: &SceneWorlds) {
    let Some(format) = worlds.audio_format.clone() else {
        return;
    };
    let audio_sender = worlds.wiring.audio_sender.clone();
    let mut installed = false;
    world.spawn((OnAboutToWait::new(move |_world: &World, _entity, ()| {
        if installed {
            return;
        }
        let Some(format) = format.latest() else {
            return;
        };
        installed = true;
        if let Err(error) = audio_sender.send(install_audio_job(format.sample_rate)) {
            log::error!("the audio world refused its sources ({error:?})");
        }
    }),));
}

/// The pacing a host uses to step a scene's worlds frame by frame.
///
/// Physics runs at a rate that is not an integer multiple of the render
/// rate, so each frame carries the fraction of a step it did not spend
/// into the next one. The same remainders make a stepped run reproducible:
/// the same frames give the same steps.
pub struct SceneStepper {
    /// The physics time the last frame did not spend.
    physics_debt: f64,
    /// The audio time the last frame did not spend.
    audio_debt: f64,
    /// How long one frame lasts.
    frame_seconds: f64,
    /// How long one physics step lasts.
    physics_period: f64,
    /// How long one audio frame lasts, or none when the host pulls no
    /// audio.
    sample_period: Option<f64>,
}

impl SceneStepper {
    /// A stepper for a scene rendered at `frame_hz`, simulated at
    /// `physics_hz`, and - when `sample_rate` is given - mixed at that
    /// rate.
    #[must_use]
    pub fn new(frame_hz: f64, physics_hz: f64, sample_rate: Option<u32>) -> Self {
        Self {
            physics_debt: 0.0,
            audio_debt: 0.0,
            frame_seconds: 1.0 / frame_hz,
            physics_period: 1.0 / physics_hz,
            sample_period: sample_rate.map(|rate| 1.0 / f64::from(rate)),
        }
    }

    /// Step the scene once for a frame: gameplay ticks, physics steps as
    /// its rate comes due, and the audio world drains the transport jobs
    /// those ticks booked.
    ///
    /// The render world is not stepped here: it is a sub-world the draw
    /// drains, not one with a rate of its own.
    pub fn step(&mut self, host: &World, worlds: &SceneWorlds) {
        // Gameplay first: its requests reach the other worlds before they
        // step.
        for _ in 0..GAMEPLAY_TICKS_PER_FRAME {
            step_stepped_world(host, worlds.gameplay.entity());
        }
        self.physics_debt += self.frame_seconds;
        while self.physics_debt >= self.physics_period {
            step_stepped_world(host, worlds.physics.entity());
            self.physics_debt -= self.physics_period;
        }
        step_stepped_world(host, worlds.audio.entity());
    }

    /// The audio frames this frame's duration asks for, carrying the
    /// fraction it did not spend into the next frame.
    ///
    /// Zero when the stepper was built without a sample rate.
    pub fn audio_frames(&mut self) -> usize {
        let Some(period) = self.sample_period else {
            return 0;
        };
        self.audio_debt += self.frame_seconds;
        let frames = (self.audio_debt / period) as usize;
        self.audio_debt -= frames as f64 * period;
        frames
    }
}
/// The slots a clear hands back: every slot except the ones an in-flight
/// request reserved. A reserved slot returns when its body is despawned
/// instead, so handing it back here too would sell one render entity to
/// two bodies.
fn available_slots(slots: &[(u64, ShapeKind)], pending: &[PendingBody]) -> Vec<(u64, ShapeKind)> {
    slots
        .iter()
        .copied()
        .filter(|slot| !pending.iter().any(|(_, reserved, _, _)| reserved == slot))
        .collect()
}

/// The frame a tick publishes: every pool slot named, each either at its
/// body's pose or parked at the hidden position.
///
/// A transform frame only moves the entities it names, so a frame that
/// lists just the live bodies leaves every other entity standing where it
/// last was: a despawn — a clear, or a body that timed out — would be
/// invisible until its slot dropped again. Naming every slot makes each
/// frame a complete picture of the scene, and one that any reader can
/// apply on its own.
fn scene_frame(
    slots: &[(u64, ShapeKind)],
    poses: &HashMap<u64, (Vec3, glam::Quat)>,
) -> TransformFrame {
    TransformFrame {
        transforms: slots
            .iter()
            .map(|(bits, _)| match poses.get(bits) {
                Some((translation, rotation)) => (*bits, *translation, *rotation),
                None => (*bits, HIDDEN_POSITION, glam::Quat::IDENTITY),
            })
            .collect(),
    }
}

/// Request the scene's meshes and materials, in the order the build
/// reads their answers back: one mesh per shape kind, the ground mesh,
/// one patterned material per shape kind and color (kind-major), then the
/// ground's checker.
fn request_assets(render: &WorldHandle) -> Vec<Request<u64>> {
    let mut requests =
        Vec::with_capacity(SHAPE_KINDS.len() + 1 + SHAPE_KINDS.len() * BALL_COLORS.len() + 1);
    // One mesh per shape kind, then the ground.
    for kind in SHAPE_KINDS {
        requests.push(render.request(create_mesh(shape_mesh(kind))));
    }
    requests.push(render.request(create_mesh(cuboid_mesh(GROUND_HALF))));
    // One patterned material per shape kind and color, kind-major, then
    // the ground's checker.
    for kind in SHAPE_KINDS {
        for color in BALL_COLORS {
            requests.push(render.request(create_material_from_texture(
                (PATTERN_SIZE, PATTERN_SIZE),
                pattern_texture(kind, color),
                linear_sampler(),
            )));
        }
    }
    requests.push(render.request(create_material_from_texture(
        (GROUND_CHECKER_SIZE, GROUND_CHECKER_SIZE),
        checker_ground(),
        linear_sampler(),
    )));
    requests
}

/// Read the instance answers into the shape pool: the first answer is
/// the ground, which never moves; the rest are the pool slots, already
/// parked at the hidden position, each entry remembering the kind its
/// index draws.
fn pool_from_instances(answers: Vec<u64>) -> Vec<(u64, ShapeKind)> {
    answers
        .into_iter()
        .skip(1)
        .enumerate()
        .map(|(index, bits)| (bits, shape_kind(index)))
        .collect()
}

/// Request one instance per pool slot plus the ground, reading the mesh
/// and material bits out of the answers [`request_assets`] produced.
fn request_instances(render: &WorldHandle, answers: &[u64]) -> Vec<Request<u64>> {
    let shape_count = SHAPE_KINDS.len();
    let ground = answers[shape_count];
    let materials = &answers[shape_count + 1..];
    let ground_material = materials[shape_count * BALL_COLORS.len()];
    let mut instances = Vec::with_capacity(POOL_SIZE + 1);
    instances.push(render.request(spawn_instance(
        ground,
        ground_material,
        Transform {
            translation: GROUND_POSITION,
            ..Default::default()
        },
    )));
    // Pool slot i wears the shape and tint its index picks, matching the
    // pool's (entity, kind) pairs.
    for index in 0..POOL_SIZE {
        let kind = index % SHAPE_KINDS.len();
        // The tint walks a diagonal over the pool, so the slots a drop
        // pops back to back — a run of one kind — still carry every color
        // between them.
        let color = (index + index / SHAPE_KINDS.len()) % BALL_COLORS.len();
        let material = materials[kind * BALL_COLORS.len() + color];
        instances.push(render.request(spawn_instance(
            answers[kind],
            material,
            Transform {
                translation: HIDDEN_POSITION,
                ..Default::default()
            },
        )));
    }
    instances
}

/// The gameplay entry job: send one ball drop request into the tick.
pub fn request_drop(sender: &JobSender) {
    sender
        .send(Box::new(|world: &mut World| {
            if let Some((_, mut game)) = world.query::<&mut Gameplay>().next() {
                game.requests += 1;
            }
        }))
        .ok();
}

/// The gameplay entry job: ask the tick to put every shape back in the
/// pool. The music keeps playing; the panel drives it.
pub fn request_clear(sender: &JobSender) {
    sender
        .send(Box::new(|world: &mut World| {
            if let Some((_, mut game)) = world.query::<&mut Gameplay>().next() {
                game.clear = true;
            }
        }))
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every triangle of every generated mesh faces outward: the unlit
    /// pipeline culls back faces, so a single inward triangle shows as a
    /// hole in the surface.
    fn assert_faces_outward(name: &str, mesh: &OwnedUnlitMeshDesc) {
        let indices = mesh.indices.as_deref().unwrap_or(&[]);
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|index| Vec3::from(mesh.positions[index as usize]));
            let normal = (b - a).cross(c - a);
            // The sphere's pole rows collapse to a point, so their
            // triangles have zero area and no orientation to check.
            if normal.length_squared() <= f32::EPSILON {
                continue;
            }
            let center = (a + b + c) / 3.0;
            assert!(
                normal.dot(center) > 0.0,
                "{name}: triangle {triangle:?} faces inward",
            );
        }
    }

    #[test]
    fn meshes_face_outward() {
        for kind in SHAPE_KINDS {
            assert_faces_outward(kind_name(kind), &shape_mesh(kind));
        }
        assert_faces_outward("cuboid", &cuboid_mesh(GROUND_HALF));
    }

    /// A test's name for one shape kind.
    fn kind_name(kind: ShapeKind) -> &'static str {
        match kind {
            ShapeKind::Ball => "sphere",
            ShapeKind::Cube => "cube",
            ShapeKind::Cylinder => "cylinder",
            ShapeKind::Cone => "cone",
        }
    }

    /// A clear hands back every slot except the ones a request still in
    /// flight reserved: returning those would sell one render entity to two
    /// bodies.
    #[test]
    fn a_clear_returns_only_unreserved_slots() {
        let slots: Vec<(u64, ShapeKind)> = (0..4).map(|i| (i, ShapeKind::Ball)).collect();
        // A request the physics world has not answered yet holds slot 2.
        let (commands, _receiver) = async_channel::unbounded::<Job<()>>();
        let sender = JobSender::new(commands);
        let mut pending: Vec<PendingBody> = Vec::new();
        {
            let slot = slots[2];
            let (job, request) =
                Request::wrap(Box::new(move |_: &mut World| RigidBodyHandle::invalid()));
            sender.send(job).unwrap();
            pending.push((request, slot, 0, 0));
        }
        let free = available_slots(&slots, &pending);
        assert_eq!(
            free.iter().map(|(bits, _)| *bits).collect::<Vec<_>>(),
            vec![0, 1, 3],
            "the reserved slot must not come back while its request is in flight",
        );
        // Nothing in flight returns the whole pool.
        assert_eq!(available_slots(&slots, &[]).len(), slots.len());
    }

    /// Every frame names every slot: the ones with a live body at their
    /// pose, the rest parked. A frame that merely omitted the despawned
    /// bodies would leave them standing where they were, and a clear nobody
    /// can see is no clear at all.
    #[test]
    fn a_frame_names_every_slot() {
        let slots: Vec<(u64, ShapeKind)> = (0..4).map(|i| (i, ShapeKind::Cube)).collect();
        let pose = Vec3::new(1.0, 2.0, 3.0);
        let mut poses = HashMap::new();
        poses.insert(2u64, (pose, glam::Quat::IDENTITY));
        let frame = scene_frame(&slots, &poses);
        assert_eq!(frame.transforms.len(), slots.len());
        for (bits, translation, rotation) in &frame.transforms {
            assert!(slots.iter().any(|(slot, _)| slot == bits));
            assert_eq!(*rotation, glam::Quat::IDENTITY);
            let expected = if *bits == 2 { pose } else { HIDDEN_POSITION };
            assert_eq!(*translation, expected, "slot {bits} carries the wrong pose");
        }
        // An empty pose map is the cleared scene: every slot parked.
        let cleared = scene_frame(&slots, &HashMap::new());
        assert!(
            cleared
                .transforms
                .iter()
                .all(|(_, translation, _)| *translation == HIDDEN_POSITION)
        );
    }

    #[test]
    fn patterns_cover_their_canvas() {
        for kind in SHAPE_KINDS {
            for color in BALL_COLORS {
                let pixels = pattern_texture(kind, color);
                assert_eq!(pixels.len(), (PATTERN_SIZE * PATTERN_SIZE) as usize);
                let accents = pixels.iter().filter(|pixel| **pixel != color).count();
                assert!(accents > 0, "{kind:?}: the pattern painted nothing");
                assert!(
                    accents < pixels.len(),
                    "{kind:?}: the pattern left no base color",
                );
            }
        }
    }

    /// The build's instance answers map onto the pool in order: the
    /// ground is dropped, and slot i wears the kind its index picks, so
    /// the slot list and the shape list stay in step.
    #[test]
    fn instance_answers_map_onto_the_pool() {
        let answers: Vec<u64> = (0..POOL_SIZE as u64 + 1).collect();
        let pool = pool_from_instances(answers);
        assert_eq!(pool.len(), POOL_SIZE);
        // The ground's bit 0 is gone; the pool starts at bit 1.
        assert_eq!(pool[0].0, 1);
        for (index, (bits, kind)) in pool.iter().enumerate() {
            assert_eq!(*bits, index as u64 + 1);
            assert_eq!(*kind, shape_kind(index));
        }
    }

    /// Every mesh a pool slot can wear is one of the shape kinds, so the
    /// pool's kind lookup never falls off the end.
    #[test]
    fn pool_kinds_cover_the_shapes() {
        let pool = pool_from_instances((0..POOL_SIZE as u64 + 1).collect());
        for (_, kind) in &pool {
            assert!(SHAPE_KINDS.contains(kind));
        }
    }

    /// Count a file's key presses and releases: every sounding key must be
    /// let go again, or a looping track leaves a chord hanging forever.
    fn presses_and_releases(file: &MidiFile) -> (usize, usize) {
        let mut presses = 0;
        let mut releases = 0;
        for message in file.get_messages() {
            let MidiMessage::Normal { status, .. } = message else {
                continue;
            };
            match status & 0xF0 {
                0x90 => presses += 1,
                0x80 => releases += 1,
                _ => {}
            }
        }
        (presses, releases)
    }

    /// The demo's two tracks are well formed: sorted times, and every key
    /// press released again, so the loop's seam stays clean.
    #[test]
    fn the_midi_tracks_release_every_note() {
        for file in [bgm_midi(), sequence_midi()] {
            let (presses, releases) = presses_and_releases(&file);
            assert!(presses > 0, "a track with no notes is not music");
            assert_eq!(
                presses,
                releases,
                "the track leaves {} keys hanging",
                presses - releases
            );
            let times = file.get_times();
            for pair in times.windows(2) {
                assert!(
                    pair[0] <= pair[1],
                    "the event times run backwards at {}",
                    pair[1]
                );
            }
        }
    }

    /// The background loop is whole bars long: its length is a multiple of
    /// the bar, so a loop point lands on the downbeat rather than mid-bar.
    #[test]
    fn the_background_loop_spans_whole_bars() {
        let file = bgm_midi();
        let bars = file.get_length() / BGM_BAR_SECONDS;
        assert!(
            (bars - bars.round()).abs() < f64::EPSILON,
            "the loop is {bars} bars long"
        );
        assert!(bars >= 4.0, "a four-bar loop is the shortest that breathes");
    }
}
