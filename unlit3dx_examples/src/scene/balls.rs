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
use unlit_ecs::{Bundle, DynamicBundle, Entity, RemoteEntity, Scope, World, bundle};
use unlit3d::components::Transform;
use unlit3dx::prelude::*;

use super::Scene;
use super::shapes::{BALL_COLORS, SHAPE_KINDS, shape_body};
use super::shared::{SceneState, create_instance, look};
use super::tick::advancing_in;
use super::{GamePhysics, ScenePoses, pose_job, world_ended};
use crate::render::{BallInstance, despawn_instances_job, show, sweep};

/// How long between automatic ball drops, in seconds.
const SPAWN_PERIOD: f32 = 0.7;

/// How long a ball may wait for its two halves before its instance and body
/// are reclaimed, in seconds.
const PENDING_TIMEOUT: f32 = 5.0;

/// How often live instances are tested against the camera's view, in seconds.
const SWEEP_PERIOD: f32 = 0.5;

/// How often the ball flow reports its own status, in seconds.
const STATUS_PERIOD: f32 = 1.0;

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
    pub instance: Option<RemoteEntity>,
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
    instance: RemoteEntity,
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
    Show(RemoteEntity),
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
    instances: Pending<Entity, RemoteEntity>,
    /// The shows the render world owes: one request per ball, asked under the
    /// ball whose instance is being shown. What it answers is whether the
    /// instance really wears its assets.
    shows: Pending<Entity, bool>,
    /// What a sweep swept away: one request per sweep, asked under no key at
    /// all, because what it names is the whole scene rather than one ball.
    retired: Pending<(), Vec<BallInstance>>,
    /// Where the answers land, reused across ticks.
    settled_bodies: Vec<(Entity, Settled<RigidBodyHandle>)>,
    settled_instances: Vec<(Entity, Settled<RemoteEntity>)>,
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
    /// Seconds since the previous automatic drop.
    spawn_elapsed: f32,
    /// Seconds since the previous view sweep.
    sweep_elapsed: f32,
    /// Seconds since the previous status line.
    status_elapsed: f32,
    /// Whether the spawn cadence drops balls without a panel request.
    pub automatic_drops: bool,
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
    orphans: Vec<RemoteEntity>,
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
            bodies: Pending::new(scene.physics_state.clone(), Some(PENDING_TIMEOUT)),
            instances: Pending::new(scene.render_state.clone(), Some(PENDING_TIMEOUT)),
            shows: Pending::new(scene.render_state.clone(), Some(PENDING_TIMEOUT)),
            retired: Pending::new(scene.render_state.clone(), Some(PENDING_TIMEOUT)),
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
            spawn_elapsed: 0.0,
            sweep_elapsed: 0.0,
            status_elapsed: 0.0,
            automatic_drops: true,
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
}

mod drop;
mod fold;
mod sweep;

/// The balls and the tick that drops, poses and sweeps them, spawned together.
///
/// The balls hold no world of their own: every instance, body and pose is
/// asked of the worlds the tick is handed, so a state spawned without its
/// tick would hold streams nobody reads.
///
/// The tick reads the scene's assets, which the build in the default band
/// writes, so it states [`BALLS_ORDER`](super::BALLS_ORDER): two behaviours in
/// one band have no order between them, and the read would find no assets.
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
            advancing_in(super::BALLS_ORDER, move |balls, world, frame| Self::tick(
                balls,
                world,
                &scene,
                frame.delta_time
            )),
        ]
    }
}

impl BallDrops {
    /// The balls for the worlds `scene` reaches.
    #[must_use]
    pub(super) fn new(scene: &Scene, automatic_drops: bool) -> Self {
        let mut balls = Balls::new(scene);
        balls.automatic_drops = automatic_drops;
        Self {
            balls,
            scene: scene.clone(),
        }
    }

    /// Advance the balls by one tick: fold the answers, drop, pose and sweep.
    fn tick(balls: &mut Balls, world: &World, scene: &Scene, delta_seconds: f32) {
        balls.spawn_elapsed += delta_seconds;
        balls.sweep_elapsed += delta_seconds;
        balls.status_elapsed += delta_seconds;
        let Some((state, ready)) = world
            .root_scope()
            .query::<(&SceneState, Entity)>()
            .iter()
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
        balls.fold_answers(world, state, scene, delta_seconds);
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
        if balls.automatic_drops && balls.spawn_elapsed >= SPAWN_PERIOD {
            balls.spawn_elapsed %= SPAWN_PERIOD;
            balls.drop_ball(world, state, scene);
        }

        balls.publish(world, scene);

        if balls.sweep_elapsed >= SWEEP_PERIOD {
            balls.sweep_elapsed %= SWEEP_PERIOD;
            balls.sweep_outside_view(world, scene);
        }
    }
}

/// What a tick does with a ball.
///
/// A ball is reclaimed when one of the requests it is waiting on was given up
/// on — it waited out `PENDING_TIMEOUT`, or a world that owed it an
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
fn live_poses(
    scope: &Scope<'_>,
    snapshot: &GamePhysics,
    poses: &mut Vec<(RemoteEntity, Transform)>,
) {
    for ball in scope.query::<&Ball>().iter() {
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
mod tests;
