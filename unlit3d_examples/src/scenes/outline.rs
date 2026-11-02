//! The outline scene: one picture drawn by both ways of drawing an outline.
//!
//! Every body is one mesh drawn twice from one buffer — lit, and again spread
//! along its own normals with its front faces culled — so the second draw
//! shows only where it reaches past the first: a band around the silhouette.
//! The band is a width in world units, so a body further from the camera wears
//! a thinner one.
//!
//! Beside them, screen-space strokes draw what a silhouette cannot: the orbit
//! the bodies stand on, the ticks along it, and a pair of armillary rings
//! circling the largest body. A stroke is a width in physical pixels, so it
//! keeps its own width across the whole ring — the near side and the far side
//! alike — where the band narrows with distance.
//!
//! The strokes come in both of the source's ink behaviours. The ticks are
//! opaque: they cover what they draw over and write the depth that hides the
//! strokes behind them. The orbit and the armillary rings are washes, blended
//! over the frame instead of covering it, so a ring shows the body it passes
//! in front of and two crossing rings deepen where they share a pixel. The
//! rings circle a body whose far side they pass behind, so the same frame
//! holds a wash in front of a body and one hidden by it.
//!
//! The two are in one scene because the picture is the argument: a frame
//! holding either alone would leave the other's promise unpinned. The scene
//! is declared twice — once for a single-sampled target and once for a
//! multisampled one — because an opaque stroke's antialiasing follows the
//! target's sample count: single-sampled it cuts at half coverage, and
//! multisampled it carries that coverage in its alpha for the blend to place.
//!
//! The bodies carry vertex normals because the shell is pushed along them; a
//! mesh without normals cannot be spread at all. One body in the frame carries
//! none, and is drawn lit anyway: its normals come from the screen-space
//! derivatives of its own surface, which is the fallback a lit draw takes when a
//! mesh has no normal channel. The two paths are in one frame because the
//! library has both, and a capture holding only the baked one would leave the
//! fallback unverified.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, camera_looking_at};
use unlit_ecs::{Bundle, DynamicBundle, bundle};
use unlit_wgpu::pipeline::LitOptions;
use unlit3d::prelude::*;

/// Where the camera looks down from.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.00, 2.75, 4.05);

/// What it looks at: a little above the orbit, so the ring is seen from above.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.00, 0.55, 0.00);

/// The band's width, in world units.
const HULL_WIDTH: f32 = 0.055;

/// The colour the band is drawn in.
const INK: glam::Vec4 = glam::Vec4::new(0.01, 0.01, 0.04, 1.0);

/// The snapshot the single-sampled capture freezes its frame under.
const SNAPSHOT: &str = "outline.webp";

/// The snapshot the multisampled capture freezes its frame under.
const MSAA_SNAPSHOT: &str = "outline_msaa.webp";

/// The ink the opaque ticks carry: the bytes the segment stream holds, which
/// the fragment writes into a linear target before it is encoded.
///
/// The band's colour reaches the shader as a float and is quantized to bytes on
/// the way in, so the stroke states the quantized bytes rather than the floats
/// again: two colours that look alike in the source would otherwise differ by
/// a few counts on screen and the scene would show two inks.
const INK_STROKE: [u8; 4] = [3, 3, 10, 255];

/// [`INK_STROKE`] washed by [`WASH_ALPHA`]: the blended orbit's ink, the same
/// colour at the alpha a wash blends with.
const INK_WASH: [u8; 4] = [3, 3, 10, 102];

/// The paper the whole scene is drawn on.
const PAPER: wgpu::Color = wgpu::Color {
    r: 0.8,
    g: 0.78,
    b: 0.72,
    a: 1.0,
};

/// The ambient term every body receives before the sun is added.
const AMBIENT: glam::Vec4 = glam::Vec4::new(0.2, 0.2, 0.24, 0.0);

/// Where the one directional light hangs and what it aims at: from above and
/// to the left, so each body is lit on one side and turned away on the other.
const SUN: (glam::Vec3, glam::Vec3) = (
    glam::Vec3::new(-3.0, 4.0, 2.0),
    glam::Vec3::new(0.0, 0.45, 0.0),
);

