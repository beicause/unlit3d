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
use unlit_ecs::{Bundle, DynamicBundle, Entity, Scope, World, bundle};
use unlit3d::components::Transform;
use unlit3dx::prelude::*;

use super::Scene;
use super::order::{ScenePhase, TICK_BALLS, advancing};
use super::shapes::{BALL_COLORS, SHAPE_KINDS, shape_body};
use super::shared::{SceneState, create_instance, look};
use super::{GAME_HZ, GamePhysics, ScenePoses, pose_job, world_ended};
use crate::render::{BallInstance, despawn_instances_job, show, sweep};

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

/// How far a ball has come through its flow.
///
/// One field rather than one flag per step, because the steps are exclusive: a
/// ball waiting for its show is not drawn, and a ball that is drawn is not
/// waiting for one, while two flags could claim both at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BallPhase {
    /// A half is still on its way.
    Waiting,
    /// Both halves are here and a show is on its way or unanswered.
    Requested,
    /// The render world confirmed the instance wears its assets.
    Shown,
}

/// One ball, on an entity of its own in the gameplay world.
///
/// The entity is the ball's identity: every answer names it, so a ball needs
/// no id of its own and no table to be looked up in. A ball still waiting for a
/// half has `None` where that half will be, and it counts as drawn only
/// once the render world has confirmed it: see [`BallPhase`].
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
    /// How far the ball has come: see [`BallPhase`].
    pub phase: BallPhase,
    /// Where the ball starts, which is where its instance is shown.
    pub transform: Transform,
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
    /// Both halves are here and no show is queued: show this instance.
    ///
    /// The instance travels with the decision, so a ball that reaches this
    /// branch is known to have both halves and needs no second look.
    Show(Entity),
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
/// a tick that folds nothing allocates nothing but the poses it sends.
pub struct Balls {
    /// The bodies the physics world owes: one request per ball, asked under
    /// the ball it is for.
    bodies: Pending<Entity, RigidBodyHandle>,
    /// The instances the render world owes: one request per ball, asked under
    /// the ball it is for.
    instances: Pending<Entity, Entity>,
    /// The shows the render world owes: one request per ball, asked under the
    /// ball whose instance is being shown. What it answers is whether the
    /// instance really wears its assets.
    shows: Pending<Entity, bool>,
    /// What a sweep swept away: one request per sweep, asked under no key at
    /// all, because what it names is the whole scene rather than one ball.
    retired: Pending<(), Vec<BallInstance>>,
    /// Where the answers land, reused across ticks.
    settled_bodies: Vec<(Entity, Settled<RigidBodyHandle>)>,
    settled_instances: Vec<(Entity, Settled<Entity>)>,
    settled_shows: Vec<(Entity, Settled<bool>)>,
    settled_retired: Vec<((), Settled<Vec<BallInstance>>)>,
    /// The balls a request was given up on this tick, and has to reclaim.
    expired: Vec<Entity>,
    /// Whether a sweep is still owed an answer: a second sweep asked before
    /// the first answered would have its answer dropped, and with it the
    /// instances the first one retired.
    sweep_asked: bool,
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
    /// The balls the tick has decided to show, reused across ticks.
    showing: Vec<ShowCandidate>,
    /// The instances a sweep tests against the camera's view, reused across
    /// sweeps.
    sweeping: Vec<BallInstance>,
    /// The bodies a tick has to send away, kept until a send goes through so
    /// a full queue only delays the cleanup.
    despawns: Vec<RigidBodyHandle>,
    /// The instances that have nobody left to stand for, kept the same way.
    orphans: Vec<Entity>,
    /// The xorshift state for spawn placement.
    rng: u64,
}

