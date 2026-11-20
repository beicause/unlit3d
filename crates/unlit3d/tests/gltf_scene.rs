//! GPU coverage for the `gltf` feature: uploading a document's textures,
//! materials and meshes into an existing mesh source's graph, spawning the
//! default scene's nodes, rendering what those entities draw, and unloading
//! it all again.
//!
//! The documents are built in-memory as glTF JSON with a single data-URI
//! buffer holding the vertices, indices and a 4x4 PNG (rows red, green, blue,
//! white) as a buffer view, so `LitGltf::from_bytes` needs no base path.

use crate::common::{
    CLEAR, Ctx, HEIGHT, TestGpu, WIDTH, camera_looking_at, count_pixels_off_background,
};
use unlit_ecs::bundle;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::gltf::{AnimationId, GltfNode, GltfResources, LitGltf, MeshId, NodeId, PrimitiveId};
use unlit3d::prelude::*;

mod deform;
mod documents;
mod lighting;
mod materials;
mod scene;

use self::{
    deform::{
        a_morphed_quad_follows_its_weights, a_skinned_quad_follows_its_joint,
        an_animation_moves_the_drawn_mesh,
    },
    lighting::{a_plain_material_shades_by_the_lights, an_unlit_material_ignores_the_lights},
    materials::{
        a_blended_material_composites_over_the_frame, a_blended_material_writes_no_depth,
        a_double_sided_material_draws_back_faces,
        a_masked_material_discards_fragments_below_its_cutoff,
    },
    scene::{
        a_placement_moves_the_whole_document, node_hierarchy_translates_the_quad,
        textured_quad_renders_four_colours, unload_empties_the_frame,
        untextured_material_draws_flat,
    },
};

gpu_tests! {
    textured_quad_renders_four_colours,
    node_hierarchy_translates_the_quad,
    a_placement_moves_the_whole_document,
    untextured_material_draws_flat,
    a_blended_material_composites_over_the_frame,
    a_blended_material_writes_no_depth,
    a_masked_material_discards_fragments_below_its_cutoff,
    a_skinned_quad_follows_its_joint,
    an_animation_moves_the_drawn_mesh,
    a_morphed_quad_follows_its_weights,
    a_double_sided_material_draws_back_faces,
    an_unlit_material_ignores_the_lights,
    a_plain_material_shades_by_the_lights,
    unload_empties_the_frame,
}
