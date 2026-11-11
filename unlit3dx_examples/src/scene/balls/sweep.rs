//! Cleaning up: despawning what left the view, and publishing the poses.

use super::*;

impl Balls {
    /// Despawn the bodies the tick has collected, in one job.
    ///
    /// A despawn is idempotent, so the list is kept until a send goes through:
    /// a full queue delays the cleanup by a tick instead of losing it.
    pub(super) fn despawn_bodies(&mut self, scene: &Scene) {
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
    pub(super) fn despawn_instances(&mut self, scene: &Scene) {
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
    pub(super) fn sweep_outside_view(&mut self, world: &World, scene: &Scene) {
        let mut sweeping = std::mem::take(&mut self.sweeping);
        sweeping.clear();
        for (ball, entity) in world.root_scope().query::<(&Ball, Entity)>().iter() {
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
    pub(super) fn publish(&mut self, world: &World, scene: &Scene) {
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
        // the three hides every ball. It keeps its own clock, so the line
        // stays once a second whatever the drop cadence is.
        if self.status_elapsed >= STATUS_PERIOD {
            self.status_elapsed %= STATUS_PERIOD;
            log::debug!(
                "shown balls: {}; snapshot bodies: {}; posed last tick: {}",
                world
                    .root_scope()
                    .query::<&Ball>()
                    .iter()
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