impl Balls {
    /// The balls, driving the worlds their tick is handed.
    #[must_use]
    pub fn new(scene: &Scene) -> Self {
        Self {
            // Each of the three is bounded by the same wait: a half that has
            // not come within it is given up on, and so is a show the render
            // world never answered.
            bodies: Pending::new(scene.physics_state.clone(), Some(PENDING_TIMEOUT_TICKS)),
            instances: Pending::new(scene.render_state.clone(), Some(PENDING_TIMEOUT_TICKS)),
            shows: Pending::new(scene.render_state.clone(), Some(PENDING_TIMEOUT_TICKS)),
            retired: Pending::new(scene.render_state.clone(), Some(PENDING_TIMEOUT_TICKS)),
            settled_bodies: Vec::new(),
            settled_instances: Vec::new(),
            settled_shows: Vec::new(),
            settled_retired: Vec::new(),
            expired: Vec::new(),
            sweep_asked: false,
            drops: 0,
            clear: false,
            last_snapshot: None,
            requests: 0,
            ticks: 0,
            poses_sent: 0,
            showing: Vec::new(),
            sweeping: Vec::new(),
            despawns: Vec::new(),
            orphans: Vec::new(),
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
            .ok()
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
            phase: BallPhase::Waiting,
            transform: Transform {
                translation: position,
                ..Default::default()
            },
        }]);

        // Both halves are asked for under the ball: the answer names it, and
        // the wait is bounded, so a ball that hears nothing is reclaimed
        // rather left waiting forever.
        if let Err(error) =
            self.instances
                .request(&scene.render, ball, create_instance(mesh, material))
        {
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
        if let Err(error) = self
            .bodies
            .request(&scene.physics, ball, move |world: &mut World| {
                spawn_body(world, description)
            })
        {
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
        for (mut ball, entity) in scope.query::<(&mut Ball, Entity)>() {
            // Taken rather than read: the halves are sent away, and a ball
            // that is gone must not leave them to be sent a second time.
            if let Some(handle) = ball.body.take() {
                self.despawns.push(handle);
            }
            if let Some(instance) = ball.instance.take() {
                self.orphans.push(instance);
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
    ///
    /// Every wait is bounded by the same timeout, so this is the one place a
    /// request is given up on: an answer that never came names its ball, and
    /// the ball is reclaimed with whatever halves it did receive.
    fn fold_answers(&mut self, world: &World, state: Entity, scene: &Scene) {
        let scope = world.root_scope();

        // A ball whose request was given up on loses the halves it has, and
        // the ones still in flight are dropped when they arrive: nothing is
        // left to fill them into.
        self.expired.clear();

        // A retired instance is already gone from the render world: only its
        // body and its ball entity are left to despawn.
        self.retired.advance(&mut self.settled_retired);
        for (_, answer) in self.settled_retired.drain(..) {
            let Settled::Answered(answers) = answer else {
                // A sweep that never answered leaves the instances it was
                // asked about in the render world rather than retiring them
                // twice: the next sweep names them again.
                continue;
            };
            for answer in answers {
                if let Some(handle) = scope
                    .query_one::<&Ball>(answer.ball)
                    .ok()
                    .and_then(|found| found.body)
                {
                    self.despawns.push(handle);
                }
                scope.queue().despawn(answer.ball);
            }
        }

        // An instance whose ball is already gone has nobody to be shown on,
        // so it is despawned rather than leaked.
        self.instances.advance(&mut self.settled_instances);
        for (ball, answer) in self.settled_instances.drain(..) {
            match (scope.query_one::<&mut Ball>(ball), answer) {
                (Ok(mut found), Settled::Answered(instance)) => {
                    found.instance = Some(instance);
                }
                (Err(_), Settled::Answered(instance)) => self.orphans.push(instance),
                (_, Settled::Expired) => self.expired.push(ball),
            }
        }

        self.bodies.advance(&mut self.settled_bodies);
        for (ball, answer) in self.settled_bodies.drain(..) {
            match (scope.query_one::<&mut Ball>(ball), answer) {
                (Ok(mut found), Settled::Answered(handle)) => {
                    found.body = Some(handle);
                }
                (Err(_), Settled::Answered(handle)) => self.despawns.push(handle),
                (_, Settled::Expired) => self.expired.push(ball),
            }
        }

        // The show answers once the instance really wears its assets, so a
        // show the render world never answered leaves the ball waiting instead
        // of marking it shown.
        self.shows.advance(&mut self.settled_shows);
        for (ball, answer) in self.settled_shows.drain(..) {
            match (scope.query_one::<&mut Ball>(ball), answer) {
                (Ok(mut found), Settled::Answered(true)) => {
                    found.phase = BallPhase::Shown;
                }
                // A show answered no, or a ball that went away while its show
                // was in flight: the instance it wore has nobody left to
                // stand for.
                (Ok(mut found), Settled::Answered(false)) | (Ok(mut found), Settled::Expired) => {
                    if let Some(instance) = found.instance.take() {
                        self.orphans.push(instance);
                    }
                    self.expired.push(ball);
                }
                (Err(_), _) => self.expired.push(ball),
            }
        }

        // The balls that are ready to show, or that have waited past the point
        // of being worth waiting for.
        let stale = world_ended(&scene.physics_state) || world_ended(&scene.render_state);
        let mut showing = std::mem::take(&mut self.showing);
        showing.clear();
        for (mut ball, entity) in scope.query::<(&mut Ball, Entity)>() {
            match waiting(&ball, entity, &self.expired, stale) {
                Waiting::Wait => {}
                Waiting::Reclaim => {
                    // A reclaim is the one way a ball leaves without anyone
                    // asking, so it says so: a silent one looks like a ball
                    // that vanished.
                    log::debug!(
                        "reclaiming a ball that was {} ({:?})",
                        match ball.phase {
                            BallPhase::Waiting => "waiting for a half",
                            BallPhase::Requested => "waiting for its show",
                            BallPhase::Shown => "shown",
                        },
                        ball.kind,
                    );
                    // The halves are taken as they are sent away, so a clear
                    // later in this tick cannot send them a second time.
                    if let Some(handle) = ball.body.take() {
                        self.despawns.push(handle);
                    }
                    if let Some(instance) = ball.instance.take() {
                        self.orphans.push(instance);
                    }
                    scope.queue().despawn(entity);
                }
                Waiting::Show(instance) => {
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
            .ok()
            .and_then(|state| state.loaded.clone());
        if let Some(assets) = assets {
            for candidate in showing.drain(..) {
                let mesh = assets.meshes[candidate.kind];
                let material =
                    assets.materials[candidate.kind * BALL_COLORS.len() + candidate.color];
                let instance = candidate.instance;
                let transform = candidate.transform;
                let sent =
                    self.shows
                        .request(&scene.render, candidate.ball, move |world: &mut World| {
                            show(world, instance, mesh, material, &transform)
                        });
                match sent {
                    Ok(()) => {
                        if let Ok(mut ball) = scope.query_one::<&mut Ball>(candidate.ball) {
                            ball.phase = BallPhase::Requested;
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
        if self.orphans.is_empty() {
            return;
        }
        let instances = self.orphans.clone();
        match scene.render.send(despawn_instances_job(instances)) {
            Ok(()) | Err(SendError::Closed) => self.orphans.clear(),
            Err(SendError::Full) => {
                log::debug!("the render world's queue is full; a despawn waits a tick");
            }
        }
    }

    /// Ask the render world to despawn the shown instances the camera can no
    /// longer see. Each answer names the ball it belonged to, so the body and
    /// the ball entity go with the instance.
    ///
    /// The sweep repeats every `SWEEP_PERIOD` seconds, so a ball the camera
    /// loses is put away within one period, and one whose sweep met a full
    /// queue within two.
    fn sweep_outside_view(&mut self, world: &World, scene: &Scene) {
        let mut sweeping = std::mem::take(&mut self.sweeping);
        sweeping.clear();
        for (ball, entity) in world.root_scope().query::<(&Ball, Entity)>() {
            // Only a ball the render world confirmed is out there to be seen:
            // one still waiting for its show has no instance to lose.
            if ball.phase != BallPhase::Shown {
                continue;
            }
            if let Some(instance) = ball.instance {
                sweeping.push(BallInstance {
                    ball: entity,
                    instance,
                });
            }
        }
        if sweeping.is_empty() {
            self.sweeping = sweeping;
            return;
        }
        // One sweep at a time: a second one asked before the first answered
        // would be dropped on the floor, and with it the instances the first
        // retired.
        if self.sweep_asked {
            self.sweeping = sweeping;
            return;
        }
        match self
            .retired
            .request(&scene.render, (), sweep(sweeping.clone()))
        {
            Ok(()) => {
                self.sweep_asked = true;
                sweeping.clear();
            }
            // A closed queue is the scene going away: nothing is left to
            // sweep, and the instances stay for the next one.
            Err(SendError::Closed) => {}
            Err(SendError::Full) => {
                log::debug!("the render world's queue is full; a sweep waits for the next one");
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
                    .filter(|ball| ball.phase == BallPhase::Shown)
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

/// The balls and the tick that drops, poses and sweeps them, spawned together.
///
/// The balls hold no world of their own: every instance, body and pose is
/// asked of the worlds the tick is handed, so a state spawned without its
/// tick would hold streams nobody reads.
pub(super) struct BallDrops {
    /// The state the tick advances.
    balls: Balls,
    /// The worlds the balls ask their instances, bodies and poses from.
    scene: Scene,
}

impl Bundle for BallDrops {
    fn into_bundle(self) -> DynamicBundle {
        let Self { balls, scene } = self;
        bundle![
            balls,
            advancing(
                ScenePhase::Objects.at(TICK_BALLS),
                move |balls, world, _frame| { Self::tick(balls, world, &scene) }
            ),
        ]
    }
}

impl BallDrops {
    /// The balls for the worlds `scene` reaches.
    #[must_use]
    pub(super) fn new(scene: &Scene) -> Self {
        Self {
            balls: Balls::new(scene),
            scene: scene.clone(),
        }
    }

    /// Advance the balls by one tick: fold the answers, drop, pose and sweep.
    fn tick(balls: &mut Balls, world: &World, scene: &Scene) {
        balls.ticks += 1;
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
        balls.fold_answers(world, state, scene);
        if core::mem::take(&mut balls.clear) {
            balls.clear_balls(world, scene);
        }

        // Manual requests first, then the automatic cadence. This runs before
        // the pose publish below: a drop is a request to the physics world,
        // not a reaction to its output.
        while balls.requests > 0 {
            balls.requests -= 1;
            balls.drop_ball(world, state, scene);
        }
        if balls.ticks.is_multiple_of(SPAWN_TICKS) {
            balls.drop_ball(world, state, scene);
        }

        balls.publish(world, scene);

        if balls.ticks.is_multiple_of(SWEEP_TICKS) {
            balls.sweep_outside_view(world, scene);
        }
    }
}

/// What a tick does with a ball.
///
/// A ball is reclaimed when one of the requests it is waiting on was given up
/// on — it waited out `PENDING_TIMEOUT_TICKS`, or a world that owed it an
/// answer ended — and when the world it wears an instance in ended, because
/// the instance went with that world.
///
/// The wait is measured by the requests themselves, not by the tick the ball
/// was dropped on: a ball the render world confirmed owes nothing, and no
/// number of ticks takes it away.
fn waiting(ball: &Ball, entity: Entity, expired: &[Entity], stale: bool) -> Waiting {
    if stale {
        return Waiting::Reclaim;
    }
    if expired.contains(&entity) {
        return Waiting::Reclaim;
    }
    if ball.phase == BallPhase::Shown {
        return Waiting::Wait;
    }
    // A ball whose show is on its way is left alone: showing it twice would
    // send a second job for the same instance.
    if ball.phase == BallPhase::Requested {
        return Waiting::Wait;
    }
    match (ball.instance, ball.body) {
        (Some(instance), Some(_)) => Waiting::Show(instance),
        // A half is still on its way, so there is nothing to show yet.
        _ => Waiting::Wait,
    }
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
///
/// The poses are the one buffer a tick does not keep: the job that carries
/// them runs once in the render world and answers nothing, so the `Vec`
/// moved into it never comes back. Everything else a tick reuses is a field.
fn live_poses(scope: &Scope<'_>, snapshot: &GamePhysics, poses: &mut Vec<(Entity, Transform)>) {
    for ball in scope.query::<&Ball>() {
        if ball.phase == BallPhase::Waiting {
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
    use super::super::shared::SceneAssets;
    use super::*;
    use glam::Quat;

    fn ball(instance: Option<Entity>, body: Option<RigidBodyHandle>, shown: bool) -> Ball {
        Ball {
            kind: 0,
            color: 0,
            instance,
            body,
            phase: if shown {
                BallPhase::Shown
            } else {
                BallPhase::Waiting
            },
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
    /// reclaimed when a request it is waiting on was given up on.
    #[test]
    fn a_ball_waits_shows_and_is_reclaimed() {
        let entity = Entity::from_raw(1, 0);
        let instance = Entity::from_raw(10, 0);
        let body = RigidBodyHandle::from_raw_parts(7, 0);

        let missing_instance = ball(None, Some(body), false);
        assert_eq!(
            waiting(&missing_instance, entity, &[], false),
            Waiting::Wait
        );
        assert_eq!(
            waiting(&missing_instance, entity, &[entity], false),
            Waiting::Reclaim,
            "an instance that was given up on takes the ball with it"
        );

        let ready = ball(Some(instance), Some(body), false);
        assert_eq!(waiting(&ready, entity, &[], false), Waiting::Show(instance));
        assert_eq!(waiting(&ready, entity, &[], true), Waiting::Reclaim);

        let requested = Ball {
            phase: BallPhase::Requested,
            ..ball(Some(instance), Some(body), false)
        };
        assert_eq!(waiting(&requested, entity, &[], false), Waiting::Wait);
        assert_eq!(
            waiting(&requested, entity, &[entity], false),
            Waiting::Reclaim,
            "a show nobody answered must not keep the ball alive forever"
        );
    }

    /// A shown ball owes no request, so only the world it wears an instance in
    /// ending takes it away: a wait measured against the tick the ball was
    /// dropped on would reclaim it a fixed time after it landed.
    #[test]
    fn a_shown_ball_is_only_taken_by_its_world_ending() {
        let entity = Entity::from_raw(1, 0);
        let instance = Entity::from_raw(10, 0);
        let body = RigidBodyHandle::from_raw_parts(7, 0);
        let shown = ball(Some(instance), Some(body), true);
        assert_eq!(waiting(&shown, entity, &[], false), Waiting::Wait);
        assert_eq!(
            waiting(&shown, entity, &[], true),
            Waiting::Reclaim,
            "a world that ended takes the instance the ball wears with it"
        );
    }

    /// A scene whose queues a test runs itself to answer the requests on them,
    /// and fills again to keep whatever a tick sends away in the lists, where
    /// the test can count it.
    fn stalled_scene() -> (
        Scene,
        async_channel::Receiver<Job>,
        async_channel::Receiver<Job>,
    ) {
        let (render_tx, render_rx) = async_channel::bounded(1);
        let (physics_tx, physics_rx) = async_channel::bounded(1);
        let (audio_tx, _audio_rx) = async_channel::bounded(1);
        let scene = Scene {
            render: JobSender::new(render_tx),
            physics: JobSender::new(physics_tx),
            audio: JobSender::new(audio_tx),
            poses: Snapshot::new().reader(),
            render_state: Snapshot::new().reader(),
            physics_state: Snapshot::new().reader(),
            audio_state: Snapshot::new().reader(),
        };
        (scene, render_rx, physics_rx)
    }

    /// Run every job waiting on a queue, so the answer it publishes is there
    /// for the next tick to fold. The world those jobs are for does not exist
    /// in a test, so each runs against an empty world of its own.
    fn answer(jobs: &async_channel::Receiver<Job>) {
        let mut other = World::new();
        while let Ok(job) = jobs.try_recv() {
            job(&mut other);
        }
    }

    /// Fill every queue, so a send a tick makes on the way out reports `Full`
    /// and the cleanup stays in the lists, where a test can count it.
    fn stall(scene: &Scene) {
        for queue in [&scene.render, &scene.physics, &scene.audio] {
            let _ = queue.send(Box::new(|_: &mut World| {}));
        }
    }

    /// A scene state whose assets are loaded, so a tick does not turn back at
    /// the door: no render world builds them in a test.
    fn ready_scene_state(world: &mut World, scene: &Scene, jobs: &async_channel::Receiver<Job>) {
        let entity = world
            .root_scope_mut()
            .spawn(bundle![SceneState::new(scene)]);
        // The asset build has no render world to run in, so the job it sent is
        // dropped rather than run: the assets the tick needs are put in by
        // hand.
        while jobs.try_recv().is_ok() {}
        let Ok(mut state) = world.root_scope().query_one::<&mut SceneState>(entity) else {
            panic!("the state was just spawned")
        };
        state.ready = true;
        state.loaded = Some(SceneAssets {
            meshes: vec![Entity::from_raw(1, 0)],
            materials: vec![Entity::from_raw(2, 0); BALL_COLORS.len()],
        });
    }

    /// The answers land before the clear, so a clear takes a ball away only
    /// once: the halves it just received are sent away exactly once, not once
    /// by the reclaim and once again by the clear.
    #[test]
    fn a_clear_takes_the_halves_a_fold_just_landed() {
        let mut world = World::new();
        let (scene, render, physics) = stalled_scene();
        ready_scene_state(&mut world, &scene, &render);
        let instance = Entity::from_raw(10, 0);
        let body = RigidBodyHandle::from_raw_parts(7, 0);
        let entity = world
            .root_scope_mut()
            .spawn(bundle![ball(None, None, false)]);
        let mut balls = Balls::new(&scene);
        // Both halves are asked of the worlds the tick is handed and answered
        // by running what they were sent, so the fold finds them in flight
        // rather than published by hand.
        balls
            .instances
            .request(&scene.render, entity, move |_: &mut World| instance)
            .expect("the queue was drained");
        balls
            .bodies
            .request(&scene.physics, entity, move |_: &mut World| body)
            .expect("the queue was drained");
        answer(&render);
        answer(&physics);
        stall(&scene);
        balls.clear = true;

        BallDrops::tick(&mut balls, &world, &scene);
        let _ = world.apply();

        assert_eq!(
            world.root_scope().query::<&Ball>().count(),
            0,
            "the clear takes the ball the answers landed on"
        );
        assert_eq!(
            balls.despawns,
            vec![body],
            "the body the fold landed is sent away once"
        );
        assert_eq!(
            balls.orphans,
            vec![instance],
            "and so is the instance it was shown on"
        );
    }

    /// An answer whose ball is already gone is cleaned up: an instance with
    /// nobody to be shown on is despawned rather than left in the scene.
    #[test]
    fn an_answer_without_its_ball_is_cleaned_up() {
        let mut world = World::new();
        let (scene, render, physics) = stalled_scene();
        ready_scene_state(&mut world, &scene, &render);
        let instance = Entity::from_raw(10, 0);
        let body = RigidBodyHandle::from_raw_parts(7, 0);
        let gone = world
            .root_scope_mut()
            .spawn(bundle![ball(None, None, false)]);
        world
            .root_scope_mut()
            .despawn(gone)
            .expect("the ball the answers are about is gone");
        let _ = world.apply();

        let mut balls = Balls::new(&scene);
        balls
            .instances
            .request(&scene.render, gone, move |_: &mut World| instance)
            .expect("the queue was drained");
        balls
            .bodies
            .request(&scene.physics, gone, move |_: &mut World| body)
            .expect("the queue was drained");
        answer(&render);
        answer(&physics);
        stall(&scene);
        BallDrops::tick(&mut balls, &world, &scene);

        assert_eq!(
            balls.orphans,
            vec![instance],
            "an instance whose ball is gone is despawned"
        );
        assert_eq!(
            balls.despawns,
            vec![body],
            "and so is the body that answered for it"
        );
    }
}
