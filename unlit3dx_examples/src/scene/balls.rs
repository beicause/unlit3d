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
use crate::render::{despawn_instances_job, show_instance_job, sweep_instances_job};

/// How long between automatic ball drops, in seconds.
const SPAWN_PERIOD: f64 = 0.7;

/// How many gameplay ticks pass between automatic ball drops.
pub const SPAWN_TICKS: u32 = (SPAWN_PERIOD * GAME_HZ) as u32;

/// How long a ball may wait for its two halves, in seconds.
const PENDING_TIMEOUT: f64 = 5.0;

/// How many gameplay ticks a ball may wait for its two halves before its
/// instance and body are reclaimed.
pub const PENDING_TIMEOUT_TICKS: u32 = (PENDING_TIMEOUT * GAME_HZ) as u32;

/// How often live instances are tested against the camera's view, in seconds.
const SWEEP_PERIOD: f64 = 0.5;

/// How many gameplay ticks pass between view sweeps.
const SWEEP_TICKS: u32 = (SWEEP_PERIOD * GAME_HZ) as u32;

/// Where balls spawn, in metres above the ground.
pub const SPAWN_HEIGHT: f32 = 7.0;

/// How far from the centre a ball may spawn, in metres.
pub const SPAWN_SPREAD: f32 = 3.0;

/// One ball, on an entity of its own in the gameplay world.
///
/// The entity is the ball's identity: both answers name it, so a ball needs no
/// id of its own and no table to be looked up in. A ball still waiting for a
/// half has `None` where that half will be.
#[derive(Clone, Debug)]
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
    /// Whether the instance has been shown at the ball's spawn transform.
    pub shown: bool,
    /// Where the ball starts, which is where its instance is shown.
    pub transform: Transform,
}

/// One body answered by the physics world: the ball it belongs to and the body
/// the world spawned.
type BodySpawned = (Entity, RigidBodyHandle);

/// One instance answered by the render world: the ball it belongs to and the
/// instance the world spawned.
type InstanceCreated = (Entity, Entity);

/// One instance the render world despawned because the camera lost sight of
/// it, with the ball it belonged to.
type InstanceRetired = (Entity, Entity);

