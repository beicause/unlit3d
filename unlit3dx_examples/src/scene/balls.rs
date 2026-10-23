//! The balls: dropping them, giving each one a render instance and a physics
//! body, and publishing where they are.
//!
//! This is the scene's busiest object. Every drop spawns a `Ball` entity of its
//! own and then asks the two other worlds for the halves that make it visible:
//! the render world creates an instance, the physics world a body, and both
//! answer with the ball entity they were asked for, so an answer that no longer
//! finds its ball is cleaned up instead of leaking. A ball the camera loses
//! sight of is put away whole: the render world despawns the instance and
//! reports it, and the body and the ball entity go with it.
//!
//! Every answer names the ball, never a place in a parallel list, so two
//! answers arriving in either order land on the same ball. A ball counts as
//! shown once the render world confirms the instance wears its assets, not
//! when the job is queued: a job a full queue turned away is simply sent
//! again on the next tick.

use std::sync::Arc;

use glam::Vec3;
use unlit_ecs::{Entity, Scope, World, bundle};
use unlit3d::components::Transform;
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::Scene;
use super::shapes::{BALL_COLORS, SHAPE_KINDS, shape_body};
use super::shared::{SceneState, create_instance_job, look};
use super::tick::Tick;
use super::{GAME_HZ, GamePhysics, ScenePoses, pose_job, world_ended};
use crate::render::{BallInstance, despawn_instances_job, show_instance_job, sweep_instances_job};

/// How long between automatic ball drops, in seconds.
const SPAWN_PERIOD: f64 = 0.7;

/// How many gameplay ticks pass between automatic ball drops.
const SPAWN_TICKS: u32 = (SPAWN_PERIOD * GAME_HZ) as u32;

/// How long a ball may wait for its two halves, in seconds.
const PENDING_TIMEOUT: f64 = 5.0;

/// How many gameplay ticks a ball may wait for its two halves before its
/// instance and body are reclaimed.
const PENDING_TIMEOUT_TICKS: u32 = (PENDING_TIMEOUT * GAME_HZ) as u32;

/// How often live instances are tested against the camera's view, in seconds.
const SWEEP_PERIOD: f64 = 0.5;

/// How many gameplay ticks pass between view sweeps.
const SWEEP_TICKS: u32 = (SWEEP_PERIOD * GAME_HZ) as u32;

/// Where balls spawn, in metres above the ground.
const SPAWN_HEIGHT: f32 = 7.0;

/// How far from the centre a ball may spawn, in metres.
const SPAWN_SPREAD: f32 = 3.0;

/// One ball, on an entity of its own in the gameplay world.
///
/// The entity is the ball's identity: every answer names it, so a ball needs
/// no id of its own and no table to be looked up in. A ball still waiting for a
/// half has `None` where that half will be.
#[derive(Debug)]
pub struct Ball {
    /// Which shape it is, an index into `SHAPE_KINDS`.
    pub kind: usize,
    /// Which color it is, an index into `BALL_COLORS`.
    pub color: usize,
    /// The render instance the render world created for it.
    pub instance: Option<Entity>,
    /// The body the physics world spawned for it.
    pub body: Option<RigidBodyHandle>,
    /// The tick the drop was made on, for the wait below.
    pub requested_at: u32,
    /// Whether the render world has confirmed the instance is shown.
    pub shown: bool,
    /// Whether a show job has been queued for it and not yet answered.
    pub show_requested: bool,
    /// Where the ball starts, which is where its instance is shown.
    pub transform: Transform,
}

/// One body answered by the physics world.
#[derive(Clone, Copy, Debug)]
struct BodySpawned {
    /// The ball that asked for the body, in this world.
    ball: Entity,
    /// The body the physics world spawned for it.
    body: RigidBodyHandle,
}

/// A ball whose two halves have both landed, waiting to be shown.
struct ShowCandidate {
    /// The ball entity.
    ball: Entity,
    /// The shape kind, an index into `SceneAssets::meshes`.
    kind: usize,
    /// The color, an index into `BALL_COLORS`.
    color: usize,
    /// The instance to point at the assets and show.
    instance: Entity,
    /// The transform to show it at.
    transform: Transform,
}

