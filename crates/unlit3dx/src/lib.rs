#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

#[cfg(feature = "audio")]
pub mod audio;
pub mod bridge;
pub mod channel;
#[cfg(feature = "render")]
pub mod gpu;
pub mod host;
pub mod job;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "physics")]
pub mod physics;
#[cfg(feature = "render")]
pub mod present;
#[cfg(feature = "render")]
pub mod render;
#[cfg(feature = "synth")]
pub mod synth;
pub mod thread;
pub mod world;

/// The types most callers of the multi-world model need.
pub mod prelude {
    #[cfg(feature = "audio")]
    pub use crate::audio::{
        AudioConfig, AudioPaused, AudioReady, AudioReadyInbox, PcmSource, PcmSourceFactory,
        pause_audio_job, spawn_audio_world,
    };
    #[cfg(feature = "render")]
    pub use crate::gpu::{Gpu, request_gpu, spawn as spawn_future};
    #[cfg(feature = "mcp")]
    pub use crate::mcp::serve_mcp;
    #[cfg(feature = "physics")]
    pub use crate::physics::{
        BodyDesc, BodyShape, PHYSICS_HZ, PhysicsBody, PhysicsConfig, PhysicsInbox, PhysicsSnapshot,
        RigidBodyHandle, spawn_body, spawn_fixed_cuboid, spawn_physics_world, step_once,
    };
    #[cfg(feature = "render")]
    pub use crate::present::{
        Presenter, bind_offscreen, draw_presented, pump_presented, resize_presented,
        setup_presenter,
    };
    #[cfg(feature = "render")]
    pub use crate::render::{
        APPLY_TRANSFORMS_ORDER, OwnedUnlitMeshDesc, TransformFrame, apply_transform_frames,
        camera_looking_at, create_material, create_material_from_texture, create_mesh, cuboid_mesh,
        linear_sampler, spawn_instance, uv_sphere,
    };
    #[cfg(feature = "synth")]
    pub use crate::synth::{
        MidiEvent, MidiFile, MidiPlayer, SynthVoice, VoiceError, load_midi_file, load_soundfont,
        midi_file, note_off_job, note_on_job, notes_off_all_job, play_midi_job, program_change_job,
        stop_audio_job, stop_midi_job, synth_source_bytes,
    };
    pub use crate::{
        bridge::RenderEntity,
        channel::{SnapshotReader, SnapshotWriter, snapshot_pair},
        host::{SubWorld, SubWorldInbox, drain_sub_world, spawn_sub_world},
        job::{Job, Request, poll_all},
        thread::{FixedClock, ThreadControl, WorldThread},
        world::{RemoteWorld, spawn_remote_world, spawn_remote_world_with},
    };
}
