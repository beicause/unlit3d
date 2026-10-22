//! The scene's assets, built once in the render world.
//!
//! The render world allocates the meshes and materials in one job and
//! publishes them. Every shape a drop needs is its own render instance, asked
//! for when the drop happens; an instance the camera can no longer see is
//! despawned by the render world, so nothing is kept hidden for reuse.

use unlit_ecs::{Entity, World};
use unlit3d::components::Transform;
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::HIDDEN_POSITION;
use super::shapes::{BALL_COLORS, PATTERN_SIZE, SHAPE_KINDS, pattern_texture, shape_mesh};
use super::tick::Tick;
use crate::render::{allocate_mesh, allocate_rgba_material, hidden_instance_of, linear_sampler};

/// The render entities a shape is built from, in kind-major order.
///
/// The build job allocates them in the render world and publishes them here;
/// the gameplay world names them when it asks for an instance.
#[derive(Clone, Debug)]
pub struct SceneAssets {
    /// One mesh per [`SHAPE_KINDS`] entry, in that order.
    pub meshes: Vec<Entity>,
    /// One material per shape kind and color, kind-major:
    /// `kind * BALL_COLORS.len() + color`.
    pub materials: Vec<Entity>,
}

/// The scene's assets, on its own entity in the gameplay world.
///
/// It owns the render entities every shape is built from, and whether the
/// build has landed. It does not own instances: a drop asks the render world
/// for one, and the render world despawns it once the camera loses sight of
/// it.
pub struct SceneState {
    /// The assets the build job published, read once they land.
    assets: SnapshotReader<SceneAssets>,
    /// The assets once the build landed, for the jobs that name them.
    pub loaded: Option<SceneAssets>,
    /// Whether the build landed and the scene is live.
    ready: bool,
}

impl SceneState {
    /// Ask the render world for the scene's assets.
    ///
    /// The whole build is one job there: it allocates the meshes and the
    /// materials, and publishes their entities. Until that lands the scene is
    /// empty, and every object that needs a shape stays unspawned.
    pub fn new(server: WorldHandle) -> Self {
        let assets = Snapshot::new();
        log::info!("the gameplay world started; asking the render world for the scene");
        // The reader comes after the job is queued: a snapshot keeps the
        // published value, so the build cannot land where nobody will see it.
        if let Err(error) = server
            .commands
            .clone()
            .send(build_assets_job(assets.clone()))
        {
            log::error!(
                "the render world refused the scene build ({error:?}); the scene stays empty"
            );
        }
        Self {
            assets: assets.reader(),
            loaded: None,
            ready: false,
        }
    }

    /// Whether the build landed and the scene is live.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Take the assets once the build job has published them.
    ///
    /// The scene stays empty if the render world refused the job.
    fn poll_build(&mut self) {
        if self.ready {
            return;
        }
        let Some(assets) = self.assets.latest() else {
            return;
        };
        let assets = assets.as_ref().clone();
        log::info!(
            "the scene is live: {} meshes, {} materials",
            assets.meshes.len(),
            assets.materials.len()
        );
        self.loaded = Some(assets);
        self.ready = true;
    }
}

impl Tick for SceneState {
    fn tick(&mut self, _world: &World, _frame: &Frame) {
        self.poll_build();
    }
}

/// The job that allocates the scene's meshes and materials in the render
/// world.
///
/// One step, so nothing waits for an answer in between. The entities go back
/// through `assets`.
pub fn build_assets_job(assets: Snapshot<SceneAssets>) -> Job<()> {
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
        assets.publish(SceneAssets { meshes, materials });
    })
}

/// The job that spawns one hidden instance wearing the assets it names, and
/// pushes its entity back tagged with `id`.
///
/// It stays hidden until its body is ready and the show job puts it in place.
pub fn create_instance_job(
    id: u64,
    mesh: Entity,
    material: Entity,
    created: Stream<(u64, Entity)>,
) -> Job<()> {
    Box::new(move |world: &mut World| {
        let instance = hidden_instance_of(
            world,
            mesh,
            material,
            Transform {
                translation: HIDDEN_POSITION,
                ..Default::default()
            },
        );
        let _ = created.push((id, instance));
    })
}

/// The kind and color a drop wears, as indices into [`SceneAssets`].
///
/// The palette of every shape kind and color runs from its last entry down,
/// so a drop picks the last look first and cycles through the rest.
#[must_use]
pub fn look(index: usize) -> (usize, usize) {
    let looks = SHAPE_KINDS.len() * BALL_COLORS.len();
    let palette = looks - 1 - index % looks;
    (
        palette % SHAPE_KINDS.len(),
        (palette + palette / SHAPE_KINDS.len()) % BALL_COLORS.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One cycle of the palette covers every shape and color exactly once, so
    /// a scene that keeps dropping shapes never runs out of looks.
    #[test]
    fn looks_cover_the_palette() {
        let looks = SHAPE_KINDS.len() * BALL_COLORS.len();
        let mut seen = vec![false; looks];
        for index in 0..looks {
            let (kind, color) = look(index);
            assert!(kind < SHAPE_KINDS.len());
            assert!(color < BALL_COLORS.len());
            let at = kind * BALL_COLORS.len() + color;
            assert!(!seen[at], "look {at} repeats at index {index}");
            seen[at] = true;
        }
        assert!(seen.iter().all(|seen| *seen));
    }
}
