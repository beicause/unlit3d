//! The glTF loader's tests: the documents they build, and what the loaded scene
//! and its animations do with them.

use super::*;
use crate::components::{MorphWeights, SkinPose};
use crate::gltf::material::{morph_target_count, samples_base_color_texture};
use unlit_ecs::World;
use unlit_ecs::bundle;

mod animation;
mod builders;
mod scene;

use self::builders::*;
