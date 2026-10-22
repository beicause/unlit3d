//! The shapes: dropping them, mapping their bodies onto pool slots, and
//! publishing where they are.
//!
//! This is the scene's busiest object. It asks the pool for a slot, asks
//! physics for a body, and turns the physics snapshot into the poses the
//! render world draws. A slot it takes is its own until the body is gone, so
//! nothing else can hand the same render entity to a second body.

use std::collections::VecDeque;
use std::sync::Arc;

use glam::Vec3;
use hashbrown::HashMap;
use unlit_ecs::{Entity, World};
use unlit3d::components::Transform;
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::shapes::{ShapeKind, shape_body};
use super::shared::SceneState;
use super::tick::Tick;
use super::{GAME_HZ, GamePhysics, ScenePoses, SceneWiring, pose_job};

/// How long between automatic ball drops, in seconds.
const SPAWN_PERIOD: f64 = 0.7;

/// How many gameplay ticks pass between automatic ball drops.
pub const SPAWN_TICKS: u32 = (SPAWN_PERIOD * GAME_HZ) as u32;

/// How long a spawn request may stay unanswered, in seconds.
const PENDING_TIMEOUT: f64 = 5.0;

/// How many gameplay ticks a spawn request may stay unanswered before its
/// render entity returns to the pool.
pub const PENDING_TIMEOUT_TICKS: u32 = (PENDING_TIMEOUT * GAME_HZ) as u32;

/// Where balls spawn, in metres above the ground.
pub const SPAWN_HEIGHT: f32 = 7.0;

/// How far from the centre a ball may spawn, in metres.
pub const SPAWN_SPREAD: f32 = 3.0;

/// One body request in flight: its id, the pool slot its render entity
/// was reserved for, the tick the request was made, and the scene
/// generation it belongs to.
type PendingBody = (u64, (Entity, ShapeKind), u32, u32);

/// One body answered by the physics world: the request it answers, the
/// generation it belongs to, and the body the world spawned.
type BodySpawned = (u64, u32, RigidBodyHandle);

/// The shapes, living on their own entity in the gameplay world.
///
/// A body's pose is authoritative in the physics world and drawn by the
/// render world, so this object is the one place the two meet: it maps the
/// handles the snapshot names onto the pool slots it took, and publishes the
/// resulting poses as a job.
pub struct Balls {
    /// The render world's command channel: the poses go to it as jobs, like
    /// every other change to that world.
    render: JobSender,
    /// The physics world's latest pose snapshot.
    physics: SnapshotReader<GamePhysics>,
    /// The physics world's command channel.
    physics_sender: JobSender,
    /// The answers the physics world pushes when a body is spawned, and
    /// this world's reader of them.
    spawns: Stream<BodySpawned>,
    spawned: StreamReader<BodySpawned>,
    /// The id the next body request is given. It is wide enough never to
    /// come back around: an answer to a request that already timed out must
    /// not be mistaken for a later request's.
    next_request: u64,
    /// Body requests in flight, with the render entity reserved, the tick
    /// the request was made, and the generation it belongs to: a request
    /// from before a restart is despawned instead of mapped when it
    /// answers.
    ///
    /// These requests are polled from the tick rather than awaited in a
    /// future: answering one mutates the shape state (the handle map, the
    /// slot it reserved, the rng), and a future cannot hold `&mut Balls`
    /// across an await; a clear cancels in-flight requests by generation,
    /// which a pool of spawned futures could only do with abort handles it
    /// does not have; and the timeout below counts ticks, not wall time,
    /// which keeps the headless snapshot deterministic regardless of thread
    /// timing.
    pending: Vec<PendingBody>,
    /// Body handle to render entity, for bodies in the snapshot.
    by_handle: HashMap<RigidBodyHandle, Entity>,
    /// The live bodies in the order they were mapped, oldest first, each
    /// with the render slot it drives: a full pool recycles the oldest
    /// instead of refusing the drop.
    live: VecDeque<(RigidBodyHandle, (Entity, ShapeKind))>,
    /// A clear requested but not yet consumed by the tick.
    clear: bool,
    /// Bumped on every clear, so stale requests are told apart.
    generation: u32,
    /// The snapshot the previous tick published, to skip repeated work.
    last_snapshot: Option<Arc<GamePhysics>>,
    /// Ball spawns requested but not yet consumed by the tick.
    requests: u32,
    /// Ticks since the world started, driving the auto spawn.
    ticks: u32,
    /// Pose entries the last job carried, for the status line.
    poses_sent: usize,
    /// The xorshift state for spawn placement.
    rng: u64,
}

