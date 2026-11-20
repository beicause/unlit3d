//! The glTF loader's tests: the documents they build, and what the loaded scene
//! and its animations do with them.

use super::*;
use crate::components::{MorphWeights, SkinPose};
use crate::gltf::material::{base_color_uv_set, morph_target_count};
use unlit_ecs::World;
use unlit_ecs::bundle;

mod animation;
mod builders;
pub(super) mod documents;
mod primitives;
mod scene;

use self::builders::*;
