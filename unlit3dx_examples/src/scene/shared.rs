//! The shape pool: the render instances the scene reuses, built once and
//! handed out a slot at a time.
//!
//! Building it happens in the render world, in one job: meshes and materials,
//! then the instances that name them, so nothing waits for an answer in
//! between. The job publishes the instances it built, and the tick here takes
//! them up when they land.

use unlit_ecs::{Entity, World};
use unlit3d::components::Transform;
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::HIDDEN_POSITION;
use super::shapes::{
    BALL_COLORS, PATTERN_SIZE, SHAPE_KINDS, ShapeKind, pattern_texture, shape_kind, shape_mesh,
};
use super::tick::Tick;
use crate::render::{allocate_mesh, allocate_rgba_material, instance_of, linear_sampler};

/// How many balls the scene keeps around. The pool never grows: the build
/// makes the instances once, and a spawned ball reuses the first free entity.
pub const POOL_SIZE: usize = 48;

/// The shape pool, living on its own entity in the gameplay world.
///
/// It owns every render entity the scene drops as a shape, and which of them
/// are free: an object that needs a shape asks for a slot and gives it back
/// when its body is gone, so no other object has to know what the pool holds.
pub struct SceneState {
    /// The render world's command channel: the objects move their instances
    /// through it.
    render: JobSender,
    /// The instances the build job published, read once they land.
    instances: SnapshotReader<Vec<Entity>>,
    /// Every render entity the pool owns, for a scene restart: the free
    /// slots are [`SceneState::free`], this list is all of them.
    slots: Vec<(Entity, ShapeKind)>,
    /// Render entities not yet assigned to a body.
    free: Vec<(Entity, ShapeKind)>,
    /// Whether the build landed and the pool is live.
    ready: bool,
}

impl SceneState {
    /// Ask the render world for the pool.
    ///
    /// The whole build is one job there: it allocates the meshes and
    /// materials, then the instances that name them, and publishes the
    /// instance entities in pool order. Until that lands the pool is empty
    /// and every object that needs a shape stays unspawned.
    pub fn new(server: WorldHandle) -> Self {
        let instances = Snapshot::new();
        // Subscribe before the job is queued: a snapshot keeps nothing until
        // a reader exists, and the render world may run the build before this
        // constructor returns.
        let reader = instances.reader();
        let render = server.sender();
        log::info!("the gameplay world started; asking the render world for the scene");
        if let Err(error) = render.send(build_pool_job(instances)) {
            log::error!(
                "the render world refused the scene build ({error:?}); the scene stays empty"
            );
        }
        Self {
            render,
            instances: reader,
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

    /// Take the pool once the build job has published it.
    ///
    /// The pool stays empty if the render world refused the job.
    fn poll_build(&mut self) {
        if self.ready {
            return;
        }
        let Some(instances) = self.instances.latest() else {
            return;
        };
        let pool = pool_from_instances(instances.as_ref().clone());
        self.slots = pool.clone();
        self.free = pool;
        self.ready = true;
        log::info!("the shape pool is live: {} bodies", self.free.len());
    }
}

impl Tick for SceneState {
    fn tick(&mut self, _world: &World, _frame: &Frame) {
        self.poll_build();
    }
}

/// The job that builds the whole pool in the render world.
///
/// One step, so the instances can name meshes and materials this same job
/// spawned. The entities go back through `instances`, in pool order.
pub fn build_pool_job(instances: Snapshot<Vec<Entity>>) -> Job<()> {
    Box::new(move |world: &mut World| {
        let mut meshes = Vec::with_capacity(SHAPE_KINDS.len());
        for kind in SHAPE_KINDS {
            meshes.push(allocate_mesh(world, &shape_mesh(kind)));
        }
        // One patterned material per shape kind and color, kind-major.
        let mut materials = Vec::with_capacity(SHAPE_KINDS.len() * BALL_COLORS.len());
        for kind in SHAPE_KINDS {
            for color in BALL_COLORS {
                materials.push(allocate_rgba_material(
                    world,
                    (PATTERN_SIZE, PATTERN_SIZE),
                    &pattern_texture(kind, color),
                    linear_sampler(),
                ));
            }
        }
        let mut pool = Vec::with_capacity(POOL_SIZE);
        // Pool slot i wears the shape and tint its index picks, matching the
        // pool's (entity, kind) pairs.
        for index in 0..POOL_SIZE {
            let kind = index % SHAPE_KINDS.len();
            // The tint walks a diagonal over the pool, so the slots a drop
            // pops back to back — a run of one kind — still carry every color
            // between them.
            let color = (index + index / SHAPE_KINDS.len()) % BALL_COLORS.len();
            pool.push(instance_of(
                world,
                meshes[kind],
                materials[kind * BALL_COLORS.len() + color],
                Transform {
                    translation: HIDDEN_POSITION,
                    ..Default::default()
                },
            ));
        }
        instances.publish(pool);
    })
}

/// Read the instances the build published into the shape pool: one slot per
/// instance, each remembering the kind its index draws.
pub fn pool_from_instances(instances: Vec<Entity>) -> Vec<(Entity, ShapeKind)> {
    instances
        .into_iter()
        .enumerate()
        .map(|(index, slot)| (slot, shape_kind(index)))
        .collect()
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
