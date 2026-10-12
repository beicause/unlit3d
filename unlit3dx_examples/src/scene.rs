//! The demo's scene: its constants, meshes, camera, and the gameplay
//! that drives the worlds.
//!
//! The windowed binary and the headless run both build on this module:
//! everything here is world-shaped, free of window and thread plumbing,
//! so the same gameplay logic drives the windowed show and the
//! deterministic headless capture.

use std::sync::Arc;

use crossbeam_channel::Sender;
use glam::Vec3;
use hashbrown::HashMap;
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit3d::prelude::*;
use unlit3dx::prelude::*;

/// The window opens at this size, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (1024, 720);

/// The number of samples every frame is rendered with.
pub const SAMPLE_COUNT: u32 = 4;

/// The format the offscreen target renders into; the blit re-encodes it
/// into the surface's own format when it presents.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

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

/// A contact slower than this, in metres per second, stays silent.
pub const IMPACT_SPEED: f32 = 0.8;

/// The shortest a contact note rings, in gameplay ticks: the note a
/// contact at the impact threshold plays.
pub const NOTE_TICKS_MIN: u32 = 24;

/// The longest a contact note rings, in gameplay ticks: reached at
/// [`NOTE_SPEED_MAX`] and beyond.
pub const NOTE_TICKS_MAX: u32 = 120;

/// The impact speed whose note rings the longest, in metres per second.
pub const NOTE_SPEED_MAX: f32 = 8.0;

/// A contact note's duration, in gameplay ticks, from its impact speed:
/// a gentle tap rings short, a hard hit rings long, both capped so the
/// texture stays even.
pub fn note_ticks(speed: f32) -> u32 {
    let loudness = ((speed - IMPACT_SPEED) / (NOTE_SPEED_MAX - IMPACT_SPEED)).clamp(0.0, 1.0);
    (NOTE_TICKS_MIN as f32 + loudness * (NOTE_TICKS_MAX - NOTE_TICKS_MIN) as f32) as u32
}

/// The factor every synthesizer sample scales by, making up the headroom
/// soundfont presets leave.
pub const SYNTH_GAIN: f32 = 4.0;

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

/// The pentatonic scale contact notes are picked from, as MIDI keys.
///
/// The octave sits low on purpose: the soundfont's higher keys turn
/// brittle at impact velocities, and a lower register stays mellow.
pub const SCALE: [i32; 10] = [48, 50, 52, 55, 57, 60, 62, 64, 67, 69];

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

/// The sampler every material in the scene uses: linear filtering keeps
/// the flat colors clean under minification.
pub fn linear_sampler() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    }
}

/// The host frame's behaviour order: setup runs before the draw.
pub const FRAME_SETUP: i32 = 0;

/// The host frame's behaviour order: the draw runs last.
pub const FRAME_DRAW: i32 = 10;

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
    let camera = nested.spawn((orbit_camera(WINDOW_SIZE, ORBIT_START),));
    nested.spawn((OnFrame::new(
        FrameBehaviourOrder(RENDER_CAMERA),
        move |world, _entity, frame| {
            // The orbit advances by each frame's own delta, so the speed
            // does not follow the redraw rate.
            azimuth += ORBIT_SPEED * frame.delta_time;
            let eye = Vec3::new(
                ORBIT_TARGET.x + azimuth.cos() * ORBIT_RADIUS,
                ORBIT_HEIGHT,
                ORBIT_TARGET.z + azimuth.sin() * ORBIT_RADIUS,
            );
            let aspect = frame.size.0 as f32 / frame.size.1.max(1) as f32;
            let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
                60f32.to_radians(),
                aspect,
                0.1,
            );
            let view = glam::camera::rh::view::look_at_mat4(eye, ORBIT_TARGET, Vec3::Y);
            let _ = world.with_mut::<Camera, _>(camera, |current| {
                current.view_from_world = view;
                current.clip_from_view = projection;
                current.active = true;
            });
        },
    ),));

    // Physics snapshots become render transforms here, every frame,
    // before anything draws.
    let _ = nested.spawn_frame_behaviour(apply_transform_frames(frame_reader));
    renderer
}

