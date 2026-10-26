//! The interactive headless host: MCP drives the same scene without a window.
//!
//! The host waits for commands rather than pacing itself. Only the frame-stepping
//! tool advances simulation time, so a client can reproduce a complete sequence
//! of input, rendering and recording without sleeping between commands.

use schemars::JsonSchema;
use serde::Deserialize;
use unlit_ecs::mcp::serde_json::{self, Value, json};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolResult, ToolRouter, ToolSource, WorldArg, command, tool,
};
use unlit_ecs::{Entity, World, bundle};
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::input::dispatch_input;
use unlit3d::prelude::*;
use unlit3d::winit::builtin::ExitRequest;
use unlit3dx::mcp::{pump_mcp, serve_mcp};
use unlit3dx::prelude::*;

use crate::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};
use crate::scene::{
    AudioSetup, Ball, HEADLESS_HZ, PanelLayout, PanelSlots, SceneHandles, SceneStepper,
    SceneTracks, WINDOW_SIZE, mount_scene_content, setup_render_world, spawn_scene,
};

/// Run the interactive headless host until MCP asks it to exit.
pub fn run(soundfont: std::sync::Arc<[u8]>) {
    let gpu = pollster::block_on(Gpu::headless()).expect("the headless run opens a GPU");
    let mut host = World::new();
    let input = InputWriter::new();
    host.root_scope_mut().spawn(bundle![
        input.clone(),
        input.subscribe(),
        ExitRequest(false),
    ]);
    let render = spawn_sub_world(&mut host, "render", 64);
    let panels = PanelSlots::default();
    let worlds = spawn_scene::<Stepped>(
        &mut host,
        render.clone(),
        AudioSetup::Pulled {
            soundfont,
            autoplay_background: false,
        },
        panels.clone(),
        &input,
        false,
    );
    let (renderer, pass) = setup_render_world(
        &mut host,
        render.entity,
        &gpu.device,
        &gpu.queue,
        gpu.capabilities,
    );
    mount_scene_content(&render, pass, Some(panels));
    let target = {
        let nested = host
            .root_scope()
            .query_one::<&mut World>(render.entity)
            .expect("the render sub-world exists");
        let context = nested
            .root_scope()
            .query_one::<&Renderer>(renderer.0)
            .expect("the renderer is a resource entity")
            .context();
        bind_offscreen(&nested, context, pass.0, &gpu.device, WINDOW_SIZE)
            .expect("the offscreen target binds")
    };
    host.root_scope_mut().spawn(bundle![Interactive {
        worlds,
        render: render.entity,
        renderer: renderer.0,
        target: target.view.id().to_bits(),
        stepper: SceneStepper::new(PHYSICS_HZ, Some(AUDIO_SAMPLE_RATE)),
        frame: Frame {
            delta_time: (1.0 / HEADLESS_HZ) as f32,
            index: 0,
            size: WINDOW_SIZE,
        },
        samples: Vec::new(),
    }]);
    serve_mcp(&mut host);
    log::info!("the interactive headless MCP server is ready");
    loop {
        pump_mcp(&mut host).expect("the MCP command queue applies");
        let exit = host
            .root_scope()
            .query::<&ExitRequest>()
            .any(|request| request.0);
        if exit {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
}

struct Interactive {
    worlds: SceneHandles,
    render: Entity,
    renderer: Entity,
    target: u64,
    stepper: SceneStepper,
    frame: Frame,
    samples: Vec<f32>,
}

impl Interactive {
    fn step(&mut self, host: &World, frames: u32) {
        for _ in 0..frames {
            dispatch_input(host);
            self.stepper.step(host, &self.worlds, 1.0 / HEADLESS_HZ);
            let frames = self.stepper.audio_frames(1.0 / HEADLESS_HZ);
            if frames > 0 {
                self.samples.clear();
                self.samples.resize(frames * AUDIO_CHANNELS, 0.0);
                let filled = with_stepped_world(host, self.worlds.audio.entity, |audio| {
                    dispatch_audio_fill(
                        audio,
                        &mut self.samples,
                        AUDIO_SAMPLE_RATE,
                        AUDIO_CHANNELS as u16,
                    );
                });
                assert!(filled.is_some(), "the audio world is a stepped world");
            }
            let mut nested = host
                .root_scope()
                .query_one::<&mut World>(self.render)
                .expect("the render sub-world exists");
            drain_sub_world(&mut nested);
            dispatch_frame(&mut nested, &mut self.frame);
            let _ = nested
                .root_scope()
                .query_one::<&mut Renderer>(self.renderer)
                .map(|mut renderer| renderer.render(&nested));
            self.frame.index = self.frame.index.wrapping_add(1);
        }
    }

    fn status(&self, host: &World) -> Value {
        let scene = with_stepped_world(host, self.worlds.gameplay.entity, |world| {
            let scope = world.root_scope();
            let balls = scope.query::<&Ball>().count();
            let sequence = scope.query::<&SceneTracks>().next().and_then(|tracks| {
                scope
                    .query_one::<&crate::scene::Track>(tracks.sequence)
                    .ok()
                    .map(|track| (track.midi.paused, track.midi.time, track.midi.finished))
            });
            let buttons = scope
                .query::<&PanelLayout>()
                .next()
                .map(|layout| {
                    layout
                        .buttons
                        .iter()
                        .map(|(label, rect)| {
                            (
                                (*label).to_owned(),
                                json!([rect.center().x, rect.center().y]),
                            )
                        })
                        .collect::<serde_json::Map<_, _>>()
                })
                .unwrap_or_default();
            (balls, sequence, buttons)
        });
        let Some((balls, sequence, buttons)) = scene else {
            return json!({ "ready": false });
        };
        json!({
            "ready": true,
            "frame": self.frame.index,
            "balls": balls,
            "sequence_paused": sequence.map(|state| state.0),
            "sequence_time": sequence.map(|state| state.1),
            "sequence_finished": sequence.map(|state| state.2),
            "buttons": buttons,
            "render_world": { "Handle": self.render.to_bits() },
            "audio_world": { "Handle": self.worlds.audio.entity.to_bits() },
            "target": self.target,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StepFrames {
    #[serde(default)]
    world: unlit_ecs::mcp::WorldId,
    /// How many fixed-length frames to advance.
    frames: u32,
}

unlit_ecs::mcp::inventory::submit! {
    ToolSource { extend: register }
}

fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool(
        "step_frames",
        "Advance the interactive headless scene by an exact number of fixed frames.",
        step_frames,
    ));
    router.add_route(tool(
        "scene_status",
        "Report the interactive scene's frame, balls, MIDI sequence, button centres and MCP world ids.",
        scene_status,
    ));
}

async fn step_frames(commands: CommandSender, args: StepFrames) -> ToolResult {
    command(&commands, args.world, move |host| {
        if args.frames == 0 {
            return Err("frames must be at least 1".to_owned());
        }
        let mut interactive = host
            .root_scope()
            .query::<&mut Interactive>()
            .next()
            .ok_or_else(|| "the host is not an interactive headless scene".to_owned())?;
        interactive.step(host, args.frames);
        Ok(json!({ "frame": interactive.frame.index }))
    })
    .await
}

async fn scene_status(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |host| {
        let interactive = host
            .root_scope()
            .query::<&Interactive>()
            .next()
            .ok_or_else(|| "the host is not an interactive headless scene".to_owned())?;
        Ok(interactive.status(host))
    })
    .await
}