/// What a tick does with a ball that has not been shown yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    /// A half is still on its way, or the show is already queued.
    Wait,
    /// Both halves are here and no show is queued: show it.
    Show,
    /// It waited too long, or a world that owed a half ended: reclaim it.
    Reclaim,
}

/// The balls, living on their own entity in the gameplay world.
///
/// A body's pose is authoritative in the physics world and drawn by the render
/// world, so this object is the one place the two meet: it reads the handles
/// the snapshot names off the ball entities, and publishes the resulting poses
/// as a job.
///
/// It reaches the other worlds through the `Scene` its tick is handed, so it
/// keeps no channel of its own: what it does keep is the answers waiting to be
/// folded into the balls they name, and the buffers those answers land in, so
/// a tick that folds nothing allocates nothing.
pub struct Balls {
    /// The answers the physics world publishes when a body is spawned, and
    /// this world's reader of them.
    spawns: Stream<BodySpawned>,
    spawned: StreamReader<BodySpawned>,
    /// The answers the render world publishes when it creates an instance,
    /// and this world's reader of them.
    creates: Stream<BallInstance>,
    created: StreamReader<BallInstance>,
    /// The answers the render world publishes when an instance is shown, and
    /// this world's reader of them.
    shows: Stream<BallInstance>,
    shown: StreamReader<BallInstance>,
    /// The instances the render world despawned because the camera lost sight
    /// of them, and this world's reader of them.
    retired: Stream<BallInstance>,
    retired_instances: StreamReader<BallInstance>,
    /// How many drops have been made, picking each ball's look. The counter
    /// only orders the looks: a ball is found by its entity, not by it.
    drops: u32,
    /// A clear requested but not yet consumed by the tick.
    clear: bool,
    /// The snapshot the previous tick published, to skip repeated work.
    last_snapshot: Option<Arc<GamePhysics>>,
    /// Ball drops requested but not yet consumed by the tick.
    requests: u32,
    /// Ticks since the world started, driving the auto drop and the sweep.
    ticks: u32,
    /// Pose entries the last job carried, for the status line.
    poses_sent: usize,
    /// Render answers reused across ticks.
    created_answers: Vec<BallInstance>,
    /// Physics answers reused across ticks.
    spawned_answers: Vec<BodySpawned>,
    /// Show answers reused across ticks.
    shown_answers: Vec<BallInstance>,
    /// Sweep answers reused across ticks.
    retired_answers: Vec<BallInstance>,
    /// The balls the tick has decided to show, reused across ticks.
    showing: Vec<ShowCandidate>,
    /// The instances a sweep tests against the camera's view, reused across
    /// sweeps.
    sweeping: Vec<BallInstance>,
    /// The bodies and instances a tick has to send away, kept until a send
    /// goes through so a full queue only delays the cleanup.
    despawns: Vec<RigidBodyHandle>,
    instances: Vec<Entity>,
    /// The xorshift state for spawn placement.
    rng: u64,
}

impl Balls {
    /// The balls, driving the worlds their tick is handed.
    #[must_use]
    pub fn new() -> Self {
        let spawns = Stream::new();
        let spawned = spawns.subscribe();
        let creates = Stream::new();
        let created = creates.subscribe();
        let shows = Stream::new();
        let shown = shows.subscribe();
        let retired = Stream::new();
        let retired_instances = retired.subscribe();
        Self {
            spawns,
            spawned,
            creates,
            created,
            shows,
            shown,
            retired,
            retired_instances,
            drops: 0,
            clear: false,
            last_snapshot: None,
            requests: 0,
            ticks: 0,
            poses_sent: 0,
            created_answers: Vec::new(),
            spawned_answers: Vec::new(),
            shown_answers: Vec::new(),
            retired_answers: Vec::new(),
            showing: Vec::new(),
            sweeping: Vec::new(),
            despawns: Vec::new(),
            instances: Vec::new(),
            rng: 0x853c49e6748fea9b,
        }
    }

