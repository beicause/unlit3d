//! The shapes: dropping them, mapping their bodies onto render instances,
//! and publishing where they are.
//!
//! This is the scene's busiest object. It asks the render world for an
//! instance, asks physics for a body, and turns the physics snapshot into the
//! poses the render world draws. An instance belongs to its body until the
//! body is gone: the render world despawns it once the camera loses sight of
//! it, reports it back here, and the body goes with it.

use std::sync::Arc;

use glam::Vec3;
use hashbrown::HashMap;
use unlit_ecs::{Entity, World};
use unlit3d::components::Transform;
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::shapes::{BALL_COLORS, SHAPE_KINDS, shape_body};
use super::shared::{SceneState, create_instance_job, look};
use super::tick::Tick;
use super::{GAME_HZ, GamePhysics, ScenePoses, SceneWiring, pose_job};
use crate::render::{despawn_instances_job, show_instance_job, sweep_instances_job};

/// How long between automatic ball drops, in seconds.
const SPAWN_PERIOD: f64 = 0.7;

/// How many gameplay ticks pass between automatic ball drops.
pub const SPAWN_TICKS: u32 = (SPAWN_PERIOD * GAME_HZ) as u32;

/// How long a spawn request may stay unanswered, in seconds.
const PENDING_TIMEOUT: f64 = 5.0;

/// How many gameplay ticks a spawn request may stay unanswered before its
/// render instance and body are reclaimed.
pub const PENDING_TIMEOUT_TICKS: u32 = (PENDING_TIMEOUT * GAME_HZ) as u32;

/// How often live instances are tested against the camera's view, in seconds.
const SWEEP_PERIOD: f64 = 0.5;

/// How many gameplay ticks pass between view sweeps.
const SWEEP_TICKS: u32 = (SWEEP_PERIOD * GAME_HZ) as u32;

/// Where balls spawn, in metres above the ground.
pub const SPAWN_HEIGHT: f32 = 7.0;

/// How far from the centre a ball may spawn, in metres.
pub const SPAWN_SPREAD: f32 = 3.0;

/// One body request in flight: the instance it asked for, the body the
/// physics world answered with, and the generation it belongs to.
///
/// The two halves may land in either order, and a clear may remove the record
/// before either lands: an answer that no longer finds its request is cleaned
/// up instead of leaking.
struct PendingBody {
    id: u64,
    kind: usize,
    color: usize,
    instance: Option<Entity>,
    handle: Option<RigidBodyHandle>,
    born: u32,
    age: u32,
    transform: Transform,
}

/// One body answered by the physics world: the request it answers, the
/// generation it belongs to, and the body the world spawned.
type BodySpawned = (u64, u32, RigidBodyHandle);

/// One render instance answered by the render world: the request it answers
/// and the instance the world spawned.
type InstanceCreated = (u64, Entity);

/// The shapes, living on their own entity in the gameplay world.
///
/// A body's pose is authoritative in the physics world and drawn by the
/// render world, so this object is the one place the two meet: it maps the
/// handles the snapshot names onto the render instances it asked for, and
/// publishes the resulting poses as a job.
pub struct Balls {
    /// The render world's command channel: the instances and the poses go to
    /// it as jobs, like every other change to that world.
    render: JobSender,
    /// The physics world's latest pose snapshot.
    physics: SnapshotReader<GamePhysics>,
    /// The physics world's command channel.
    physics_sender: JobSender,
    /// The answers the physics world pushes when a body is spawned, and
    /// this world's reader of them.
    spawns: Stream<BodySpawned>,
    spawned: StreamReader<BodySpawned>,
    /// The answers the render world pushes when it creates an instance, and
    /// this world's reader of them.
    creates: Stream<InstanceCreated>,
    created: StreamReader<InstanceCreated>,
    /// The instances the render world despawned because the camera lost sight
    /// of them, and this world's reader of them.
    retired: Stream<Entity>,
    retired_instances: StreamReader<Entity>,
    /// The id the next body request is given. It is wide enough never to
    /// come back around: an answer to a request that already timed out must
    /// not be mistaken for a later request's.
    next_request: u64,
    /// Body requests in flight, with the instance requested, the tick the
    /// request was made, and the generation it belongs to: a request from
    /// before a restart is despawned instead of mapped when it answers.
    ///
    /// These requests are polled from the tick rather than awaited in a
    /// future: answering one mutates the shape state (the handle map, the
    /// instance it asked for, the rng), and a future cannot hold `&mut Balls`
    /// across an await; a clear cancels in-flight requests by generation,
    /// which a pool of spawned futures could only do with abort handles it
    /// does not have; and the timeout below counts ticks, not wall time,
    /// which keeps the headless snapshot deterministic regardless of thread
    /// timing.
    pending: Vec<PendingBody>,
    /// Body handle to render instance, for every live body.
    by_handle: HashMap<RigidBodyHandle, Entity>,
    /// Render instance to body handle, for the sweep's answers.
    by_instance: HashMap<Entity, RigidBodyHandle>,
    /// A clear requested but not yet consumed by the tick.
    clear: bool,
    /// Bumped on every clear, so stale requests are told apart.
    generation: u32,
    /// The snapshot the previous tick published, to skip repeated work.
    last_snapshot: Option<Arc<GamePhysics>>,
    /// Ball spawns requested but not yet consumed by the tick.
    requests: u32,
    /// Ticks since the world started, driving the auto spawn and the sweep.
    ticks: u32,
    /// Pose entries the last job carried, for the status line.
    poses_sent: usize,
    /// Render answers reused across ticks.
    created_answers: Vec<InstanceCreated>,
    /// Physics answers reused across ticks.
    spawned_answers: Vec<BodySpawned>,
    /// Sweep answers reused across ticks.
    retired_answers: Vec<Entity>,
    /// The xorshift state for spawn placement.
    rng: u64,
}

