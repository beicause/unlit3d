//! `wgpu_unlit_render` — a compact, opinionated renderer for unlit draws on
//! WebGPU.
//!
//! The crate targets WebGPU (and the native backends behind it) with a
//! mobile-first bias; WebGL/GLES are not supported. It draws a whole scene —
//! opaque and transparent instances alike — in a single render pass, into one
//! [`wgpu::TextureView`] chosen by the caller.
//!
//! The crate is layered: the modules below are general facilities a caller
//! builds any pipeline on top of, and the built-in unlit pipeline — and the
//! egui backend that draws with it — are added by features.
//!
//! | Feature | Default | Provides |
//! |---------|---------|----------|
//! | `unlit` | yes | [`pipeline::UnlitPipeline`], [`pipeline::UnlitOptions`] and the WESL module they compose |
//! | `egui` | no | the `ui` module, an egui backend built on the unlit pipeline |
//!
//! # Layout
//!
//! - [`resources`]: a dependency-tracked graph of the GPU resources a frame
//!   uses. Resources are created, replaced and removed through it, and the
//!   graph propagates "dirty" state to dependents so that derived resources
//!   (bind groups, pipelines) are rebuilt lazily. It also holds virtual nodes
//!   that own no GPU resource, for grouping or standing in for others.
//! - [`mesh`]: vertex compression and the per-mesh decode metadata that makes
//!   the compact vertex formats usable in a shader.
//! - [`offset_allocator`]: a sub-allocator over one contiguous range, for
//!   packing many small ranges into a single GPU buffer.
//! - [`buffer_pool`] and [`vertex_pool`]: GPU buffers sub-allocated with it,
//!   so many meshes share one buffer instead of each owning its own.
//! - [`scene`]: the declarative description of a frame — pipelines, their
//!   bindings, materials, meshes and vertex buffers.
//! - [`render_attachments`]: the attachments a pass renders into and the
//!   pass-opening entry point.
//! - [`specialize`]: variant caching — a [`Specializable`](specialize::Specializable)
//!   value is compiled once per key and reused.
//! - [`pipeline`]: the binding slots, bind-group indices and vertex-buffer
//!   slots this crate draws with — plus, with the `unlit` feature, the
//!   built-in unlit pipeline and the WESL composition behind it.
//! - `ui`: an egui backend, with the `egui` feature.
//!
//! Nothing in the general modules knows about the built-in pipeline: it uses
//! the same binding and slot conventions, the same vertex compression and the
//! same resource tracking a caller’s own pipeline would.
//!
//! # Shaders
//!
//! The shaders this crate ships are authored in WESL and bundled at build
//! time; see [`shader`]. The package always carries the modules mirroring the
//! Rust types a caller binds (`globals`, `view`, `mesh_metadata` and the
//! `mesh_compression` decode functions); the `unlit` feature adds the
//! built-in entry shader, which [`pipeline::UnlitPipeline`] composes into the
//! variant [`pipeline::UnlitOptions`] selects.
//!
//! A caller who wants their own entry shader composes it directly with
//! [`wesl`](https://docs.rs/wesl): [`shader`] is a WESL `StaticPackage`, so
//! `wesl::resolver::PackageResolver` can resolve
//! `import wgpu_unlit_render::mesh_compression;` against the same modules the
//! built-in pipeline uses.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    feature = "unlit",
    doc = include_str!("unlit_example.md")
)]

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
    "wgpu_unlit_render.rs"
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
#[cfg(feature = "egui")]
pub mod ui;
pub mod util;
pub mod vertex_pool;
