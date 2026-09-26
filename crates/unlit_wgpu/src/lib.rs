#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

// The WESL shader package is generated at build time from `shaders/*.wesl`;
// the `unlit` feature decides whether the built-in entry shader is one of the
// modules it bundles.
wesl_core::wesl_pkg!(
    #[doc = "The WESL modules of the built-in shader package."]
    #[expect(
        missing_docs,
        reason = "the package is generated from `shaders/*.wesl`; its items carry no docs of their own"
    )]
    pub shader,
    "unlit_wgpu.rs"
);

pub mod buffer_pool;
pub mod globals;
pub mod mesh;
pub mod offset_allocator;
pub mod pipeline;
pub mod render_attachments;
pub mod resources;
pub mod scene;
pub mod specialize;
pub mod staging;
#[cfg(feature = "egui")]
pub mod ui;
pub mod util;
pub mod vertex_pool;