impl Balls {
    /// The shapes, driving the worlds `wiring` reaches.
    #[must_use]
    pub fn new(wiring: &SceneWiring) -> Self {
        let spawns = Stream::new();
        let spawned = spawns.subscribe();
        let creates = Stream::new();
        let created = creates.subscribe();
        let retired = Stream::new();
        let retired_instances = retired.subscribe();
        Self {
            render: wiring.render_sender.clone(),
            physics: wiring.physics_reader.clone(),
            physics_sender: wiring.physics_sender.clone(),
            spawns,
            spawned,
            creates,
            created,
            retired,
            retired_instances,
            next_request: 0,
            pending: Vec::new(),
            by_handle: HashMap::new(),
            by_instance: HashMap::new(),
            clear: false,
            generation: 0,
            last_snapshot: None,
            requests: 0,
            ticks: 0,
            poses_sent: 0,
            created_answers: Vec::new(),
            spawned_answers: Vec::new(),
            retired_answers: Vec::new(),
            rng: 0x853c49e6748fea9b,
        }
    }

    /// Ask for one more shape on the next tick.
    pub fn request_drop(&mut self) {
        self.requests += 1;
    }

    /// Ask the next tick to put every shape away.
    pub fn request_clear(&mut self) {
        self.clear = true;
    }

    /// The next symmetric random float in -1..1.
    fn random_signed(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32 - 1.0
    }