/// The balls, living on their own entity in the gameplay world.
///
/// A body's pose is authoritative in the physics world and drawn by the render
/// world, so this object is the one place the two meet: it reads the handles
/// the snapshot names off the ball entities, and publishes the resulting poses
/// as a job.
///
/// It reaches the other worlds through the `Scene` its tick is handed, so it
/// keeps no channel of its own: what it does keep is the answers waiting to be
/// folded into the balls they name.
pub struct Balls {
    /// The answers the physics world publishes when a body is spawned, and
    /// this world's reader of them.
    spawns: Stream<BodySpawned>,
    spawned: StreamReader<BodySpawned>,
    /// The answers the render world publishes when it creates an instance,
    /// and this world's reader of them.
    creates: Stream<InstanceCreated>,
    created: StreamReader<InstanceCreated>,
    /// The instances the render world despawned because the camera lost sight
    /// of them, and this world's reader of them.
    retired: Stream<InstanceRetired>,
    retired_instances: StreamReader<InstanceRetired>,
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
    created_answers: Vec<InstanceCreated>,
    /// Physics answers reused across ticks.
    spawned_answers: Vec<BodySpawned>,
    /// Sweep answers reused across ticks.
    retired_answers: Vec<InstanceRetired>,
    /// The bodies and instances a tick has to send away, reused across ticks
    /// so a tick that retires nothing allocates nothing.
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
        let retired = Stream::new();
        let retired_instances = retired.subscribe();
        Self {
            spawns,
            spawned,
            creates,
            created,
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
            retired_answers: Vec::new(),
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
            spawns.publish((ball, handle));
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
    fn clear_shapes(&mut self, world: &World, scene: &Scene) {
        let scope = world.root_scope();
        let mut handles = Vec::new();
        let mut instances = Vec::new();
        let mut cleared = 0;
        for (ball, entity) in scope.query::<(&Ball, Entity)>() {
            if let Some(handle) = ball.body {
                handles.push(handle);
            }
            if let Some(instance) = ball.instance {
                instances.push(instance);
            }
            scope.queue().despawn(entity);
            cleared += 1;
        }
        // A clear also cancels the drops the panel asked for but the tick has
        // not spent yet: they would repopulate the scene immediately.
        self.requests = 0;
        self.despawn_bodies(scene, &mut handles);
        self.despawn_instances(scene, &mut instances);
        self.last_snapshot = None;
        self.poses_sent = 0;
        log::info!("the balls were cleared: {cleared} despawned");
    }

    /// Fold the answers the other worlds sent since the last tick into the
    /// balls they name, show the ones that now have both halves, and put away
    /// the ones that waited too long.
    fn fold_answers(&mut self, world: &World, state: Entity, scene: &Scene) {
        // The buffers outlive the tick: a tick that folds nothing pays
        // nothing, and the one that does hands the filled ones away.
        let mut despawns = std::mem::take(&mut self.despawns);
        let mut instances = std::mem::take(&mut self.instances);
        despawns.clear();
        instances.clear();
        let scope = world.root_scope();

        // A retired instance is already gone from the render world: only its
        // body and its ball entity are left to despawn.
        let mut retired = std::mem::take(&mut self.retired_answers);
        retired.clear();
        self.retired_instances.pump();
        self.retired_instances.copy_into(&mut retired);
        for (_instance, ball) in retired.drain(..) {
            if let Some(handle) = scope.query_one::<&Ball>(ball).and_then(|found| found.body) {
                despawns.push(handle);
            }
            scope.queue().despawn(ball);
        }
        self.retired_answers = retired;

        // An instance whose ball is already gone has nobody to be shown on,
        // so it is despawned rather than leaked.
        let mut created = std::mem::take(&mut self.created_answers);
        created.clear();
        self.created.pump();
        self.created.copy_into(&mut created);
        for (ball, instance) in created.drain(..) {
            match scope.query_one::<&mut Ball>(ball) {
                Some(mut ball) => ball.instance = Some(instance),
                None => instances.push(instance),
            }
        }
        self.created_answers = created;

        let mut spawned = std::mem::take(&mut self.spawned_answers);
        spawned.clear();
        self.spawned.pump();
        self.spawned.copy_into(&mut spawned);
        for (ball, handle) in spawned.drain(..) {
            match scope.query_one::<&mut Ball>(ball) {
                Some(mut ball) => ball.body = Some(handle),
                None => despawns.push(handle),
            }
        }
        self.spawned_answers = spawned;

        // A ball whose two halves both landed is ready to show; one that has
        // waited too long, or whose world ended and will never answer, is put
        // away instead of being waited on forever.
        let ticks = self.ticks;
        let stale = world_ended(&scene.physics_state) || world_ended(&scene.render_state);
        let mut shown = Vec::new();
        for (ball, entity) in scope.query::<(&Ball, Entity)>() {
            if ball.shown {
                continue;
            }
            let (Some(handle), Some(instance)) = (ball.body, ball.instance) else {
                if stale || ticks.saturating_sub(ball.requested_at) > PENDING_TIMEOUT_TICKS {
                    if let Some(handle) = ball.body {
                        despawns.push(handle);
                    }
                    if let Some(instance) = ball.instance {
                        instances.push(instance);
                    }
                    scope.queue().despawn(entity);
                }
                continue;
            };
            shown.push((
                entity,
                ball.kind,
                ball.color,
                instance,
                handle,
                ball.transform.clone(),
            ));
        }

        // The assets are read once for every ball that is ready to show.
        let assets = scope
            .query_one::<&SceneState>(state)
            .and_then(|state| state.loaded.clone());
        if let Some(assets) = assets {
            for (entity, kind, color, instance, handle, transform) in shown {
                let mesh = assets.meshes[kind];
                let material = assets.materials[kind * BALL_COLORS.len() + color];
                match scene
                    .render
                    .send(show_instance_job(instance, mesh, material, transform))
                {
                    Ok(()) => {
                        if let Some(mut ball) = scope.query_one::<&mut Ball>(entity) {
                            ball.shown = true;
                        }
                    }
                    // A closed queue is the scene going away, not a fault.
                    Err(SendError::Closed) => {}
                    Err(error) => {
                        log::warn!("the render world refused a show job ({error:?})");
                        despawns.push(handle);
                        instances.push(instance);
                        scope.queue().despawn(entity);
                    }
                }
            }
        }

        self.despawn_bodies(scene, &mut despawns);
        self.despawn_instances(scene, &mut instances);
        self.despawns = despawns;
        self.instances = instances;
    }

    /// Despawn the bodies it names, in one job.
    fn despawn_bodies(&self, scene: &Scene, despawns: &mut Vec<RigidBodyHandle>) {
        if despawns.is_empty() {
            return;
        }
        // The job takes the buffer with it: it runs in another world and
        // cannot give the allocation back.
        let handles = std::mem::take(despawns);
        let sent = scene.physics.send(Box::new(move |world: &mut World| {
            for handle in handles {
                let _ = despawn_body(world, handle);
            }
        }));
        if let Err(error) = sent {
            log::warn!("the physics world refused a despawn job ({error:?})");
        }
    }

    /// Despawn the instances it names, in one job.
    fn despawn_instances(&self, scene: &Scene, instances: &mut Vec<Entity>) {
        if instances.is_empty() {
            return;
        }
        let instances = std::mem::take(instances);
        if let Err(error) = scene.render.send(despawn_instances_job(instances)) {
            log::warn!("the render world refused a despawn job ({error:?})");
        }
    }

    /// Ask the render world to despawn the shown instances the camera can no
    /// longer see. Each answer names the ball it belonged to, so the body and
    /// the ball entity go with the instance.
    fn sweep_outside_view(&mut self, world: &World, scene: &Scene) {
        let mut instances = Vec::new();
        for (ball, entity) in world.root_scope().query::<(&Ball, Entity)>() {
            if !ball.shown {
                continue;
            }
            if let Some(instance) = ball.instance {
                instances.push((instance, entity));
            }
        }
        if instances.is_empty() {
            return;
        }
        let retired = self.retired.clone();
        if let Err(error) = scene.render.send(sweep_instances_job(instances, retired)) {
            log::warn!("the render world refused a sweep job ({error:?})");
        }
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

        let poses = live_poses(&world.root_scope(), &snapshot);

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
            Err(error) => log::warn!("the render world refused a pose job ({error:?})"),
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

        // A clear first: every ball goes away, and its halves are cleaned up
        // as they arrive.
        if core::mem::take(&mut self.clear) {
            self.clear_shapes(world, scene);
        }

        self.fold_answers(world, state, scene);

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

/// The poses a tick publishes: one per shown ball, at the pose the physics
/// snapshot names for its body. A ball the snapshot has not named yet is left
/// out, keeping the transform the show job gave it.
///
/// A pose names the ball's render instance, not the ball entity: the job that
/// carries it runs in the render world, whose entities are its own. A ball
/// entity from this world happens to share a number with some render entity,
/// and writing a pose onto that one moves whatever it is.
fn live_poses(scope: &Scope<'_>, snapshot: &GamePhysics) -> Vec<(Entity, Transform)> {
    let mut poses = Vec::new();
    for ball in scope.query::<&Ball>() {
        if !ball.shown {
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
    poses
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
        let waiting = RigidBodyHandle::from_raw_parts(8, 0);
        let instance = Entity::from_raw(10, 0);
        {
            let mut scope = world.root_scope_mut();
            scope.spawn(bundle![ball(Some(instance), Some(handle), true)]);
        }
        {
            let mut scope = world.root_scope_mut();
            scope.spawn(bundle![ball(
                Some(Entity::from_raw(11, 0)),
                Some(waiting),
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
            .insert(waiting, (Vec3::ZERO, Quat::IDENTITY));

        let poses = live_poses(&world.root_scope(), &snapshot);

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

        let poses = live_poses(&world.root_scope(), &snapshot);

        assert_eq!(poses.len(), 1);
        assert_eq!(poses[0].0, instance, "the pose names the render instance");
        assert_ne!(poses[0].0, ball, "the pose must not name the ball entity");
    }
}
