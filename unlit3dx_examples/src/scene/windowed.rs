//! The windowed run's worlds: threads of their own, or stepped by the frame.
//!
//! A window is the one host that can give the scene both choices. The default
//! gives physics, audio and gameplay a thread each; a browser, a desktop run
//! asked for the stepped path, and the headless capture instead drive every
//! world from the frame loop that draws them. What differs is who calls a tick
//! and how it is paced; the entities and components are the same either way.
//!
//! This lives outside [`super::spawn_scene`] because it is the window's policy
//! rather than the scene's shape: which runner to take, and whether a stepped
//! run needs the stepper that drives it. The headless and interactive runs
//! answer both questions for themselves.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use unlit_ecs::{World, bundle};
use unlit3d::frame::OnFrame;
use unlit3d::input::InputWriter;
use unlit3d::winit::builtin::window;
use unlit3dx::prelude::*;

use super::{AudioSetup, PanelSlots, SceneHandles, SceneStepper, spawn_scene};

/// Build the scene for a windowed host, on the threads `stepped` asks for.
///
/// A stepped run also gets the behaviour that advances its worlds: without it
/// nothing would, since the worlds have no thread to pace them. It is spawned
/// here rather than by the caller so that a run cannot ask for the stepped
/// path and forget the stepper.
///
/// The balls drop on their own cadence: a windowed run has someone watching
/// the scene, so it needs no panel click to stay populated.
pub fn spawn_windowed_scene(
    host: &mut World,
    render: WorldHandle,
    panels: PanelSlots,
    input: &InputWriter,
    stepped: bool,
) -> SceneHandles {
    let worlds = if stepped {
        spawn_scene::<Stepped>(host, render, stepped_audio(), panels, input, true)
    } else {
        spawn_scene::<Threaded>(host, render, AudioSetup::Device, panels, input, true)
    };
    // The stepped worlds run before the draw, so the transforms they publish
    // are the ones this frame shows.
    if stepped {
        let stepper = SceneStepper::new(PHYSICS_HZ, None);
        host.root_scope_mut()
            .spawn(bundle![stepped_frame(worlds.clone(), stepper)]);
    }
    host.root_scope_mut().spawn(bundle![display_refresh_rate(
        worlds.refresh_millihertz.clone()
    )]);
    worlds
}

/// Watch the display the window is on and publish its refresh rate.
///
/// The rate is read every frame, so moving the window to another display or a
/// display changing mode is followed by the next frame. A display that reports
/// nothing leaves the rate at zero, which the gameplay world reads as "keep the
/// default".
fn display_refresh_rate(refresh_millihertz: Arc<AtomicU32>) -> OnFrame {
    OnFrame::new(move |world, _entity, _frame| {
        let rate = window(world)
            .and_then(|window| window.current_monitor())
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .unwrap_or(0);
        refresh_millihertz.store(rate, Ordering::Relaxed);
    })
}

/// Step the scene's worlds once per frame, before the draw.
///
/// A stepped run has no threads, so the host steps what a threaded build
/// would leave to them. The stepper follows the frame's own duration and
/// carries the step remainders, so the worlds keep their rates whatever the
/// display redraws at.
fn stepped_frame(worlds: SceneHandles, mut stepper: SceneStepper) -> OnFrame {
    OnFrame::new(move |world, _entity, frame| {
        stepper.step(world, &worlds, f64::from(frame.delta_time));
    })
}

/// The audio setup a windowed run takes when the host steps the worlds.
///
/// A browser's stepped world opens the page's Web Audio context once the
/// user's first click on Play asks for it: a context may only start inside a
/// gesture, so the scene stays silent until then even though the page has
/// been up the whole time. A desktop run that asked for the stepped path has
/// nothing to play to and stays silent.
fn stepped_audio() -> AudioSetup {
    if cfg!(target_arch = "wasm32") {
        AudioSetup::Device
    } else {
        AudioSetup::Silent
    }
}
