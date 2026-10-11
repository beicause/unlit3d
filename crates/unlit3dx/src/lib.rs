//! Multi-world extension for [unlit3d]: run the worlds a game is made of —
//! host, render, physics, audio, gameplay — as independent ECS worlds,
//! joined only by jobs and snapshots.
//!
//! [unlit3d]: https://docs.rs/unlit3d
//!
//! # The model
//!
//! Every world is a plain ECS `World` that lives on exactly one thread:
//!
//! - The *host* owns the window and the event loop, and reaches any work that
//!   must stay on the main thread — a render sub-world presenting to the swap
//!   chain — through a nested `World` component ([`spawn_sub_world`]).
//! - A *remote world* runs on a thread of its own, spawned with
//!   [`spawn_remote_world`]; the host entity returned carries a
//!   [`RemoteWorld`] proxy whose `send` hands it jobs.
//! - Only data flows back: a thread publishes its newest state as a snapshot
//!   through a [`SnapshotWriter`], and readers on other threads peek at it
//!   with a [`SnapshotReader`]. Nothing ever calls back into another world.
//!
//! Feature-gated modules build on that core: [`physics`] simulates with
//! rapier and publishes snapshots, [`audio`] mixes PCM sources into a cpal
//! output stream, and [`synth`] turns a soundfont file into one. The
//! unlit3dx examples crate shows all of them in one scene.
#![forbid(unsafe_code)]

#[cfg(feature = "audio")]
pub mod audio;
pub mod bridge;
pub mod channel;
pub mod host;
pub mod job;
#[cfg(feature = "physics")]
pub mod physics;
#[cfg(feature = "render")]
pub mod render;
#[cfg(feature = "synth")]
pub mod synth;
pub mod thread;
pub mod world;

/// The types most callers of the multi-world model need.
pub mod prelude {
    #[cfg(feature = "audio")]
    pub use crate::audio::{AudioConfig, PcmSource, PcmSourceFactory, spawn_audio_world};
    #[cfg(feature = "physics")]
    pub use crate::physics::{
        BallDesc, PHYSICS_HZ, PhysicsBody, PhysicsConfig, PhysicsInbox, PhysicsSnapshot,
        RigidBodyHandle, spawn_ball, spawn_fixed_cuboid, spawn_physics_world,
    };
    #[cfg(feature = "render")]
    pub use crate::render::{APPLY_TRANSFORMS_ORDER, TransformFrame, apply_transform_frames};
    #[cfg(feature = "synth")]
    pub use crate::synth::{
        SynthVoice, VoiceError, note_off_job, note_on_job, notes_off_all_job, synth_source,
    };
    pub use crate::{
        bridge::RenderEntity,
        channel::{SnapshotReader, SnapshotWriter, snapshot_pair},
        host::spawn_sub_world,
        job::{Job, Request},
        thread::{FixedClock, ThreadControl, WorldThread},
        world::{RemoteWorld, spawn_remote_world, spawn_remote_world_with},
    };
}