/// A camera on the orbit at the given azimuth, looking at the orbit
/// target.
pub fn orbit_camera(size: (u32, u32), azimuth: f32) -> Camera {
    let eye = Vec3::new(
        ORBIT_TARGET.x + azimuth.cos() * ORBIT_RADIUS,
        ORBIT_HEIGHT,
        ORBIT_TARGET.z + azimuth.sin() * ORBIT_RADIUS,
    );
    let aspect = size.0 as f32 / size.1.max(1) as f32;
    Camera {
        view_from_world: glam::camera::rh::view::look_at_mat4(eye, ORBIT_TARGET, Vec3::Y),
        clip_from_view: glam::camera::rh::proj::directx::perspective_infinite_reverse(
            60f32.to_radians(),
            aspect,
            0.1,
        ),
        active: true,
    }
}

/// A mesh as raw arrays: positions, uvs, colors, indices.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// The opaque white every mesh vertex carries; materials tint via
/// textures.
pub const WHITE_COLOR: [u8; 4] = [255, 255, 255, 255];

/// A lat-long sphere for the balls: positions, uvs, opaque white colors
/// and indices. Flat colors make the shading geometry simple; the normals
/// come from the sphere positions, normalized on the GPU side of the
/// unlit pass.
pub fn uv_sphere(radius: f32, slices: usize, stacks: usize) -> RawMesh {
    let mut positions = Vec::with_capacity((slices + 1) * (stacks + 1));
    let mut uvs = Vec::with_capacity((slices + 1) * (stacks + 1));
    let mut colors = Vec::with_capacity((slices + 1) * (stacks + 1));
    for stack in 0..=stacks {
        let v = stack as f32 / stacks as f32;
        let phi = v * std::f32::consts::PI;
        let (sin_phi, cos_phi) = phi.sin_cos();
        for slice in 0..=slices {
            let u = slice as f32 / slices as f32;
            let theta = u * std::f32::consts::TAU;
            let (sin_theta, cos_theta) = theta.sin_cos();
            positions.push([
                radius * sin_phi * cos_theta,
                radius * cos_phi,
                radius * sin_phi * sin_theta,
            ]);
            uvs.push([u, v]);
            colors.push(WHITE_COLOR);
        }
    }
    let mut indices = Vec::with_capacity(slices * stacks * 6);
    for stack in 0..stacks {
        for slice in 0..slices {
            let row = (stack * (slices + 1) + slice) as u32;
            let next = row + slices as u32 + 1;
            // Wound CCW as seen from outside the sphere, matching the
            // back-face culling of the unlit pipeline: stacks grow
            // downward and slices counterclockwise, so each quad reads
            // (top, top-next, bottom) and (top-next, bottom-next,
            // bottom) against the outward direction.
            indices.extend_from_slice(&[row, row + 1, next, row + 1, next + 1, next]);
        }
    }
    (positions, uvs, colors, indices)
}

/// The unit right and up axes for each cuboid face, with the face's
/// outward normal, so the six faces share one loop.
pub const FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
];

/// An axis-aligned cuboid from its half extents: 24 vertices (four per
/// face, so each face has clean uvs) and 36 indices.
pub fn cuboid_mesh(half: Vec3) -> RawMesh {
    let mut positions = Vec::with_capacity(24);
    let mut uvs = Vec::with_capacity(24);
    let mut colors = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, up, right) in FACES {
        // The face spans the half extents projected onto its right and
        // up axes; the normal is a unit axis, so its projection is one
        // component.
        let extent_right = right[0] * half.x + right[1] * half.y + right[2] * half.z;
        let extent_up = up[0] * half.x + up[1] * half.y + up[2] * half.z;
        let base = positions.len() as u32;
        let face = |du: f32, dv: f32| -> [f32; 3] {
            [
                normal[0] * half.x + right[0] * du * extent_right + up[0] * dv * extent_up,
                normal[1] * half.y + right[1] * du * extent_right + up[1] * dv * extent_up,
                normal[2] * half.z + right[2] * du * extent_right + up[2] * dv * extent_up,
            ]
        };
        let corners = [
            face(-1.0, -1.0),
            face(1.0, -1.0),
            face(1.0, 1.0),
            face(-1.0, 1.0),
        ];
        let corner_uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (position, uv) in corners.into_iter().zip(corner_uvs) {
            positions.push(position);
            uvs.push(uv);
            colors.push(WHITE_COLOR);
        }
        // Corners sit at (-1, -1) (1, -1) (1, 1) (-1, 1) in the face's
        // (right, up) plane. When right crosses up along the normal that
        // plain order reads CCW from outside; a face whose axes cross
        // against the normal reads CW and takes the reversed order.
        let plain = Vec3::from(right)
            .cross(Vec3::from(up))
            .dot(Vec3::from(normal))
            > 0.0;
        if plain {
            indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
        } else {
            indices.extend_from_slice(&[base + 2, base + 1, base, base, base + 3, base + 2]);
        }
    }
    (positions, uvs, colors, indices)
}