/// How bright that light is.
const SUN_INTENSITY: f32 = 0.55;

/// The orbit's radius, and the height it is drawn at.
///
/// Wide enough to enclose every body's whole silhouette, including the band
/// around it: a ring the bodies straddle would cross their own surfaces, and a
/// stroke drawn through a body reads as a mistake rather than as ink.
const ORBIT_RADIUS: f32 = 1.75;
const ORBIT_HEIGHT: f32 = 0.0;

/// How many segments the orbit is drawn with, and its half width in physical
/// pixels.
const ORBIT_SEGMENTS: u32 = 128;
const ORBIT_HALF_WIDTH: f32 = 1.75;

/// How many ticks stand on the orbit, how tall they are and how thick.
const TICKS: u32 = 24;
const TICK_HEIGHT: f32 = 0.22;
const TICK_HALF_WIDTH: f32 = 1.25;

/// The alpha the blended inks are washed to.
///
/// Light enough that what a ring passes in front of reads through it, deep
/// enough that two rings crossing share a pixel of clearly deeper ink.
const WASH_ALPHA: f32 = 0.4;

/// The armillary rings circling the largest body: each is drawn around an
/// axis the frame tilts differently, so the two cross twice and each passes
/// in front of the body it circles on one side and behind it on the other.
///
/// The colour is a saturated ink washed to [`WASH_ALPHA`]; the rings are drawn
/// wider than the orbit so their crossings read at a glance.
const ARMILLARY: [(glam::Vec3, [u8; 3]); 2] = [
    (glam::Vec3::new(0.0, 0.9, 1.0), [36, 96, 220]),
    (glam::Vec3::new(0.9, 0.3, 0.6), [210, 60, 60]),
];
const ARMILLARY_RADIUS: f32 = 0.98;
const ARMILLARY_SEGMENTS: u32 = 96;
const ARMILLARY_HALF_WIDTH: f32 = 2.5;

/// The point both armillary rings are centred on: the largest body's centre,
/// so each ring hugs it — outside the band [`HULL_WIDTH`] draws, inside the
/// orbit that encloses the whole group.
const ARMILLARY_CENTRE: glam::Vec3 = glam::Vec3::new(0.0, 0.92, -0.2);

/// How finely the smooth spheres are split — fine enough that a baked normal is
/// what the gradient follows rather than the facets.
const SLICES: u32 = 48;
const STACKS: u32 = 24;

/// How coarsely the one faceted body is split.
///
/// Coarse on purpose: it carries no normals, so it is shaded by the normals its
/// own triangles give it, and a facet whose corners disagree about their
/// direction is exactly what makes that shading visible as facets rather than
/// as the smooth gradient the baked normal would produce.
const FACETS: (u32, u32) = (7, 5);

/// The two bodies without normals: where each stands, how big it is and what it
/// is tinted.
///
/// They stand in front of the subject, mirrored about the middle of the frame,
/// so the derivative-shaded pair is the nearest thing in the picture — its
/// facets are what a frame is read against.
const FACETED: [(glam::Vec3, f32, glam::Vec4); 2] = [
    (
        glam::Vec3::new(-0.62, 0.56, 0.95),
        0.3,
        glam::Vec4::new(0.34, 0.29, 0.52, 1.0),
    ),
    (
        glam::Vec3::new(0.62, 0.56, 0.95),
        0.3,
        glam::Vec4::new(0.29, 0.36, 0.54, 1.0),
    ),
];

/// The bodies with normals: where each stands, how big it is and what it is
/// tinted.
///
/// Mirrored about the middle of the frame, with the largest in the centre and
/// the two smaller ones standing back. The same world-space band comes out
/// narrower on the ones further from the camera — the one thing a picture of a
/// single body could not show.
const BODIES: [(glam::Vec3, f32, glam::Vec4); 3] = [
    (
        glam::Vec3::new(0.0, 0.92, -0.2),
        0.62,
        glam::Vec4::new(0.85, 0.44, 0.2, 1.0),
    ),
    (
        glam::Vec3::new(-1.2, 0.72, -0.3),
        0.42,
        glam::Vec4::new(0.22, 0.5, 0.72, 1.0),
    ),
    (
        glam::Vec3::new(1.2, 0.72, -0.3),
        0.42,
        glam::Vec4::new(0.72, 0.66, 0.26, 1.0),
    ),
];