impl Balls {
    /// The shapes, driving the worlds `wiring` reaches.
    #[must_use]
    pub fn new(wiring: &SceneWiring) -> Self {
        let spawns = Stream::new();
        let spawned = spawns.subscribe();
        Self {
            render: wiring.render_sender.clone(),
            physics: wiring.physics_reader.clone(),
            physics_sender: wiring.physics_sender.clone(),
            spawns,
            spawned,
            next_request: 0,
            pending: Vec::new(),
            by_handle: HashMap::new(),
            live: VecDeque::new(),
            clear: false,
            generation: 0,
            last_snapshot: None,
            requests: 0,
            ticks: 0,
            poses_sent: 0,
            rng: 0x853c49e6748fea9b,
        }
    }

    /// Ask for one more shape on the next tick.
    pub fn request_drop(&mut self) {
        self.requests += 1;
    }

    /// Ask the next tick to put every shape back in the pool.
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

    /// Request one body from the physics world, reserving a pool slot now so
    /// two in-flight requests cannot both claim the same render entity. The
    /// reserved slot's kind picks the collider shape.
    ///
    /// A full pool recycles its oldest live shape, so a drop always answers.
    /// The recycled body is despawned and its mapping forgotten here,
    /// because a snapshot already in flight may still name it.
    fn drop_ball(&mut self, world: &World, state: Entity) {
        let (slot, kind) = match world
            .root_scope()
            .query_one::<&mut SceneState>(state)
            .and_then(|mut state| state.take_slot())
        {
            Some(slot) => slot,
            None => {
                let Some((handle, slot)) = self.live.pop_front() else {
                    return;
                };
                self.physics_sender
                    .send(Box::new(move |world: &mut World| {
                        let _ = despawn_body(world, handle);
                    }))
                    .ok();
                self.by_handle.remove(&handle);
                slot
            }
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
        // The answer comes back over a stream rather than a reply slot: the
        // physics world pushes it when it runs the job, and the tick below
        // folds whatever arrived into its own state.
        let id = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        let born = self.generation;
        let spawns = self.spawns.clone();
        let sent = self.physics_sender.send(Box::new(move |world: &mut World| {
            let handle = spawn_body(world, description);
            let _ = spawns.push((id, born, handle));
        }));
        if let Err(error) = sent {
            log::warn!("the physics world refused a body request ({error:?})");
            let _ = world
                .root_scope()
                .query_one::<&mut SceneState>(state)
                .map(|mut state| state.give((slot, kind)));
            return;
        }
        self.pending
            .push((id, (slot, kind), self.ticks, self.generation));
        log::debug!("a body was requested at {position:?}");
    }

    /// Put every live body back in the pool: the shapes clear without
    /// rebuilding anything, so the same render entities drop again. The
    /// music is left alone — the panel drives it, not the shapes. Requests
    /// still in flight are despawned when they answer, using the generation
    /// bump.
    fn clear_shapes(&mut self, world: &World, state: Entity) {
        self.generation = self.generation.wrapping_add(1);
        let live = self.live.len();
        for (handle, _) in self.live.drain(..) {
            self.physics_sender
                .send(Box::new(move |world: &mut World| {
                    let _ = despawn_body(world, handle);
                }))
                .ok();
        }
        self.by_handle.clear();
        // A clear also cancels the drops the panel asked for but the tick
        // has not spent yet: they would repopulate the scene immediately.
        self.requests = 0;
        let free = world
            .root_scope()
            .query_one::<&mut SceneState>(state)
            .map(|mut scene_state| {
                let free = available_slots(scene_state.slots(), &self.pending);
                scene_state.restore_free(free);
            })
            .is_some();
        debug_assert!(free, "the pool lives on its own entity");
        self.last_snapshot = None;
        self.poses_sent = 0;
        log::info!(
            "the shapes were cleared: {live} bodies despawned, {} slots free",
            world
                .root_scope()
                .query_one::<&SceneState>(state)
                .map_or(0, |state| state.free_count())
        );
    }

    /// Map the bodies the physics world spawned since the last tick, then
    /// retire the requests it either ran long ago or never will: the world
    /// died, and the reservation must not leak. A request from before a
    /// restart is despawned instead of mapped, so its slot returns to the
    /// pool.
    fn retire_stale(&mut self, world: &World, state: Entity) {
        let generation = self.generation;
        let ticks = self.ticks;
        let physics = &self.physics_sender;
        self.spawned.pump();
        let mut answers = Vec::new();
        self.spawned.copy_into(&mut answers);
        for (id, born, handle) in answers {
            // A request that already timed out has no entry left; its body
            // still exists, so it has to be despawned rather than leaked.
            let Some(position) = self.pending.iter().position(|(pending, ..)| *pending == id)
            else {
                physics
                    .send(Box::new(move |world: &mut World| {
                        let _ = despawn_body(world, handle);
                    }))
                    .ok();
                continue;
            };
            let (_, (slot, kind), _, _) = self.pending.swap_remove(position);
            if born == generation {
                self.by_handle.insert(handle, slot);
                self.live.push_back((handle, (slot, kind)));
            } else {
                physics
                    .send(Box::new(move |world: &mut World| {
                        let _ = despawn_body(world, handle);
                    }))
                    .ok();
                let _ = world
                    .root_scope()
                    .query_one::<&mut SceneState>(state)
                    .map(|mut state| state.give((slot, kind)));
            }
        }
        self.pending.retain(|(_, (slot, kind), age, _)| {
            if ticks - *age > PENDING_TIMEOUT_TICKS {
                let _ = world
                    .root_scope()
                    .query_one::<&mut SceneState>(state)
                    .map(|mut state| state.give((*slot, *kind)));
                return false;
            }
            true
        });
    }

    /// Publish the snapshot's poses onto every pool slot, the live bodies at
    /// their body's pose and the rest parked.
    fn publish(&mut self, world: &World, state: Entity) {
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
                // The posed count goes out too: a mismatch against the
                // visible transforms means the pose job, not the flow,
                // stalls.
                self.poses_sent,
            );
        }