/// Bind an offscreen size-pixel target as the render world's render
/// target, and return the color texture the frames land in.
///
/// The attachments go into the render world's resource graph exactly as a
/// window surface's do, so the renderer specializes its pipelines on them
/// the same way — and the color texture is the one a screenshot copies
/// out of.
pub fn bind_offscreen(
    world: &World,
    renderer: &mut Renderer,
    device: &wgpu::Device,
    size: (u32, u32),
) -> wgpu::Texture {
    let color = create_color_target(device, OFFSCREEN_FORMAT, size.0, size.1);
    // The texture is registered as a resource in its own right, and the
    // view depends on it, so the renderer holding the view keeps the
    // texture alive — and a screenshot can resolve the texture by handle
    // and read it back.
    let color_view = {
        let mut graph = world
            .get_mut::<ResourceGraph>(renderer.context().graph)
            .expect("the context's resource graph exists");
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments = FrameAttachments::new(
        world,
        renderer.context(),
        OFFSCREEN_FORMAT,
        size,
        SAMPLE_COUNT,
        true,
    );
    attachments.bind(world, renderer, color_view);
    color
}

/// The scene build's progress, one phase per round trip through the
/// render server.
pub enum Phase {
    /// Mesh and material requests in flight, in the order the second
    /// phase reads them back.
    Assets(Vec<Request<u64>>),
    /// Instance spawn requests in flight, ground first, then the pool.
    Instances(Vec<Request<u64>>),
    /// The pool is live and bodies may be assigned to it.
    Ready,
}

/// The answers, in request order, once every request has landed.
///
/// The gate matters: a request's answer is consumed when polled, so
/// polling a batch where only some answers have arrived would drop the
/// ones it took, and those requests would never answer again.
pub fn poll_all(requests: &[Request<u64>]) -> Option<Vec<u64>> {
    requests
        .iter()
        .all(Request::answered)
        .then(|| requests.iter().filter_map(Request::poll).collect())
}

/// A contact pair of body handles, ordered for the notes map.
pub type ContactKey = ((u32, u32), (u32, u32));

/// A sounding note: the channel, the key and the ticks left before it
/// fades.
pub type ContactNote = (i32, i32, u32);

/// The gameplay state, living on its own entity in the gameplay world.
///
/// Gameplay is authoritative and the render world's client: it owns which
/// render entity maps to which body, requests bodies from the physics
/// world, builds the scene by requesting assets and instances from the
/// render server, publishes transforms, and books the notes each
/// collision plays.
pub struct Gameplay {
    /// The mailbox gameplay publishes the per-frame transforms into.
    writer: SnapshotWriter<TransformFrame>,
    /// The physics world's latest snapshot.
    physics: SnapshotReader<PhysicsSnapshot>,
    /// The physics world's command channel.
    physics_sender: Sender<Job<()>>,
    /// The audio world's command channel.
    audio_sender: Sender<Job<()>>,
    /// The render sub-world proxy, the server the scene is built through.
    render: SubWorld,
    /// The scene build's progress.
    phase: Phase,
    /// Render entities not yet assigned to a body, as entity bits.
    pool: Vec<u64>,
    /// Body handle to render entity bits, for bodies in the snapshot.
    by_handle: HashMap<RigidBodyHandle, u64>,
    /// Body requests in flight, with the entity bits reserved and the
    /// tick the request was made, so a lost request returns its bits to
    /// the pool.
    pending: Vec<(Request<RigidBodyHandle>, u64, u32)>,
    /// The notes currently sounding, by ordered contact pair, with the
    /// channel, key and ticks left before the note fades out.
    notes: HashMap<ContactKey, ContactNote>,
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
    /// The scene is requested right away, in two phases: first the meshes
    /// and materials, then the instances that use them. The answer order
    /// matches the request order, which is how the build reads the bits
    /// back.
    pub fn new(
        writer: SnapshotWriter<TransformFrame>,
        physics: SnapshotReader<PhysicsSnapshot>,
        physics_sender: Sender<Job<()>>,
        audio_sender: Sender<Job<()>>,
        render: SubWorld,
    ) -> Self {
        let mut requests = Vec::with_capacity(2 + BALL_COLORS.len() + 1);
        let (positions, uvs, colors, indices) = uv_sphere(BALL_RADIUS, 32, 22);
        requests.push(render.request(create_mesh(OwnedUnlitMeshDesc {
            positions,
            uvs: Some(uvs),
            colors: Some(colors),
            indices: Some(indices),
        })));
        let (positions, uvs, colors, indices) = cuboid_mesh(GROUND_HALF);
        requests.push(render.request(create_mesh(OwnedUnlitMeshDesc {
            positions,
            uvs: Some(uvs),
            colors: Some(colors),
            indices: Some(indices),
        })));
        for color in BALL_COLORS {
            requests.push(render.request(create_material(color, linear_sampler())));
        }
        requests.push(render.request(create_material_from_texture(
            (GROUND_CHECKER_SIZE, GROUND_CHECKER_SIZE),
            checker_ground(),
            linear_sampler(),
        )));
        log::info!("the gameplay world started; requesting the scene assets from the render world");
        Self {
            writer,
            physics,
            physics_sender,
            audio_sender,
            render,
            phase: Phase::Assets(requests),
            pool: Vec::new(),
            by_handle: HashMap::new(),
            pending: Vec::new(),
            notes: HashMap::new(),
            last_snapshot: None,
            requests: 0,
            ticks: 0,
            transforms_published: 0,
            rng: 0x853c49e6748fea9b,
        }
    }

    /// Advance the scene build by one round trip: when every request of a
    /// phase has answered, queue the next phase's requests.
    fn advance_build(&mut self) {
        match core::mem::replace(&mut self.phase, Phase::Ready) {
            Phase::Assets(requests) => {
                self.phase = match poll_all(&requests) {
                    Some(answers) => {
                        let [sphere, ground, red, green, blue, yellow, ground_material] =
                            answers.try_into().expect("the asset request count");
                        log::info!("the render assets landed; requesting the scene instances");
                        let mut instances = Vec::with_capacity(POOL_SIZE + 1);
                        instances.push(self.render.request(spawn_instance(
                            ground,
                            ground_material,
                            Transform {
                                translation: GROUND_POSITION,
                                ..Default::default()
                            },
                        )));
                        for index in 0..POOL_SIZE {
                            instances.push(self.render.request(spawn_instance(
                                sphere,
                                [red, green, blue, yellow][index % BALL_COLORS.len()],
                                Transform {
                                    translation: HIDDEN_POSITION,
                                    ..Default::default()
                                },
                            )));
                        }
                        Phase::Instances(instances)
                    }
                    None => Phase::Assets(requests),
                };
            }
            Phase::Instances(requests) => {
                self.phase = match poll_all(&requests) {
                    Some(answers) => {
                        // The first answer is the ground, which never
                        // moves; the rest are the ball pool, already
                        // parked at the hidden position.
                        self.pool = answers;
                        self.pool.remove(0);
                        log::info!("the ball pool is live: {} balls", self.pool.len());
                        Phase::Ready
                    }
                    None => Phase::Instances(requests),
                };
            }
            Phase::Ready => {}
        }
    }

    /// The next symmetric random float in -1..1.
    fn random_signed(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32 - 1.0
    }

    /// Request one ball from the physics world, reserving a pool entity
    /// now so two in-flight requests cannot both claim the same render
    /// entity.
    fn drop_ball(&mut self) {
        let Some(bits) = self.pool.pop() else {
            return;
        };
        let position = Vec3::new(
            self.random_signed() * SPAWN_SPREAD,
            SPAWN_HEIGHT,
            self.random_signed() * SPAWN_SPREAD,
        );
        let velocity = Vec3::new(self.random_signed() * 2.0, 0.0, self.random_signed() * 2.0);
        let description = BallDesc {
            position,
            velocity,
            radius: BALL_RADIUS,
            restitution: 0.65,
        };
        let (job, request) = Request::wrap(Box::new(move |world: &mut World| {
            spawn_ball(world, description)
        }));
        self.physics_sender.send(job).ok();
        self.pending.push((request, bits, self.ticks));
        log::debug!("a ball was requested at {position:?}");
    }

    /// Advance the build, publish the newest physics snapshot: assign
    /// render transforms, play the impacts that started, fade out the
    /// contacts that stopped, and keep the drop cadence going.
    pub fn tick(&mut self) {
        self.ticks += 1;
        self.advance_build();
        if !matches!(self.phase, Phase::Ready) {
            return;
        }

        // Retire stale requests: the physics world either ran them long
        // ago or the world died; either way the reservation must not
        // leak.
        self.pending.retain_mut(|(request, bits, age)| {
            if self.ticks - *age > PENDING_TIMEOUT_TICKS {
                self.pool.push(*bits);
                return false;
            }
            match request.poll() {
                Some(handle) => {
                    self.by_handle.insert(handle, *bits);
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

        for (one, two, speed) in &snapshot.started {
            if *speed < IMPACT_SPEED {
                continue;
            }
            // Either side may be the ground; only mapped bodies sound.
            let bits = self.by_handle.get(one).or_else(|| self.by_handle.get(two));
            let Some(bits) = bits else {
                continue;
            };
            let key = SCALE[(*bits as usize) % SCALE.len()];
            // A capped ceiling keeps hard impacts from turning brash.
            let velocity = (*speed * 40.0).clamp(64.0, 104.0) as i32;
            self.audio_sender.send(note_on_job(0, key, velocity)).ok();
            self.notes
                .insert(contact_key(one, two), (0, key, note_ticks(*speed)));
        }
        // Note end: each contact's note dies when its duration runs out,
        // even if the contact lingers, so a resting ball stops droning.
        // Hard impacts rang longer than soft ones from the start.
        self.notes.retain(|_, (channel, key, left)| {
            *left = left.saturating_sub(1);
            if *left == 0 {
                self.audio_sender.send(note_off_job(*channel, *key)).ok();
                false
            } else {
                true
            }
        });

        // The render transform set: only bodies with a pool entity move
        // one.
        let transforms: Vec<(u64, Vec3, glam::Quat)> = snapshot
            .transforms
            .iter()
            .filter_map(|(handle, translation, rotation)| {
                let bits = self.by_handle.get(handle)?;
                Some((*bits, *translation, *rotation))
            })
            .collect();
        self.transforms_published = transforms.len();
        self.writer.publish(TransformFrame { transforms });
    }
}

/// The map key of a contact pair, ordered so either side can look it up.
fn contact_key(one: &RigidBodyHandle, two: &RigidBodyHandle) -> ContactKey {
    let (a, b) = (one.into_raw_parts(), two.into_raw_parts());
    if a <= b { (a, b) } else { (b, a) }
}

/// The gameplay entry job: send one ball drop request into the tick.
pub fn request_drop(sender: &Sender<Job<()>>) {
    sender
        .send(Box::new(|world: &mut World| {
            if let Some((_, mut game)) = world.query::<&mut Gameplay>().next() {
                game.requests += 1;
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
    fn assert_faces_outward(name: &str, mesh: &RawMesh) {
        let (positions, _, _, indices) = mesh;
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|index| Vec3::from(positions[index as usize]));
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
        assert_faces_outward("sphere", &uv_sphere(BALL_RADIUS, 8, 6));
        assert_faces_outward("cuboid", &cuboid_mesh(GROUND_HALF));
    }

    #[test]
    fn note_durations_track_impact_speed() {
        assert_eq!(note_ticks(IMPACT_SPEED), NOTE_TICKS_MIN);
        assert_eq!(note_ticks(NOTE_SPEED_MAX), NOTE_TICKS_MAX);
        assert_eq!(note_ticks(NOTE_SPEED_MAX * 2.0), NOTE_TICKS_MAX);
        let mut previous = 0;
        for step in 0..=16 {
            let speed = IMPACT_SPEED + (NOTE_SPEED_MAX - IMPACT_SPEED) * step as f32 / 16.0;
            let ticks = note_ticks(speed);
            assert!(ticks >= previous, "durations shrink at {speed} m/s");
            previous = ticks;
        }
    }
}
