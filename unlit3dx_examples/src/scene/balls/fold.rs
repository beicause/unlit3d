//! Folding the answers back in: what a ball waits for, and when it gives up.

use super::*;

impl Balls {
    /// Fold the answers the other worlds sent since the last tick into the
    /// balls they name, show the ones that now have both halves, and put away
    /// the ones that waited too long.
    ///
    /// Every wait is bounded by the same timeout, so this is the one place a
    /// request is given up on: an answer that never came names its ball, and
    /// the ball is reclaimed with whatever halves it did receive.
    pub(super) fn fold_answers(
        &mut self,
        world: &World,
        state: Entity,
        scene: &Scene,
        delta_seconds: f32,
    ) {
        let scope = world.root_scope();

        // A ball whose request was given up on loses the halves it has, and
        // the ones still in flight are dropped when they arrive: nothing is
        // left to fill them into.
        self.expired.clear();

        // A retired instance is already gone from the render world: only its
        // body and its ball entity are left to despawn.
        self.retired
            .advance(delta_seconds, &mut self.settled_retired);
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
                    .and_then(|mut found| found.get().body)
                {
                    self.despawns.push(handle);
                }
                scope.queue().despawn(answer.ball);
            }
        }

        // An instance whose ball is already gone has nobody to be shown on,
        // so it is despawned rather than leaked.
        self.instances
            .advance(delta_seconds, &mut self.settled_instances);
        for (ball, answer) in self.settled_instances.drain(..) {
            match (scope.query_one::<&mut Ball>(ball), answer) {
                (Ok(mut found), Settled::Answered(instance)) => {
                    found.get().instance = Some(instance);
                }
                (Err(_), Settled::Answered(instance)) => self.orphans.push(instance),
                (_, Settled::Expired) => self.expired.push(ball),
            }
        }

        self.bodies.advance(delta_seconds, &mut self.settled_bodies);
        for (ball, answer) in self.settled_bodies.drain(..) {
            match (scope.query_one::<&mut Ball>(ball), answer) {
                (Ok(mut found), Settled::Answered(handle)) => {
                    found.get().body = Some(handle);
                }
                (Err(_), Settled::Answered(handle)) => self.despawns.push(handle),
                (_, Settled::Expired) => self.expired.push(ball),
            }
        }

        // The show answers once the instance really wears its assets, so a
        // show the render world never answered leaves the ball waiting instead
        // of marking it shown.
        self.shows.advance(delta_seconds, &mut self.settled_shows);
        for (ball, answer) in self.settled_shows.drain(..) {
            match (scope.query_one::<&mut Ball>(ball), answer) {
                (Ok(mut found), Settled::Answered(true)) => {
                    found.get().phase = BallPhase::Shown;
                }
                // A show answered no, or a ball that went away while its show
                // was in flight: the instance it wore has nobody left to
                // stand for.
                (Ok(mut found), Settled::Answered(false)) | (Ok(mut found), Settled::Expired) => {
                    if let Some(instance) = found.get().instance.take() {
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
        for (ball, entity) in scope.query::<(&mut Ball, Entity)>().iter() {
            match waiting(ball, entity, &self.expired, stale) {
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
            .and_then(|mut state| state.get().loaded.clone());
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
                            ball.get().phase = BallPhase::Requested;
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
}