        // The render pose set: every pool slot, holding its body's pose or
        // the hidden position.
        let mut poses: HashMap<Entity, Transform> =
            HashMap::with_capacity(snapshot.transforms.len());
        for (handle, translation, rotation) in snapshot.transforms.iter() {
            if let Some(slot) = self.by_handle.get(handle) {
                poses.entry(*slot).or_insert(Transform {
                    translation: *translation,
                    rotation: *rotation,
                    ..Default::default()
                });
            }
        }
        self.poses_sent = poses.len();
        let frame = world
            .root_scope()
            .query_one::<&mut SceneState>(state)
            .map(|state| scene_frame(state.slots(), &poses))
            .unwrap_or_default();
        // The poses travel as a job: what the render world shows is its
        // own to hold, so the shapes send the state rather than leaving it
        // in a mailbox for the world to apply.
        match self.render.send(pose_job(frame)) {
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

        // A clear first: every body goes back, and the tick keeps the
        // same pool.
        if core::mem::take(&mut self.clear) {
            self.clear_shapes(world, state);
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

        self.publish(world, state);
    }
}

/// The slots a clear hands back: every slot except the ones an in-flight
/// request reserved. A reserved slot returns when its body is despawned
/// instead, so handing it back here too would sell one render entity to
/// two bodies.
fn available_slots(
    slots: &[(Entity, ShapeKind)],
    pending: &[PendingBody],
) -> Vec<(Entity, ShapeKind)> {
    slots
        .iter()
        .copied()
        .filter(|slot| !pending.iter().any(|(_, reserved, ..)| reserved == slot))
        .collect()
}

/// The poses a tick publishes: every pool slot named, each either at its
/// body's pose or parked at the hidden position.
///
/// A reader only moves the entities it names, so a publish that lists just
/// the live bodies leaves every other entity standing where it last was: a
/// despawn — a clear, or a body that timed out — would be invisible until
/// its slot dropped again. Naming every slot makes each publish a complete
/// picture of the scene, and one that any reader can apply on its own.
pub fn scene_frame(
    slots: &[(Entity, ShapeKind)],
    poses: &HashMap<Entity, Transform>,
) -> ScenePoses {
    ScenePoses(
        slots
            .iter()
            .map(|(slot, _)| {
                let pose = poses.get(slot).cloned().unwrap_or(Transform {
                    translation: super::HIDDEN_POSITION,
                    ..Default::default()
                });
                (*slot, pose)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::HIDDEN_POSITION;

    /// A clear hands back every slot except the ones a request still in
    /// flight reserved: returning those would sell one render entity to two
    /// bodies.
    #[test]
    fn a_clear_returns_only_unreserved_slots() {
        let slots: Vec<(Entity, ShapeKind)> = (0..4)
            .map(|index| (Entity::from_raw(index, 0), ShapeKind::Ball))
            .collect();
        // A request the physics world has not answered yet holds slot 2.
        let pending: Vec<PendingBody> = vec![(0, slots[2], 0, 0)];
        let free = available_slots(&slots, &pending);
        assert_eq!(
            free.iter()
                .map(|(slot, _)| slot.index())
                .collect::<Vec<_>>(),
            vec![0, 1, 3],
            "the reserved slot must not come back while its request is in flight",
        );
        // Nothing in flight returns the whole pool.
        assert_eq!(available_slots(&slots, &[]).len(), slots.len());
    }

    /// Every publish names every slot: the ones with a live body at their
    /// pose, the rest parked. A publish that merely omitted the despawned
    /// bodies would leave them standing where they were, and a clear nobody
    /// can see is no clear at all.
    #[test]
    fn a_frame_names_every_slot() {
        let slots: Vec<(Entity, ShapeKind)> = (0..4)
            .map(|index| (Entity::from_raw(index, 0), ShapeKind::Cube))
            .collect();
        let pose = Vec3::new(1.0, 2.0, 3.0);
        let mut poses = HashMap::new();
        poses.insert(
            slots[2].0,
            Transform {
                translation: pose,
                ..Default::default()
            },
        );
        let frame = scene_frame(&slots, &poses);
        assert_eq!(frame.0.len(), slots.len());
        for (slot, transform) in &frame.0 {
            assert!(slots.iter().any(|(known, _)| known == slot));
            let expected = if *slot == slots[2].0 {
                pose
            } else {
                HIDDEN_POSITION
            };
            assert_eq!(
                transform.translation,
                expected,
                "slot {} carries the wrong pose",
                slot.index()
            );
            assert_eq!(transform.rotation, glam::Quat::IDENTITY);
            assert_eq!(transform.scale, Vec3::ONE);
        }
        // An empty pose map is the cleared scene: every slot parked.
        let cleared = scene_frame(&slots, &HashMap::new());
        assert!(
            cleared
                .0
                .iter()
                .all(|(_, pose)| pose.translation == HIDDEN_POSITION)
        );
    }
}