    /// Request one body from the physics world, asking the render world for
    /// its instance beside the body request.
    ///
    /// The look picks the collider shape as well as the mesh and material, so
    /// both worlds agree on what the drop is.
    fn drop_ball(&mut self, world: &World, state: Entity) {
        let (kind, color) = look(self.next_request as usize);
        let id = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        let born = self.generation;

        let selected = world
            .root_scope()
            .query_one::<&SceneState>(state)
            .and_then(|scene| {
                scene.loaded.as_ref().map(|assets| {
                    (
                        assets.meshes[kind],
                        assets.materials[kind * BALL_COLORS.len() + color],
                    )
                })
            });
        let Some((mesh, material)) = selected else {
            return;
        };
        if let Err(error) = self.render.send(create_instance_job(
            id,
            mesh,
            material,
            self.creates.clone(),
        )) {
            log::warn!("the render world refused an instance request ({error:?})");
            return;
        }

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
            shape: shape_body(SHAPE_KINDS[kind]),
        };
        // The answer comes back over a stream rather than a reply slot: the
        // physics world pushes it when it runs the job, and the tick below
        // folds whatever arrived into its own state.
        let spawns = self.spawns.clone();
        let sent = self.physics_sender.send(Box::new(move |world: &mut World| {
            let handle = spawn_body(world, description);
            let _ = spawns.publish((id, born, handle));
        }));
        if let Err(error) = sent {
            log::warn!("the physics world refused a body request ({error:?})");
            // The creation already in flight answers with an instance nobody
            // reserves any more; its answer is despawned when it arrives.
            return;
        }
        self.pending.push(PendingBody {
            id,
            kind,
            color,
            instance: None,
            handle: None,
            born,
            age: self.ticks,
            transform: Transform {
                translation: position,
                ..Default::default()
            },
        });
        log::debug!("a body was requested at {position:?}");
    }

    /// Put every live body away. The instances are despawned rather than kept
    /// hidden: one the camera cannot see has no reason to stay. The music is
    /// left alone — the panel drives it, not the shapes. Requests still in
    /// flight are despawned when they answer, using the generation bump.
    fn clear_shapes(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let live = self.by_handle.len();
        let requests = self.pending.len();
        let mut handles = Vec::with_capacity(live + requests);
        let mut instances = Vec::with_capacity(live + requests);
        for (handle, instance) in self.by_handle.drain() {
            handles.push(handle);
            instances.push(instance);
        }
        self.by_instance.clear();
        for pending in self.pending.drain(..) {
            if let Some(handle) = pending.handle {
                handles.push(handle);
            }
            if let Some(instance) = pending.instance {
                instances.push(instance);
            }
        }
        // A clear also cancels the drops the panel asked for but the tick
        // has not spent yet: they would repopulate the scene immediately.
        self.requests = 0;
        self.despawn_bodies(handles);
        self.despawn_instances(instances);
        self.last_snapshot = None;
        self.poses_sent = 0;
        log::info!("the shapes were cleared: {live} bodies despawned, {requests} requests dropped");
    }

    /// Map the bodies, instances and view answers the other worlds sent since
    /// the last tick, then retire the requests that either ran long ago or
    /// never will: the world died, and their halves must not leak.
    fn retire_stale(&mut self, world: &World, state: Entity) {
        let mut despawns = Vec::new();
        let mut instances = Vec::new();

        // A retired instance is already gone from the render world: only its
        // body is left to despawn.
        self.retired_instances.pump();
        let mut retired = std::mem::take(&mut self.retired_answers);
        retired.clear();
        self.retired_instances.copy_into(&mut retired);
        for instance in retired.drain(..) {
            if let Some(handle) = self.by_instance.remove(&instance) {
                self.by_handle.remove(&handle);
                despawns.push(handle);
            }
        }
        self.retired_answers = retired;

        self.created.pump();
        let mut created = std::mem::take(&mut self.created_answers);
        created.clear();
        self.created.copy_into(&mut created);
        for (id, instance) in created.drain(..) {
            // A request that already timed out has no entry left; the
            // instance it made is still hidden, so it is despawned rather
            // than leaked.
            match self.pending.iter_mut().find(|pending| pending.id == id) {
                Some(pending) => pending.instance = Some(instance),
                None => instances.push(instance),
            }
        }
        self.created_answers = created;

        self.spawned.pump();
        let mut spawned = std::mem::take(&mut self.spawned_answers);
        spawned.clear();
        self.spawned.copy_into(&mut spawned);
        for (id, _born, handle) in spawned.drain(..) {
            match self.pending.iter_mut().find(|pending| pending.id == id) {
                Some(pending) => pending.handle = Some(handle),
                None => despawns.push(handle),
            }
        }
        self.spawned_answers = spawned;

        // A request whose two halves both landed is ready to show: a stale
        // one is despawned instead.
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].handle.is_some() && self.pending[index].instance.is_some() {
                let pending = self.pending.swap_remove(index);
                self.finish(pending, world, state, &mut despawns, &mut instances);
            } else {
                index += 1;
            }
        }

        // A request that never answered loses whatever half it did get.
        let ticks = self.ticks;
        let mut index = 0;
        while index < self.pending.len() {
            if ticks.saturating_sub(self.pending[index].age) > PENDING_TIMEOUT_TICKS {
                let pending = self.pending.swap_remove(index);
                if let Some(handle) = pending.handle {
                    despawns.push(handle);
                }
                if let Some(instance) = pending.instance {
                    instances.push(instance);
                }
            } else {
                index += 1;
            }
        }

        self.despawn_bodies(despawns);
        self.despawn_instances(instances);
    }

    /// Show one request's instance at its body's side and map the handle onto
    /// it, or undo both halves when the request belongs to an older
    /// generation.
    fn finish(
        &mut self,
        pending: PendingBody,
        world: &World,
        state: Entity,
        despawns: &mut Vec<RigidBodyHandle>,
        instances: &mut Vec<Entity>,
    ) {
        let (Some(handle), Some(instance)) = (pending.handle, pending.instance) else {
            return;
        };
        if pending.born != self.generation {
            despawns.push(handle);
            instances.push(instance);
            return;
        }
        let selected = world
            .root_scope()
            .query_one::<&SceneState>(state)
            .and_then(|scene| {
                scene.loaded.as_ref().map(|assets| {
                    (
                        assets.meshes[pending.kind],
                        assets.materials[pending.kind * BALL_COLORS.len() + pending.color],
                    )
                })
            });
        let Some((mesh, material)) = selected else {
            despawns.push(handle);
            instances.push(instance);
            return;
        };
        match self.render.send(show_instance_job(
            instance,
            mesh,
            material,
            pending.transform,
        )) {
            Ok(()) => {
                self.by_handle.insert(handle, instance);
                self.by_instance.insert(instance, handle);
            }
            // A closed queue is the scene going away, not a fault.
            Err(SendError::Closed) => {}
            Err(error) => {
                log::warn!("the render world refused a show job ({error:?})");
                despawns.push(handle);
                instances.push(instance);
            }
        }
    }

    /// Despawn the bodies it names, in one job.
    fn despawn_bodies(&self, handles: Vec<RigidBodyHandle>) {
        if handles.is_empty() {
            return;
        }
        let sent = self.physics_sender.send(Box::new(move |world: &mut World| {
            for handle in handles {
                let _ = despawn_body(world, handle);
            }
        }));
        if let Err(error) = sent {
            log::warn!("the physics world refused a despawn job ({error:?})");
        }
    }

    /// Despawn the instances it names, in one job.
    fn despawn_instances(&self, instances: Vec<Entity>) {
        if instances.is_empty() {
            return;
        }
        if let Err(error) = self.render.send(despawn_instances_job(instances)) {
            log::warn!("the render world refused a despawn job ({error:?})");
        }
    }

    /// Ask the render world to despawn the live instances the camera can no
    /// longer see. The answers arrive on the retired stream, and the bodies
    /// they name go with them.
    fn sweep_outside_view(&mut self) {
        if self.by_handle.is_empty() {
            return;
        }
        let instances: Vec<Entity> = self.by_handle.values().copied().collect();
        if let Err(error) = self
            .render
            .send(sweep_instances_job(instances, self.retired.clone()))
        {
            log::warn!("the render world refused a sweep job ({error:?})");
        }
    }

    /// Publish the snapshot's poses onto the live bodies.
    fn publish(&mut self) {
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

        // One status line a second: the ball flow passes through the pending
        // requests, the handle map and the physics snapshot, and a stall in
        // any of the three hides every ball.
        if self.ticks.is_multiple_of(SPAWN_TICKS) {
            log::debug!(
                "pending ball requests: {}; mapped bodies: {}; snapshot bodies: {}; published: {}",
                self.pending.len(),
                self.by_handle.len(),
                snapshot.transforms.len(),
                // The posed count goes out too: a mismatch against the
                // visible transforms means the pose job, not the flow,
                // stalls.
                self.poses_sent,
            );
        }

        let poses = live_poses(&self.by_handle, &snapshot);
        self.poses_sent = poses.len();
        // The poses travel as a job: what the render world shows is its own
        // to hold, so the shapes send the state rather than leaving it in a
        // mailbox for the world to apply.
        match self.render.send(pose_job(ScenePoses(poses))) {
            Ok(()) => {}
            // A closed queue is the scene going away, not a fault: the render
            // world stops before the gameplay world's last tick, and a pose
            // sent after it has nothing left to draw.
            Err(SendError::Closed) => {}
            Err(error) => log::warn!("the render world refused a pose job ({error:?})"),
        }
    }
}

