//! Dropping a ball and putting them all away: the two commands the panel sends.

use super::*;

impl Balls {
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
    pub(super) fn drop_ball(&mut self, world: &World, state: Entity, scene: &Scene) {
        let (kind, color) = look(self.drops as usize);
        self.drops = self.drops.wrapping_add(1);

        let selected = world
            .root_scope()
            .query_one::<&SceneState>(state)
            .ok()
            .and_then(|mut state| {
                state.get().loaded.as_ref().map(|assets| {
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
    pub(super) fn clear_balls(&mut self, world: &World, scene: &Scene) {
        let scope = world.root_scope();
        let mut cleared = 0;
        for (ball, entity) in scope.query::<(&mut Ball, Entity)>().iter() {
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
}
