#![doc = include_str!("../README.md")]
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
        RigidBodyHandle, spawn_ball, spawn_fixed_cuboid, spawn_physics_world, step_once,
    };
    #[cfg(feature = "render")]
    pub use crate::render::{
        APPLY_TRANSFORMS_ORDER, OwnedUnlitMeshDesc, TransformFrame, apply_transform_frames,
        create_material, create_material_from_texture, create_mesh, spawn_instance,
    };
    #[cfg(feature = "synth")]
    pub use crate::synth::{
        SynthVoice, VoiceError, note_off_job, note_on_job, notes_off_all_job, synth_source,
        synth_source_with_gain,
    };
    pub use crate::{
        bridge::RenderEntity,
        channel::{SnapshotReader, SnapshotWriter, snapshot_pair},
        host::{SubWorld, SubWorldInbox, drain_sub_world, spawn_sub_world},
        job::{Job, Request},
        thread::{FixedClock, ThreadControl, WorldThread},
        world::{RemoteWorld, spawn_remote_world, spawn_remote_world_with},
    };
}