impl Tick for Balls {
    fn tick(&mut self, world: &World, _frame: &Frame) {
        self.ticks += 1;
        let Some((state, ready)) = world
            .root_scope()
            .query::<(&SceneState, Entity)>()
            .next()
            .map(|(scene_state, entity)| (entity, scene_state.is_ready()))
        else {
            return;
        };
        if !ready {
            return;
        }

        // A clear first: every body goes away, and the instances are ready to
        // be despawned.
        if core::mem::take(&mut self.clear) {
            self.clear_shapes();
        }

        self.retire_stale(world, state);

        // Consume manual requests first, then the automatic cadence. This
        // runs before the snapshot gate below: a drop is a request to the
        // physics world, not a reaction to its output.
        while self.requests > 0 {
            self.requests -= 1;
            self.drop_ball(world, state);
        }
        if self.ticks.is_multiple_of(SPAWN_TICKS) {
            self.drop_ball(world, state);
        }

        self.publish();

        if self.ticks.is_multiple_of(SWEEP_TICKS) {
            self.sweep_outside_view();
        }
    }
}

/// The poses a tick publishes: one per live body the snapshot names. A body
/// the snapshot has not named yet keeps the transform the show job gave it.
fn live_poses(
    by_handle: &HashMap<RigidBodyHandle, Entity>,
    snapshot: &GamePhysics,
) -> Vec<(Entity, Transform)> {
    let mut poses = Vec::with_capacity(by_handle.len());
    for (handle, translation, rotation) in &snapshot.transforms {
        if let Some(instance) = by_handle.get(handle) {
            poses.push((
                *instance,
                Transform {
                    translation: *translation,
                    rotation: *rotation,
                    ..Default::default()
                },
            ));
        }
    }
    poses
}
