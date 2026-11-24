//! The glTF loader's tests: the documents they build, and what the loaded scene
//! and its animations do with them.

use super::*;
use crate::gltf::material::{base_color_uv_set, morph_target_count};

mod animation;
mod builders;
pub(super) mod documents;
mod primitives;
mod scene;
mod validation;

use self::builders::*;