    /// Ask for one more ball on the next tick.
    pub fn queue_drop(&mut self) {
        self.requests += 1;
    }

    /// Ask the next tick to put every ball away.
    pub fn queue_clear(&mut self) {
        self.clear = true;
    }

    /// The next symmetric random float in -1..1.
    fn random_signed(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32 - 1.0
    }

    /// Spawn one ball and ask the render and physics worlds for its halves.
    ///
    /// The look picks the collider shape as well as the mesh and material, so
    /// both worlds agree on what the drop is. The ball entity is queued first:
    /// it is what both answers name, so neither half can arrive for a ball that
    /// does not exist.
    ///
    /// A world that refuses the request outright — its queue is full, or it has
    /// stopped — costs this drop and no more: the ball is despawned before any
    /// answer can name it.
    fn drop_ball(&mut self, world: &World, state: Entity, scene: &Scene) {
        let (kind, color) = look(self.drops as usize);
        self.drops = self.drops.wrapping_add(1);

        let selected = world
            .root_scope()
            .query_one::<&SceneState>(state)
            .and_then(|state| {
                state.loaded.as_ref().map(|assets| {
                    (
                        assets.meshes[kind],
                        assets.materials[kind * BALL_COLORS.len() + color],
                    )
                })
            });
        let Some((mesh, material)) = selected else {
            return;
        };

        let position = Vec3::new(
            self.random_signed() * SPAWN_SPREAD,
            SPAWN_HEIGHT,
            self.random_signed() * SPAWN_SPREAD,
        );
        let velocity = Vec3::new(self.random_signed() * 2.0, 0.0, self.random_signed() * 2.0);

        // The handle comes back before the entity exists: commands land at the
        // end of this world's dispatch, and no tick reads a ball before the
        // next one, by which time it does.
        let commands = world.root_scope().queue();
        let ball = commands.spawn(bundle![Ball {
            kind,
            color,
            instance: None,
            body: None,
            requested_at: self.ticks,
            shown: false,
            show_requested: false,
            transform: Transform {
                translation: position,
                ..Default::default()
            },
        }]);

        if let Err(error) = scene.render.send(create_instance_job(
            ball,
            mesh,
            material,
            self.creates.clone(),
        )) {
            log::warn!("the render world refused an instance request ({error:?})");
            commands.despawn(ball);
            return;
        }

        let description = BodyDesc {
            position,
            velocity,
            restitution: 0.65,
            shape: shape_body(SHAPE_KINDS[kind]),
        };
        // The answer comes back over a stream rather than a reply slot: the
        // physics world publishes it when it runs the job, and the tick below
        // folds whatever arrived into the ball it names.
        let spawns = self.spawns.clone();
        let sent = scene.physics.send(Box::new(move |world: &mut World| {
            let handle = spawn_body(world, description);
            spawns.publish(BodySpawned { ball, body: handle });
        }));
        if let Err(error) = sent {
            log::warn!("the physics world refused a body request ({error:?})");
            // The instance already asked for has no ball to be shown on; its
            // answer despawns it when it arrives.
            commands.despawn(ball);
            return;
        }
        log::debug!("a ball was dropped at {position:?}");
    }

    /// Put every ball away, bodies and instances included.
    ///
    /// The instances are despawned rather than kept hidden: one the camera
    /// cannot see has no reason to stay. The music is left alone — the panel
    /// drives it, not the balls. A ball whose halves are still in flight is
    /// gone by the time they arrive, so they are cleaned up as orphans.
    fn clear_balls(&mut self, world: &World, scene: &Scene) {
        let scope = world.root_scope();
        let mut cleared = 0;
        for (ball, entity) in scope.query::<(&Ball, Entity)>() {
            if let Some(handle) = ball.body {
                self.despawns.push(handle);
            }
            if let Some(instance) = ball.instance {
                self.instances.push(instance);
            }
            scope.queue().despawn(entity);
            cleared += 1;
        }
        // A clear also cancels the drops the panel asked for but the tick has
        // not spent yet: they would repopulate the scene immediately.
        self.requests = 0;
        self.last_snapshot = None;
        self.poses_sent = 0;
        self.despawn_bodies(scene);
        self.despawn_instances(scene);
        log::info!("the balls were cleared: {cleared} despawned");
    }

