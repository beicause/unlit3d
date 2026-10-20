//! The multi-world runtime: one `unlit_ecs` world per thread, jobs in,
//! snapshots back.
//!
//! `unlit_ecs`'s world is `!Send`: components live behind `RefCell` and
//! structural changes need `&mut World`, so a world cannot cross a thread.
//! This crate turns that constraint into the shape of a program:
//!
//! - A **remote world** runs its own world on a fresh thread, reached
//!   through the [`handle::WorldHandle`] its spawn returns. Jobs move in through a
//!   bounded channel and are drained between ticks; results come back
//!   through a [`job::Request`].
//! - A **sub-world** runs a nested world on the host's thread, drained
//!   with [`host::drain_sub_world`] between frames.
//! - **Snapshots** are the way back, and they are opt-in: a world runs a
//!   collector for every subscription it carries and publishes what each
//!   returns, so nothing is collected until someone asks. A subscriber
//!   reads the newest `T` through a [`channel::SnapshotReader`] and never
//!   blocks; dropping the reader unsubscribes.
//!
//! The runtime layer is small and complete: [`job`], [`channel`],
//! [`thread`], [`host`], [`world`], and [`handle`] together are the whole
//! low-level API, and a caller can build its own worlds on them. The
//! feature-gated modules are the convenience layer: `physics`, `audio`,
//! and `synth` wrap rapier, cpal, and a soundfont synthesizer in the same
//! shape, while `gpu` and `present` hold the window-facing half of a
//! multi-world app.
//!
//! Nothing here holds a lock across a job: a world is owned by one thread,
//! and every cross-world exchange is a channel. Reach for the low-level
//! modules directly when a world needs a shape the convenience layer does
//! not offer — `unlit3d`'s `MeshSource` is the generic pipeline registry
//! a caller can build its own render bridge on, for instance.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

#[cfg(feature = "audio")]
pub mod audio;
/// The snapshot subscriptions between worlds.
pub mod channel;
/// A single-threaded executor for awaiting batches of requests.
pub mod executor;
#[cfg(feature = "render")]
pub mod gpu;
/// The uniform handle to a spawned world.
pub mod handle;
/// Sub-worlds, which share the host's thread.
pub mod host;
/// Jobs, the work handed to another world.
pub mod job;
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
pub mod mcp;
#[cfg(feature = "physics")]
pub mod physics;
#[cfg(feature = "render")]
pub mod present;
// The recorder exists for the MCP audio tools, which are the only thing that
// constructs one, so it is compiled exactly where they are.
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
mod record;
mod sync;
#[cfg(feature = "synth")]
pub mod synth;
/// World threads and their pacing.
pub mod thread;
#[cfg(feature = "render")]
/// UI panels laid out in one world and drawn in another.
pub mod ui;
/// Remote worlds, which run on their own thread.
pub mod world;

/// The types most callers of the multi-world model need.
pub mod prelude {
    #[cfg(feature = "audio")]
    pub use crate::audio::{
        AudioChunk, AudioConfig, AudioFillOrder, AudioFormat, AudioGain, AudioOutputOpen,
        OnAudioFill, dispatch_audio_fill, spawn_audio_world_with,
    };
    #[cfg(feature = "render")]
    pub use crate::gpu::{Gpu, request_gpu};
    #[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
    pub use crate::mcp::serve_mcp;
    #[cfg(feature = "physics")]
    pub use crate::physics::{
        BodyDesc, BodyShape, PHYSICS_HZ, PhysicsConfig, PhysicsContacts, RigidBodyHandle,
        despawn_body, init_physics_world, spawn_body, spawn_fixed_cuboid, spawn_physics_world_with,
        step_once,
    };
    #[cfg(feature = "render")]
    pub use crate::present::{
        Presenter, bind_offscreen, draw_presented, pump_presented, resize_presented,
        setup_presenter,
    };
    #[cfg(feature = "synth")]
    pub use crate::synth::{
        MidiEvent, MidiFile, MidiMessage, MidiPlayer, SynthVoice, VoiceError, load_midi_file,
        load_soundfont, midi_file, note_off_job, note_on_job, pause_track_job, play_track_job,
        program_change_job, rewind_track_job, stop_track_job,
    };
    pub use crate::{
        channel::{SnapshotReader, publish_subscribed},
        executor::{LocalPool, LocalSpawnExt, LocalTasks, try_join_all},
        handle::WorldHandle,
        host::{drain_sub_world, spawn_sub_world},
        job::{Job, JobSender, Request, RequestError, SendError},
        thread::{FixedClock, ThreadControl, ThreadStatus, WorldThread},
        world::{
            Stepped, Threaded, WorldName, WorldRunner, spawn_remote_world,
            spawn_remote_world_with_init, spawn_stepped_world_with_init, step_stepped_world,
            with_stepped_world, with_stepped_world_mut,
        },
    };
}
