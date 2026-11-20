//! Unit tests for the KHR_draco_mesh_compression import path.
//!
//! A Draco bitstream is synthesised in memory with the decoder's companion
//! encoder and wrapped in a GLB whose primitive accessors deliberately carry no
//! buffer views (their data lives in the stream). The loader decodes the stream
//! back into ordinary accessors and validates the repaired document, so the
//! tests inspect the resulting streams through the same accessor reader an
//! uncompressed primitive uses. A committed, self-contained fixture covers
//! loading from disk without writing any file. No GPU is involved: the streams
//! are inspected before they would reach `allocate_lit_mesh`.

mod assets;
mod colors;
mod fixture;
mod repair;
mod streams;
