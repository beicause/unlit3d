//! The shape pool: the render instances the scene reuses, built once and
//! handed out a slot at a time.
//!
//! Building it is the one part of the scene that is asynchronous — meshes and
//! materials, then the instances that use them — so the build lands in a
//! shared slot the tick polls, and the objects that need a slot ask this one
//! for it rather than each keeping a copy.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{Entity, World};
use unlit3d::components::Transform;
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::HIDDEN_POSITION;
use super::shapes::{
    BALL_COLORS, PATTERN_SIZE, SHAPE_KINDS, ShapeKind, pattern_texture, shape_kind, shape_mesh,
};
use super::tick::Tick;
use crate::render::{create_material_from_texture, create_mesh, linear_sampler, spawn_instance};

/// How many balls the scene keeps around. The pool never grows: the build
/// requests the instances from the render world once, and a spawned ball
/// reuses the first free entity.
pub const POOL_SIZE: usize = 48;

/// The async build's outcome, filled by the build future and consumed by the
/// first tick after it lands.
enum BuildState {
    /// The build future is still in flight.
    Building,
    /// Every instance answered; the shape pool is live.
    Ready(Vec<(Entity, ShapeKind)>),
    /// The render world dropped a request; the scene stays empty.
    Failed,
}

/// The shape pool, living on its own entity in the gameplay world.
///
/// It owns every render entity the scene drops as a shape, and which of them
/// are free: an object that needs a shape asks for a slot and gives it back
/// when its body is gone, so no other object has to know what the pool holds.
pub struct SceneState {
    /// The render world's command channel: the build asks it for the
    /// instances, and the objects move them through it.
    render: JobSender,
    /// The build future's landing slot: it fills this, the tick reads it.
    build: Rc<RefCell<BuildState>>,
    /// Whether the build future still needs polling.
    build_pending: bool,
    /// Every render entity the pool owns, for a scene restart: the free
    /// slots are [`SceneState::free`], this list is all of them.
    slots: Vec<(Entity, ShapeKind)>,
    /// Render entities not yet assigned to a body.
    free: Vec<(Entity, ShapeKind)>,
    /// Whether the build landed and the pool is live.
    ready: bool,
}

impl SceneState {
    /// Start the pool's build against `server`.
    ///
    /// The scene is built by an async task of the gameplay world itself: it
    /// awaits the meshes and materials, then the instances that use them, and
    /// leaves the pool for the tick to pick up. The answer
    /// order matches the request order, which is how the build reads the
    /// entities back.
    pub fn new(world: &World, server: WorldHandle) -> Self {
        let build = Rc::new(RefCell::new(BuildState::Building));
        let slot = Rc::clone(&build);
        let render = server.sender();
        log::info!("the gameplay world started; requesting the scene assets from the render world");
        let spawned = LocalTasks::spawn(world, async move {
            let assets = request_shape_assets(&server);
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
            let instances = request_pool_instances(&server, &answers);
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
        });
        debug_assert!(spawned, "the gameplay world carries its own tasks");
        Self {
            render,
            build,
            build_pending: true,
            slots: Vec::new(),
            free: Vec::new(),
            ready: false,
        }
    }

    /// Whether the build landed and the pool is live.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Every render entity the pool owns, live or free.
    #[must_use]
    pub fn slots(&self) -> &[(Entity, ShapeKind)] {
        &self.slots
    }

    /// The render world's command channel, for the objects that move the
    /// instances.
    #[must_use]
    pub fn render(&self) -> &JobSender {
        &self.render
    }

    /// How many slots are free right now.
    #[must_use]
    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// Take the first free slot, or none when every slot is out.
    pub fn take_slot(&mut self) -> Option<(Entity, ShapeKind)> {
        self.free.pop()
    }

    /// Put one slot back, ready to be taken again.
    pub fn give(&mut self, slot: (Entity, ShapeKind)) {
        self.free.push(slot);
    }

    /// Replace the free set, after a clear decided which slots are still
    /// reserved by a request in flight.
    pub fn restore_free(&mut self, free: Vec<(Entity, ShapeKind)>) {
        self.free = free;
    }

    /// Take the pool once the build task has landed.
    ///
    /// The task itself is driven by the pump that opens each tick; this only
    /// reads what it left behind. The pool stays empty if the render world
    /// dropped a request.
    fn poll_build(&mut self) {
        if !self.build_pending {
            return;
        }
        if matches!(&*self.build.borrow(), BuildState::Building) {
            return;
        }
        self.build_pending = false;
        let landed = core::mem::replace(&mut *self.build.borrow_mut(), BuildState::Failed);
        match landed {
            BuildState::Ready(pool) => {
                self.slots = pool.clone();
                self.free = pool;
                self.ready = true;
                log::info!("the shape pool is live: {} bodies", self.free.len());
            }
            BuildState::Failed => {
                self.ready = false;
            }
            BuildState::Building => {}
        }
    }
}

impl Tick for SceneState {
    fn tick(&mut self, _world: &World, _frame: &Frame) {
        self.poll_build();
    }
}

/// Read the instance answers into the shape pool: one slot per answer, each
/// remembering the kind its index draws.
pub fn pool_from_instances(answers: Vec<Entity>) -> Vec<(Entity, ShapeKind)> {
    answers
        .into_iter()
        .enumerate()
        .map(|(index, slot)| (slot, shape_kind(index)))
        .collect()
}

/// Request the shapes' meshes and materials, in the order the build reads
/// their answers back: one mesh per shape kind, then one patterned material
/// per shape kind and color (kind-major).
pub fn request_shape_assets(render: &WorldHandle) -> Vec<Request<Entity>> {
    let mut requests =
        Vec::with_capacity(SHAPE_KINDS.len() + SHAPE_KINDS.len() * BALL_COLORS.len());
    for kind in SHAPE_KINDS {
        requests.push(render.request(create_mesh(shape_mesh(kind))));
    }
    for kind in SHAPE_KINDS {
        for color in BALL_COLORS {
            requests.push(render.request(create_material_from_texture(
                (PATTERN_SIZE, PATTERN_SIZE),
                pattern_texture(kind, color),
                linear_sampler(),
            )));
        }
    }
    requests
}

/// Request one instance per pool slot, reading the mesh and material
/// entities out of the answers [`request_shape_assets`] produced.
pub fn request_pool_instances(render: &WorldHandle, answers: &[Entity]) -> Vec<Request<Entity>> {
    let shape_count = SHAPE_KINDS.len();
    let materials = &answers[shape_count..];
    let mut instances = Vec::with_capacity(POOL_SIZE);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The build's instance answers map onto the pool in order: slot i wears
    /// the kind its index picks, so the slot list and the shape list stay in
    /// step.
    #[test]
    fn instance_answers_map_onto_the_pool() {
        let answers: Vec<Entity> = (0..POOL_SIZE as u32)
            .map(|index| Entity::from_raw(index, 0))
            .collect();
        let pool = pool_from_instances(answers);
        assert_eq!(pool.len(), POOL_SIZE);
        for (index, (slot, kind)) in pool.iter().enumerate() {
            assert_eq!(slot.index(), index as u32);
            assert_eq!(*kind, shape_kind(index));
        }
    }

    /// Every mesh a pool slot can wear is one of the shape kinds, so the
    /// pool's kind lookup never falls off the end.
    #[test]
    fn pool_kinds_cover_the_shapes() {
        let pool = pool_from_instances(
            (0..POOL_SIZE as u32)
                .map(|index| Entity::from_raw(index, 0))
                .collect(),
        );
        for (_, kind) in &pool {
            assert!(SHAPE_KINDS.contains(kind));
        }
    }
}