/// A body: the mesh both of its draws read, and the two pipelines they use.
///
/// The two entities spawned from it share the mesh handle, so the shell adds
/// no memory to the body it surrounds — the pair is one buffer and two
/// variants of one pipeline family.
struct Outlined {
    /// Where the body stands and how big it is.
    transform: Transform,
    /// The mesh both draws read.
    mesh: GpuMesh,
    /// The pipeline the body is drawn with.
    body: GpuRenderPipeline,
    /// The pipeline its shell is drawn with.
    shell: GpuRenderPipeline,
    /// The body's own tint, which the shell does not use.
    tint: glam::Vec4,
}

impl Outlined {
    /// The shell: the same mesh, spread and with the body's faces culled.
    fn spawn_shell(&self, world: &mut World, scope: Entity) {
        super::spawn_in(
            world,
            scope,
            bundle![
                self.transform.clone(),
                self.mesh.clone(),
                self.shell.clone(),
                InstanceColor::new(INK),
                InstanceHullWidth::new(HULL_WIDTH),
            ],
        );
    }
}

impl Bundle for Outlined {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            self.transform,
            InstanceColor::new(self.tint),
            self.mesh,
            self.body,
        ]
    }
}

/// The scene: outlined bodies standing on an orbit drawn with strokes, on a
/// single-sampled target.
pub static SCENE: SceneDef = SceneDef {
    id: "outline",
    title: "Inked outlines",
    description: "inverted-hull bands beside screen-space strokes",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: Some(PAPER),
    frames: 1,
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// The same scene on a multisampled target.
///
/// The world is the one [`SCENE`] builds — the difference is the sample count
/// the capture binds, which is the one thing that changes how an opaque stroke
/// is antialiased: single-sampled it cuts at half its coverage, and
/// multisampled it carries that coverage in its alpha for the blend to place.
pub static MSAA_SCENE: SceneDef = SceneDef {
    id: "outline_msaa",
    title: "Inked outlines, multisampled",
    description: "the outline scene on a four-sample target",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: Some(PAPER),
    frames: 1,
    step_seconds: None,
    samples: 4,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build: build_msaa,
};

/// Build the single-sampled scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    built(SNAPSHOT, world, context, pass, ui, size, options)
}

/// Build the multisampled scene's world content: the same world, frozen under
/// the other snapshot's name.
fn build_msaa(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    built(MSAA_SNAPSHOT, world, context, pass, ui, size, options)
}