    /// Fold the answers the other worlds sent since the last tick into the
    /// balls they name, show the ones that now have both halves, and put away
    /// the ones that waited too long.
    fn fold_answers(&mut self, world: &World, state: Entity, scene: &Scene) {
        let scope = world.root_scope();

        // A retired instance is already gone from the render world: only its
        // body and its ball entity are left to despawn.
        let mut retired = std::mem::take(&mut self.retired_answers);
        retired.clear();
        self.retired_instances.pump();
        self.retired_instances.copy_into(&mut retired);
        for answer in retired.drain(..) {
            if let Some(handle) = scope
                .query_one::<&Ball>(answer.ball)
                .and_then(|found| found.body)
            {
                self.despawns.push(handle);
            }
            scope.queue().despawn(answer.ball);
        }
        self.retired_answers = retired;

        // An instance whose ball is already gone has nobody to be shown on,
        // so it is despawned rather than leaked.
        let mut created = std::mem::take(&mut self.created_answers);
        created.clear();
        self.created.pump();
        self.created.copy_into(&mut created);
        for answer in created.drain(..) {
            match scope.query_one::<&mut Ball>(answer.ball) {
                Some(mut ball) => ball.instance = Some(answer.instance),
                None => self.instances.push(answer.instance),
            }
        }
        self.created_answers = created;

        let mut spawned = std::mem::take(&mut self.spawned_answers);
        spawned.clear();
        self.spawned.pump();
        self.spawned.copy_into(&mut spawned);
        for answer in spawned.drain(..) {
            match scope.query_one::<&mut Ball>(answer.ball) {
                Some(mut ball) => ball.body = Some(answer.body),
                None => self.despawns.push(answer.body),
            }
        }
        self.spawned_answers = spawned;

        // The show job answers once the instance really wears its assets, so a
        // job that never reached the render world leaves the ball waiting
        // instead of marking it shown.
        let mut shown = std::mem::take(&mut self.shown_answers);
        shown.clear();
        self.shown.pump();
        self.shown.copy_into(&mut shown);
        for answer in shown.drain(..) {
            match scope.query_one::<&mut Ball>(answer.ball) {
                Some(mut ball) => ball.shown = true,
                // The ball went away while its show was in flight, so the
                // instance it wore has nobody left to stand for.
                None => self.instances.push(answer.instance),
            }
        }
        self.shown_answers = shown;

        // The balls that are ready to show, or that have waited past the point
        // of being worth waiting for.
        let ticks = self.ticks;
        let stale = world_ended(&scene.physics_state) || world_ended(&scene.render_state);
        let mut showing = std::mem::take(&mut self.showing);
        showing.clear();
        for (ball, entity) in scope.query::<(&Ball, Entity)>() {
            if ball.shown {
                continue;
            }
            match waiting(&ball, ticks, stale) {
                Waiting::Wait => {}
                Waiting::Reclaim => {
                    if let Some(handle) = ball.body {
                        self.despawns.push(handle);
                    }
                    if let Some(instance) = ball.instance {
                        self.instances.push(instance);
                    }
                    scope.queue().despawn(entity);
                }
                Waiting::Show => {
                    let Some(instance) = ball.instance else {
                        continue;
                    };
                    showing.push(ShowCandidate {
                        ball: entity,
                        kind: ball.kind,
                        color: ball.color,
                        instance,
                        transform: ball.transform.clone(),
                    });
                }
            }
        }

        // The assets are read once for every ball that is ready to show.
        let assets = scope
            .query_one::<&SceneState>(state)
            .and_then(|state| state.loaded.clone());
        if let Some(assets) = assets {
            for candidate in showing.drain(..) {
                let mesh = assets.meshes[candidate.kind];
                let material =
                    assets.materials[candidate.kind * BALL_COLORS.len() + candidate.color];
                let job = show_instance_job(
                    candidate.ball,
                    candidate.instance,
                    mesh,
                    material,
                    candidate.transform,
                    self.shows.clone(),
                );
                match scene.render.send(job) {
                    Ok(()) => {
                        if let Some(mut ball) = scope.query_one::<&mut Ball>(candidate.ball) {
                            ball.show_requested = true;
                        }
                    }
                    // A closed queue is the scene going away, not a fault.
                    Err(SendError::Closed) => {}
                    // Backpressure: the candidate is collected again next
                    // tick, so the ball is not given up on.
                    Err(SendError::Full) => {
                        log::debug!("the render world's queue is full; a show waits a tick");
                    }
                }
            }
        }
        self.showing = showing;

        self.despawn_bodies(scene);
        self.despawn_instances(scene);
    }

