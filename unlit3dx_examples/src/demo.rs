//! The demo application: the scene the run drives.
//!
//! This is the [`App`] the example's binary, browser page and Android activity
//! all run. It owns the scene's soundfont and its audio policy — which run
//! opens a device and which one the host pulls — and it forwards the run's
//! two calls to the scene module. Everything else is the run's.

use std::sync::Arc;

use unlit_ecs::World;
use unlit3dx::app::{App, Mode, RenderPorts, RenderSetup, ScenePorts, SceneWorlds};
use unlit3dx::prelude::*;

use crate::scene::{self, AudioSetup, WINDOW_SIZE};

/// The demo's own command line, flattened into the run's parser.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct DemoArgs {
    /// Load the synthesizer's soundfont from this file instead of the
    /// embedded one.
    #[arg(long)]
    soundfont: Option<std::path::PathBuf>,
}

/// The multi-world demo: one scene, every run.
pub struct Demo {
    /// The soundfont the scene's synthesizer plays, read from the command
    /// line or embedded at build time.
    pub soundfont: Arc<[u8]>,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            soundfont: unlit3dx_asset_bytes::soundfont().into(),
        }
    }
}

impl App for Demo {
    type Args = DemoArgs;

    const TITLE: &'static str = "unlit3dx multi-world";
    const SIZE: (u32, u32) = WINDOW_SIZE;

    fn configure(&mut self, args: DemoArgs) -> Result<(), String> {
        if let Some(path) = args.soundfont {
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            self.soundfont = bytes.into();
        }
        Ok(())
    }

    fn spawn_scene<R: WorldRunner>(
        &mut self,
        host: &mut World,
        ports: ScenePorts<'_>,
    ) -> SceneWorlds {
        let audio_setup = match ports.mode {
            Mode::Windowed if R::SELF_DRIVEN => AudioSetup::Device,
            // A stepped windowed run has no output device on the desktop,
            // and a browser's audio context opens on the first Play click.
            Mode::Windowed => {
                if cfg!(target_arch = "wasm32") {
                    AudioSetup::Device
                } else {
                    AudioSetup::Silent
                }
            }
            Mode::Headless => {
                if self.soundfont.is_empty() {
                    AudioSetup::Silent
                } else {
                    AudioSetup::Pulled {
                        soundfont: self.soundfont.clone(),
                        autoplay_background: true,
                    }
                }
            }
            Mode::Interactive => {
                if self.soundfont.is_empty() {
                    AudioSetup::Silent
                } else {
                    AudioSetup::Pulled {
                        soundfont: self.soundfont.clone(),
                        autoplay_background: false,
                    }
                }
            }
        };
        scene::spawn_scene::<R>(
            host,
            ports.render.clone(),
            audio_setup,
            ports.panel.clone(),
            ports.refresh_millihertz.clone(),
            ports.input,
            ports.mode != Mode::Interactive,
        )
    }

    fn setup_render(&mut self, host: &mut World, ports: RenderPorts<'_>) -> RenderSetup {
        let (renderer, pass) =
            scene::setup_render_world(host, ports.world, ports.device.clone(), ports.panel);
        RenderSetup { renderer, pass }
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub mod mcp;

// A linker drops the inventory submissions of an object file nothing
// references, and the module holds nothing but its tools, so the submission
// lives here: every configuration of the demo references `Demo`.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
unlit_ecs::mcp::inventory::submit! {
    unlit_ecs::mcp::ToolSource { extend: mcp::register }
}