/// The core both builds share; `snapshot` is the name frame 0 freezes under.
fn built(
    snapshot: &'static str,
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());

    // The default options draw unlit; these bodies are lit, so the flag is the
    // one thing that differs.
    let mut lit = super::default_options();
    lit.unlit = false;
    let body_key = LitPipelineKey::new(lit);
    // The shell is a second variant of the same family: the one that spreads
    // each vertex along its normal and culls the faces the body draws. It tests
    // depth but writes none, which is what keeps the band visible whatever order
    // the two draws land in.
    let shell_key = LitPipelineKey::new(LitOptions::inverted_hull());

    // The camera hangs in the shared subtree. Two sources draw this frame — the
    // meshes and the strokes — and a source reads only its own subtree, so a
    // camera beside one of them would be invisible to the other.
    let camera_entity = super::spawn_shared(
        world,
        bundle![camera_looking_at(CAMERA_EYE, CAMERA_TARGET, size)],
    );

    let source_entity = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(source)]);

    // One smooth sphere, shared by every body and by both of its draws.
    let (positions, normals, indices) = sphere(SLICES, STACKS);
    let mesh = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_lit_mesh(
            world,
            &body_key.options,
            LitMeshDesc {
                positions: &positions,
                normals: Some(&normals),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    // The faceted body's mesh carries positions and indices alone. A lit draw of
    // it shades by the normals its own surface gives it, derived per fragment
    // from the screen-space derivatives of the positions — the fallback a mesh
    // without a normal channel takes. It is coarse so the facets read.
    let (positions, _, indices) = sphere(FACETS.0, FACETS.1);
    let faceted = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_lit_mesh(
            world,
            &body_key.options,
            LitMeshDesc {
                positions: &positions,
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    for (translation, radius, tint) in BODIES {
        let outlined = Outlined {
            transform: Transform {
                translation,
                rotation: glam::Quat::IDENTITY,
                scale: glam::Vec3::splat(radius),
            },
            mesh: mesh.clone(),
            body: GpuRenderPipeline::new(body_key.clone()),
            shell: GpuRenderPipeline::new(shell_key.clone()),
            tint,
        };
        // The shell draws the faces the body culls, and at every pixel it is
        // behind the body, so the body covers it wherever the two coincide
        // whatever order they land in. The spawn order here is arbitrary.
        outlined.spawn_shell(world, source_entity);
        super::spawn_in(world, source_entity, outlined);
    }

    // The bodies without normals. They have no shell — the shell is pushed along
    // the normals a mesh carries, and these carry none — so they are here for the
    // shading their own facets give them.
    for (translation, radius, tint) in FACETED {
        super::spawn_in(
            world,
            source_entity,
            bundle![
                Transform {
                    translation,
                    rotation: glam::Quat::IDENTITY,
                    scale: glam::Vec3::splat(radius),
                },
                InstanceColor::new(tint),
                faceted.clone(),
                GpuRenderPipeline::new(body_key.clone()),
            ],
        );
    }

    super::spawn_in(world, source_entity, bundle![AmbientLight::new(AMBIENT)]);
    super::spawn_in(
        world,
        source_entity,
        super::PlacedLight::aiming(
            LightSource::new(DirectionalLight {
                color: glam::Vec4::new(1.0, 0.97, 0.9, 0.0),
                intensity: SUN_INTENSITY,
            }),
            SUN.0,
            SUN.1,
        ),
    );

    // The strokes are their own source, and each one belongs to its entity:
    // the source collects the Line components its scope sees every frame, so
    // the ring is edited the way the bodies are. The entities go under the
    // source's own, because a source draws its subtree and the shared one.
    //
    // The orbit and the armillary rings are washes — blended over the frame
    // and hidden by what stands in front of them — and the ticks are opaque,
    // so one source draws both of the source's ink behaviours.
    let lines = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(LineSource::new())]);
    let mut strokes = orbit();
    strokes.extend(armillary());
    for stroke in strokes {
        super::spawn_in(world, lines, bundle![stroke]);
    }

    SceneControl {
        // A single frame: nothing moves, so only the camera's aspect is
        // advanced, keeping a resized window from stretching the ring.
        advance: Box::new(move |world, _frame, _delta, frame_size| {
            super::aim_camera(
                world,
                world.shared(),
                camera_entity,
                CAMERA_EYE,
                CAMERA_TARGET,
                frame_size,
            );
        }),
        snapshot: Box::new(move |frame| (frame == 0).then(|| snapshot.to_owned())),
    }
}

/// The unit sphere every body is scaled from, as `(positions, normals,
/// indices)`.
///
/// The normals are the unit positions themselves, so the shell spreads along
/// the same directions the body shades by. The winding is counter-clockwise
/// seen from outside, which is what makes the faces the body draws the ones
/// the shell culls.
///
/// A body that carries no normals is the same geometry: whether the vertices
/// disagree about their direction is what decides between the two shading paths,
/// not the mesh the allocator was handed.
fn sphere(slices: u32, stacks: u32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    let row = slices + 1;
    let mut positions = Vec::with_capacity((row * (stacks + 1)) as usize);
    let mut normals = Vec::with_capacity(positions.capacity());
    for stack in 0..=stacks {
        let polar = core::f32::consts::PI * stack as f32 / stacks as f32;
        for slice in 0..=slices {
            let azimuth = core::f32::consts::TAU * slice as f32 / slices as f32;
            let normal = glam::Vec3::new(
                polar.sin() * azimuth.cos(),
                polar.cos(),
                polar.sin() * azimuth.sin(),
            );
            normals.push(normal.to_array());
            positions.push(normal.to_array());
        }
    }
    let mut indices = Vec::with_capacity((slices * stacks * 6) as usize);
    for stack in 0..stacks {
        for slice in 0..slices {
            let corner = stack * row + slice;
            indices.extend_from_slice(&[
                corner,
                corner + row,
                corner + row + 1,
                corner,
                corner + row + 1,
                corner + 1,
            ]);
        }
    }
    (positions, normals, indices)
}

/// The orbit and its ticks, as the strokes the line source draws.
///
/// The ring is closed by repeating its first station last, and every tick is
/// its own segment, so no two of them are joined by a stroke. The ring is a
/// wash — blended over the frame, its ink carrying [`WASH_ALPHA`] — and the
/// ticks are opaque, so one orbit holds both of the source's ink behaviours.
fn orbit() -> Vec<Line> {
    let mut segments = Vec::with_capacity(ORBIT_SEGMENTS as usize + TICKS as usize);
    let station = |step: u32| {
        let angle = core::f32::consts::TAU * step as f32 / ORBIT_SEGMENTS as f32;
        glam::Vec3::new(
            ORBIT_RADIUS * angle.cos(),
            ORBIT_HEIGHT,
            ORBIT_RADIUS * angle.sin(),
        )
    };
    for step in 0..ORBIT_SEGMENTS {
        segments.push(Line::blended(
            LineSegment::new(station(step).to_array(), station(step + 1).to_array())
                .with_color(INK_WASH)
                .with_half_width(ORBIT_HALF_WIDTH),
        ));
    }
    // The ticks stand on the ring, spaced round it rather than at one per
    // segment: they are the marks the ring is read against, and one at every
    // few stations keeps them from merging into a second, ragged ring.
    for tick in 0..TICKS {
        let step = tick * ORBIT_SEGMENTS / TICKS;
        let foot = station(step);
        let head = foot + glam::Vec3::Y * TICK_HEIGHT;
        segments.push(Line::opaque(
            LineSegment::new(foot.to_array(), head.to_array())
                .with_color(INK_STROKE)
                .with_half_width(TICK_HALF_WIDTH),
        ));
    }
    segments
}

/// The armillary rings, as the segments the stroke source draws.
///
/// Each ring circles the largest body around its own tilted axis: the axis is
/// the ring's normal, and the frame it is drawn in is the pair of directions
/// `glam` reports orthogonal to it. Two different tilts make the rings cross
/// twice, and circling a body puts each ring in front of it on one half and
/// behind it on the other — the picture a wash needs, of depth acting on a
/// stroke that covers nothing.
fn armillary() -> impl Iterator<Item = Line> {
    ARMILLARY.iter().flat_map(|(axis, ink)| {
        let (u, v) = axis.any_orthonormal_pair();
        let station = move |step: u32| {
            let angle = core::f32::consts::TAU * step as f32 / ARMILLARY_SEGMENTS as f32;
            ARMILLARY_CENTRE
                + u * (ARMILLARY_RADIUS * angle.cos())
                + v * (ARMILLARY_RADIUS * angle.sin())
        };
        (0..ARMILLARY_SEGMENTS).map(move |step| {
            Line::blended(
                LineSegment::new(station(step).to_array(), station(step + 1).to_array())
                    .with_color(wash(*ink))
                    .with_half_width(ARMILLARY_HALF_WIDTH),
            )
        })
    })
}

/// The ink a wash carries: `rgb` washed to [`WASH_ALPHA`].
fn wash(rgb: [u8; 3]) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], (WASH_ALPHA * 255.0) as u8]
}