    /// Despawn the bodies the tick has collected, in one job.
    ///
    /// A despawn is idempotent, so the list is kept until a send goes through:
    /// a full queue delays the cleanup by a tick instead of losing it.
    fn despawn_bodies(&mut self, scene: &Scene) {
        if self.despawns.is_empty() {
            return;
        }
        // The list is cloned rather than handed away: a job the queue cannot
        // take would go with its buffer, and the cleanup would be lost.
        let handles = self.despawns.clone();
        let sent = scene.physics.send(Box::new(move |world: &mut World| {
            for handle in handles {
                let _ = despawn_body(world, handle);
            }
        }));
        match sent {
            Ok(()) | Err(SendError::Closed) => self.despawns.clear(),
            Err(SendError::Full) => {
                log::debug!("the physics world's queue is full; a despawn waits a tick");
            }
        }
    }

    /// Despawn the instances the tick has collected, in one job.
    fn despawn_instances(&mut self, scene: &Scene) {
        if self.instances.is_empty() {
            return;
        }
        let instances = self.instances.clone();
        match scene.render.send(despawn_instances_job(instances)) {
            Ok(()) | Err(SendError::Closed) => self.instances.clear(),
            Err(SendError::Full) => {
                log::debug!("the render world's queue is full; a despawn waits a tick");
            }
        }
    }

    /// Ask the render world to despawn the shown instances the camera can no
    /// longer see. Each answer names the ball it belonged to, so the body and
    /// the ball entity go with the instance.
    ///
    /// The sweep repeats every `SWEEP_PERIOD` seconds, so a full queue only
    /// delays it: the list stays until a send goes through.
    fn sweep_outside_view(&mut self, world: &World, scene: &Scene) {
        let mut sweeping = std::mem::take(&mut self.sweeping);
        sweeping.clear();
        for (ball, entity) in world.root_scope().query::<(&Ball, Entity)>() {
            if !ball.shown {
                continue;
            }
            if let Some(instance) = ball.instance {
                sweeping.push(BallInstance {
                    ball: entity,
                    instance,
                });
            }
        }
        if !sweeping.is_empty() {
            let retired = self.retired.clone();
            match scene
                .render
                .send(sweep_instances_job(sweeping.clone(), retired))
            {
                Ok(()) | Err(SendError::Closed) => sweeping.clear(),
                Err(SendError::Full) => {
                    log::debug!("the render world's queue is full; a sweep waits for the next one");
                }
            }
        }
        self.sweeping = sweeping;
    }

    /// Publish the snapshot's poses onto the balls they name.
    fn publish(&mut self, world: &World, scene: &Scene) {
        let Some(snapshot) = scene.poses.latest() else {
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

        let mut poses = Vec::with_capacity(snapshot.transforms.len());
        live_poses(&world.root_scope(), &snapshot, &mut poses);

        // One status line a second: the ball flow passes through the answers,
        // the ball entities and the physics snapshot, and a stall in any of
        // the three hides every ball.
        if self.ticks.is_multiple_of(SPAWN_TICKS) {
            log::debug!(
                "shown balls: {}; snapshot bodies: {}; posed last tick: {}",
                world
                    .root_scope()
                    .query::<&Ball>()
                    .filter(|ball| ball.shown)
                    .count(),
                snapshot.transforms.len(),
                self.poses_sent,
            );
        }

        self.poses_sent = poses.len();
        match scene.render.send(pose_job(ScenePoses(poses))) {
            Ok(()) => {}
            // A closed queue is the scene going away, not a fault: the render
            // world stops before the gameplay world's last tick, and a pose
            // sent after it has nothing left to draw.
            Err(SendError::Closed) => {}
            // The poses are remembered as published only once they are on
            // their way: a full queue forgets the snapshot, so the same one
            // is posed again next tick instead of being skipped as unchanged.
            Err(SendError::Full) => {
                self.last_snapshot = None;
                log::debug!("the render world's queue is full; the poses wait for the next one");
            }
        }
    }
}

impl Default for Balls {
    fn default() -> Self {
        Self::new()
    }
}

impl Tick for Balls {
    fn tick(&mut self, world: &World, _frame: &Frame, scene: &Scene) {
        self.ticks += 1;
        let Some((state, ready)) = world
            .root_scope()
            .query::<(&SceneState, Entity)>()
            .next()
            .map(|(state, entity)| (entity, state.ready))
        else {
            return;
        };
        if !ready {
            return;
        }

        // The answers land before a clear, never after: a clear despawns the
        // balls it finds, so an answer naming one of them has to fill it in
        // first. Folded the other way round, the clear would queue the
        // despawn and the fold would still find the ball, show it, and leave
        // its instance behind when the entity went away.
        self.fold_answers(world, state, scene);
        if core::mem::take(&mut self.clear) {
            self.clear_balls(world, scene);
        }

        // Manual requests first, then the automatic cadence. This runs before
        // the pose publish below: a drop is a request to the physics world,
        // not a reaction to its output.
        while self.requests > 0 {
            self.requests -= 1;
            self.drop_ball(world, state, scene);
        }
        if self.ticks.is_multiple_of(SPAWN_TICKS) {
            self.drop_ball(world, state, scene);
        }

        self.publish(world, scene);

        if self.ticks.is_multiple_of(SWEEP_TICKS) {
            self.sweep_outside_view(world, scene);
        }
    }
}

/// What a tick does with a ball that is not shown yet.
///
/// The wait is bounded twice: a ball whose halves never arrive is reclaimed
/// after `PENDING_TIMEOUT_TICKS`, and one whose world ended is reclaimed at
/// once, because no answer is coming.
fn waiting(ball: &Ball, ticks: u32, stale: bool) -> Waiting {
    if stale || ticks.saturating_sub(ball.requested_at) > PENDING_TIMEOUT_TICKS {
        return Waiting::Reclaim;
    }
    if ball.instance.is_none() || ball.body.is_none() || ball.show_requested {
        return Waiting::Wait;
    }
    Waiting::Show
}

/// The poses a tick publishes: one per shown ball, at the pose the physics
/// snapshot names for its body. A ball the snapshot has not named yet is left
/// out, keeping the transform the show job gave it.
///
/// A pose names the ball's render instance, not the ball entity: the job that
/// carries it runs in the render world, whose entities are its own. A ball
/// entity from this world happens to share a number with some render entity,
/// and writing a pose onto that one moves whatever it is.
///
/// A ball whose show job is queued counts as posed: both jobs travel the same
/// queue in the order they were sent, so the show has put the instance in
/// place before the pose reads it. Waiting for the show's answer instead would
/// cost the ball a tick of movement, and pose it one frame behind the drop.
fn live_poses(scope: &Scope<'_>, snapshot: &GamePhysics, poses: &mut Vec<(Entity, Transform)>) {
    for ball in scope.query::<&Ball>() {
        if !ball.shown && !ball.show_requested {
            continue;
        }
        let (Some(handle), Some(instance)) = (ball.body, ball.instance) else {
            continue;
        };
        let Some((translation, rotation)) = snapshot.transforms.get(&handle) else {
            continue;
        };
        poses.push((
            instance,
            Transform {
                translation: *translation,
                rotation: *rotation,
                ..Default::default()
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    fn ball(instance: Option<Entity>, body: Option<RigidBodyHandle>, shown: bool) -> Ball {
        Ball {
            kind: 0,
            color: 0,
            instance,
            body,
            requested_at: 0,
            shown,
            show_requested: false,
            transform: Transform::default(),
        }
    }

    /// Only a shown ball with a body the snapshot names is posed: a ball still
    /// waiting for a half, or one the physics world has not reported yet, keeps
    /// the transform the show job gave it.
    #[test]
    fn only_shown_balls_are_posed() {
        let mut world = World::new();
        let handle = RigidBodyHandle::from_raw_parts(7, 0);
        let waiting_body = RigidBodyHandle::from_raw_parts(8, 0);
        let instance = Entity::from_raw(10, 0);
        {
            let mut scope = world.root_scope_mut();
            scope.spawn(bundle![ball(Some(instance), Some(handle), true)]);
        }
        {
            let mut scope = world.root_scope_mut();
            scope.spawn(bundle![ball(
                Some(Entity::from_raw(11, 0)),
                Some(waiting_body),
                false
            )]);
            scope.spawn(bundle![ball(Some(Entity::from_raw(12, 0)), None, true)]);
        }

        let mut snapshot = GamePhysics::default();
        snapshot
            .transforms
            .insert(handle, (Vec3::new(1.0, 2.0, 3.0), Quat::IDENTITY));
        snapshot
            .transforms
            .insert(waiting_body, (Vec3::ZERO, Quat::IDENTITY));

        let mut poses = Vec::new();
        live_poses(&world.root_scope(), &snapshot, &mut poses);

        assert_eq!(poses.len(), 1, "only a shown ball with a body is posed");
        assert_eq!(poses[0].1.translation, Vec3::new(1.0, 2.0, 3.0));
    }

    /// A pose names the ball's render instance: the job that carries it runs in
    /// the render world, where a ball entity from this world means something
    /// else entirely.
    #[test]
    fn a_pose_names_the_instance_not_the_ball() {
        let mut world = World::new();
        let handle = RigidBodyHandle::from_raw_parts(7, 0);
        let instance = Entity::from_raw(10, 0);
        let ball = {
            let mut scope = world.root_scope_mut();
            scope.spawn(bundle![ball(Some(instance), Some(handle), true)])
        };
        assert_ne!(ball, instance, "the fixture must not let them coincide");

        let mut snapshot = GamePhysics::default();
        snapshot
            .transforms
            .insert(handle, (Vec3::new(1.0, 2.0, 3.0), Quat::IDENTITY));

        let mut poses = Vec::new();
        live_poses(&world.root_scope(), &snapshot, &mut poses);

        assert_eq!(poses.len(), 1);
        assert_eq!(poses[0].0, instance, "the pose names the render instance");
        assert_ne!(poses[0].0, ball, "the pose must not name the ball entity");
    }

    /// A ball waits while a half is missing, is shown once both are in, and is
    /// reclaimed after the timeout or when a world that owed a half ended.
    #[test]
    fn a_ball_waits_shows_and_is_reclaimed() {
        let instance = Entity::from_raw(10, 0);
        let body = RigidBodyHandle::from_raw_parts(7, 0);
        let missing_instance = ball(None, Some(body), false);
        assert_eq!(waiting(&missing_instance, 0, false), Waiting::Wait);
        assert_eq!(
            waiting(&missing_instance, PENDING_TIMEOUT_TICKS + 1, false),
            Waiting::Reclaim
        );

        let ready = ball(Some(instance), Some(body), false);
        assert_eq!(waiting(&ready, 0, false), Waiting::Show);
        assert_eq!(waiting(&ready, 0, true), Waiting::Reclaim);

        let requested = Ball {
            show_requested: true,
            ..ball(Some(instance), Some(body), false)
        };
        assert_eq!(waiting(&requested, 0, false), Waiting::Wait);
        assert_eq!(
            waiting(&requested, PENDING_TIMEOUT_TICKS + 1, false),
            Waiting::Reclaim,
            "a show nobody answered must not keep the ball alive forever"
        );
    }
}
